"""Shared prepare/claim/cutover/NEW-turn policy, using only public primitives.

Runtime callbacks own native execution. This owner alone claims and submits the
continuation. Neither a callback result nor a failed send is ever replayed.
"""
import copy
from accounts_cutover import member, text
from account_credentials import snapshot, credential_identity
import accounts_policy as policy
from vqro_ext import VqroError


class Dispatch:
    def __init__(self, owner):
        self.owner = owner
        self.ext = owner.ext
        self.journal = owner.cutovers

    @staticmethod
    def permitted(configured, target, automatic):
        return (configured['blocked'] == 'auto_restart' and configured['auto_cutover_max_per_hour'] > 0
                if automatic else configured['blocked'] in ('offer_restart', 'auto_restart')) \
            and (target['kind'] != 'api_key' or configured['api_key_overflow'])

    def plan(self, source, target, args):
        reply = self.ext.invoke('accounts.prepare.' + target['runtime'], args)
        if not isinstance(reply, dict) or not text(reply.get('setup_id')) or not text(reply.get('membership_revision')) \
                or not isinstance(reply.get('affected'), list) or not reply['affected']:
            raise VqroError('native prepared membership is unavailable', 'unavailable')
        plan = {key: copy.deepcopy(reply.get(key)) for key in
                ('setup_id', 'membership_revision', 'native_episode', 'automatic_allowed', 'affected', 'inference')}
        if not text(plan['native_episode']):
            raise VqroError('native limit/error episode witness is unavailable; never infer from text', 'unavailable')
        rows = {row.get('id'): row for row in policy.resources(self.ext)}
        plan['source_accounts']={item['resource']:policy.configured_id(rows.get(item['resource'],{})) for item in plan['affected']}
        plan['member_episodes']=[]
        if target['runtime']=='codex':
            for item in plan['affected']:
                row=rows.get(item['resource'],{});native=policy.facts(row)
                availability=native.get('availability') or {};witness=availability.get('native') or {}
                if policy.blocked(row,limit_only=True) and text(witness.get('turnId')) and text(policy.configured_id(row)):
                    plan['member_episodes'].append({'resource':row['id'],'host':row['host'],'incarnation':row['incarnation'],
                        'epoch':native['epoch'],'session':native['session']['id'],'account':policy.configured_id(row),
                        'native_episode':witness['turnId'],'episode':policy.episode(row)})
        for item in plan['affected']:
            member(item)
            row = rows.get(item['resource'], {})
            native = policy.facts(row)
            expected = {'resource': row.get('id'), 'host': row.get('host'), 'incarnation': row.get('incarnation'),
                        'session': (native.get('session') or {}).get('id'), 'epoch': native.get('epoch'), 'account': target['id']}
            if item != expected or row.get('available') is not True or native.get('lifecycle') != 'live':
                raise VqroError('prepared native membership changed', 'stale')
        if not any(item['resource'] == source['id'] for item in plan['affected']):
            raise VqroError('source is absent from native membership', 'stale')
        return plan

    @staticmethod
    def result_members(result, plan, target, allow_original=False):
        if not isinstance(result, dict) or not isinstance(result.get('affected'), list) or not result['affected']:
            return None
        runtime = target['runtime']
        allowed = ('accepted', 'observed') if runtime == 'claude' else ('observed',)
        if result.get('outcome') not in allowed:
            return None
        prepared = {item['resource']: item for item in plan['affected']}
        normalized = []
        for item in result['affected']:
            if not isinstance(item, dict) or not text(item.get('epoch')):
                return None
            session = item.get('requested_session', item.get('session'))
            if runtime == 'pi':
                matches = [old for old in prepared.values() if old['session'] == session]
                old = matches[0] if len(matches) == 1 else None
            else:
                old = prepared.get(item.get('resource'))
            if old is None or session != old['session'] or not text(item.get('resource')) or not text(item.get('host')):
                return None
            if runtime != 'pi' and item['host'] != old['host']:
                return None
            allowed_accounts={target['id']}
            if allow_original:allowed_accounts.add(plan['source_accounts'].get(old['resource']))
            if item.get('account') not in allowed_accounts:return None
            confirmed = item.get('session_confirmed') is True and item.get('resumed') is True and item.get('session') == old['session']
            exceptional = runtime == 'claude' and item.get('process_started') is True \
                and item.get('session_confirmed') is False and item.get('resumed') is False
            if not confirmed and not exceptional:
                return None
            normalized.append((old, copy.deepcopy(item), confirmed))
        if sorted(old['resource'] for old, _, _ in normalized) != sorted(prepared):
            return None
        return normalized

    def cancel_new_turns(self, claim, result):
        self.journal.cancel_continuations(claim)
        self.ext.notify('Native restart observed; automatic continuation cancelled by current policy. Waiting for your explicit next turn.', 'warn')
        return result

    def offer_siblings(self,parent_token,target,revision):
        parent=self.journal.lookup(parent_token)
        for episode in parent['member_episodes']:
            if episode['session']==parent['session']:continue
            destination=next((item for item in parent['resumed_members'] if item['session']==episode['session']),None)
            if destination is None:continue
            source={'id':episode['resource'],'host':episode['host'],'incarnation':episode['incarnation'],
                    'data':{'agent':{'v':0,'epoch':episode['epoch'],'account':{'id':episode['account']},
                                     'session':{'id':episode['session']}}}}
            original={key:episode[key] for key in ('resource','host','incarnation','epoch','session')}
            original['account']=target['id']
            plan={'setup_id':'continuation:'+parent_token+':'+episode['session'],
                  'membership_revision':parent['membership_revision'],'native_episode':episode['native_episode'],
                  'affected':[original],'authorized_targets':[target['id']]}
            try:
                child=self.journal.claim(source,target,revision,plan,True,parent['continuation_text'],continuation_parent=parent_token)
            except VqroError as error:
                if error.kind=='conflict':continue
                raise
            self.journal.native_result(child,observed=True,attempted_targets=[])
            self.journal.await_owner(child,[destination])
            self.owner.continuation_offer(child,target)

    def confirm_continuation(self,token):
        self.journal.authorization(False)
        record=self.journal.lookup(token)
        if record['state']!='awaiting_owner':raise VqroError('continuation offer already consumed','conflict')
        destinations=[item for item in record['resumed_members'] if item['session']==record['session']]
        if len(destinations)!=1:raise VqroError('original conversation destination is ambiguous','stale')
        item=destinations[0]
        rows=self.ext.vqro.call('resource.list',{'id':item['resource']}).get('resources',[])
        row=rows[0] if len(rows)==1 else {};native=policy.facts(row)
        if row.get('available') is not True or row.get('incarnation')!=item['incarnation'] or row.get('host')!=item['host'] \
                or native.get('epoch')!=item['epoch'] or native.get('lifecycle')!='live' or native.get('state')!='idle' \
                or (native.get('session') or {}).get('id')!=item['session'] or policy.configured_id(row)!=item['account']:
            raise VqroError('restored conversation changed; no continuation','stale')
        caps=native.get('caps') or {};send=caps.get('agent.send') or {}
        if type(caps.get('rev')) is not int or 'admit' not in (send.get('modes') or {}):
            raise VqroError('native attributed admission is unavailable','unsupported')
        self.journal.authorize_owner_continuation(token)
        original=next(old for old in record['affected'] if old['session']==record['session'])
        delivery=self.journal.claim_continuation(token,original,destination=item)
        wire={'v':0,'caps_rev':caps['rev'],'epoch':item['epoch'],'mode':'admit','delivery':delivery,
              'guard':{'session':item['session']},'content':[{'type':'text','text':record['continuation_text']}],
              'origin':{'kind':'tool','sender':'vqro.accounts (owner confirmed)'},'render':['attributed_user']}
        if not self.journal.continuation_allowed(token):raise VqroError('owner authority changed','forbidden')
        try:result=self.ext.vqro.call('resource.invoke',{'resource':item['resource'],'op':'agent.send','args':wire})
        except Exception:
            self.journal.continuation_result(token,original,observed=False);raise
        observed=isinstance(result,dict) and result.get('outcome') in ('observed','accepted','dispatched')
        self.journal.continuation_result(token,original,observed=observed)
        if not observed:raise VqroError('continuation submission unknown; never replay','indeterminate')
        self.journal.complete(token)
        return result

    def execute(self, source, target, revision, *, automatic, background_loss_disclosed=False, expected_native_episode=None):
        configured = policy.settings(self.ext)
        if automatic and configured['blocked'] == 'auto_restart' and configured['auto_cutover_max_per_hour'] == 0:
            raise VqroError('AUTO hourly budget is zero; offer instead', 'offer_required')
        if not self.permitted(configured, target, automatic):
            raise VqroError('policy does not permit this cutover; nothing restarted', 'forbidden')
        # Pi's explicit human-approved, labelled message-inference exception is
        # owned by its integration's versioned table, NEVER classified here.
        if background_loss_disclosed and (automatic or target['runtime'] != 'claude'):
            raise VqroError('background-loss disclosure is only a Claude owner OFFER action', 'forbidden')
        self.journal.authorization(automatic, background_loss_disclosed=background_loss_disclosed)  # Never trust raw caller flags.
        args = {'resource': source['id'], 'episode': policy.episode(source), 'account': target['id'],
                '_account_revision': revision, 'automatic': automatic}
        plan = self.plan(source, target, args)  # Busy here consumes NO durable intent.
        if expected_native_episode is not None and expected_native_episode != plan['native_episode']:
            raise VqroError('native error changed since the owner choice; reopen account options', 'stale')
        if automatic and plan['automatic_allowed'] is not True:
            raise VqroError('native setup has no automatic-cutover consent; offer instead', 'offer_required')
        native_plan=copy.deepcopy(plan)
        ordered=[target['id']]
        if automatic and target['runtime']=='codex':
            choices,current_revision,scores,_=self.owner.alternatives(source,configured)
            if current_revision!=revision:raise VqroError('target registry changed before reservation','stale')
            rest=[row for row in choices if row['id']!=target['id'] and row['runtime']=='codex' and row['kind']=='subscription'
                  and not self.journal.known_blocked(row)]
            rest.sort(key=lambda row:(scores.get(row['id']) is not None,scores.get(row['id']) or 0),reverse=True)
            ordered += [row['id'] for row in rest]
            remaining=self.journal.remaining('codex',plan['affected'])
            if remaining==0:raise VqroError('no shared-agent hourly allowance remains','offer_required')
            ordered=ordered[:min(remaining,10)]
        plan['authorized_targets']=ordered
        claim = self.journal.claim(source, target, revision, plan, automatic, configured['continuation_text'],
                                   background_loss_disclosed=background_loss_disclosed)
        invoked = False
        try:
            current = self.owner.source(source['id'])
            if policy.episode(current) != args['episode'] or policy.facts(current).get('session') != policy.facts(source).get('session'):
                raise VqroError('source changed after claim', 'stale')
            candidates, current_revision, scores, observations = self.owner.alternatives(current, policy.settings(self.ext))
            if current_revision != revision or target not in candidates:
                raise VqroError('target/registry changed after claim', 'stale')
            if automatic and self.journal.known_blocked(target):
                raise VqroError('target is now known blocked', 'stale')
            if self.plan(current, target, args) != native_plan:
                raise VqroError('prepared native setup changed after claim', 'stale')
            registry_rows, registry_revision = snapshot(self.ext)
            if registry_revision != revision or target not in registry_rows:
                raise VqroError('credential identities changed during final validation', 'stale')
            identities = {row['id']: credential_identity(row) for row in registry_rows}
            self.journal.validate(claim, current, plan['native_episode'])
            final = policy.settings(self.ext)
            if not self.permitted(final, target, automatic) or final['continuation_text'] != configured['continuation_text']:
                raise VqroError('policy changed after claim', 'stale')
            # Last local clock check follows ALL potentially blocking reads.
            if automatic:
                if self.journal.known_blocked(target):
                    raise VqroError('target became known blocked during final validation', 'stale')
            native_args = dict(args, _claim=claim)
            if target['runtime'] == 'codex':
                native_args['episode'] = plan['native_episode']  # Never equate this with the common hash.
            invoked = True
            result = self.ext.invoke('accounts.cutover.' + target['runtime'], native_args)
        except Exception:
            if invoked:
                self.journal.native_result(claim, observed=False)
            else:
                self.journal.reject_before_effect(claim)
            raise
        disposition=result.get('disposition','ready') if isinstance(result,dict) else None
        restored_only=isinstance(result,dict) and result.get('restored_only') is True
        nonready=disposition in ('offer','all_blocked') or restored_only
        if isinstance(result,dict) and 'disposition' in result and not restored_only:
            attempts=result.get('attempted_targets');observations=result.get('inspected_blocked')
            if not isinstance(attempts,list) or not isinstance(observations,list) or any(not isinstance(item,dict) for item in observations):
                disposition='invalid'
            elif [item.get('account') for item in observations] not in (
                    [attempts] if disposition=='all_blocked' else
                    [attempts[:-1],attempts] if disposition=='offer' else [attempts[:-1]]):
                # Mixed native quota can retain a scoped negative for the final
                # account while its aggregate verdict remains unknown/OFFER.
                # Preserve that evidence; it is not positive AUTO permission.
                disposition='invalid'
            elif disposition=='ready' and (result.get('continuation_allowed') is not True or result.get('fenced') is not False):
                disposition='invalid'
        if isinstance(result,dict) and result.get('continuation_allowed') is False and not nonready:
            disposition='invalid'
        registry_rows,latest_revision=snapshot(self.ext)
        selected_id=result.get('account',target['id']) if isinstance(result,dict) else None
        selected=next((row for row in registry_rows if row['id']==selected_id),None)
        selected_in_plan=selected is not None and selected_id in plan['authorized_targets'] and latest_revision==revision
        if selected_in_plan:target=selected
        elif nonready and latest_revision==revision and (selected_id is None or selected_id in plan['source_accounts'].values()):
            if selected is not None:target=selected
        else:disposition='invalid'
        normalized=self.result_members(result,plan,target,allow_original=restored_only or nonready and not selected_in_plan) if disposition in ('ready','offer','all_blocked') else None
        restored=[]
        if normalized is not None and (nonready or target['runtime']=='codex'):
            if nonready and (result.get('continuation_allowed') is not False or result.get('fenced') is not False):normalized=None
            else:
                for old,item,confirmed in normalized:
                    rows=self.ext.vqro.call('resource.list',{'id':item['resource']}).get('resources',[])
                    row=rows[0] if len(rows)==1 else {};native=policy.facts(row)
                    if not confirmed or row.get('available') is not True or native.get('lifecycle')!='live' \
                            or row.get('host')!=item['host'] or native.get('epoch')!=item['epoch'] \
                            or (native.get('session') or {}).get('id')!=old['session'] or policy.configured_id(row)!=item['account']:
                        normalized=None;break
                    restored.append({'resource':row['id'],'host':row['host'],'session':old['session'],
                        'epoch':native['epoch'],'incarnation':row['incarnation'],'account':item['account']})
        attempted=result.get('attempted_targets') if isinstance(result,dict) else None
        self.journal.native_result(claim, observed=normalized is not None,attempted_targets=attempted,
                                   selected_account=selected_id if normalized is not None and selected_in_plan else None,
                                   inspected_blocked=result.get('inspected_blocked',[]) if isinstance(result,dict) else [])
        if normalized is None:
            self.ext.notify('Account cutover outcome unconfirmed/partial. No continuation submitted; do not retry automatically.', 'warn')
            return result
        if target['runtime']=='codex':
            self.journal.record_restored(claim,restored)
            if disposition in ('ready','offer') and not restored_only:self.offer_siblings(claim,target,revision)
        if nonready:
            if disposition=='offer' and not restored_only:
                self.journal.await_owner(claim,restored)
                self.owner.continuation_offer(claim,target)
            else:
                self.journal.cancel_continuations(claim)
                self.ext.notify('All accounts are at their limit; conversations restored, no continuation sent.' if disposition=='all_blocked'
                                else 'Automatic switching was turned off during the switch; conversations restored, no continuation sent.','warn')
            return result
        if automatic:
            if not self.journal.continuation_allowed(claim):
                return self.cancel_new_turns(claim, result)
            # Validate ALL destinations before sending to ANY of a shared setup.
            destinations = []
            for old, item, confirmed in normalized:
                rows = self.ext.vqro.call('resource.list', {'id': item['resource']}).get('resources', [])
                if len(rows) != 1:
                    raise VqroError('resumed destination unavailable; no continuation submitted', 'stale')
                row = rows[0]; native = policy.facts(row)
                if row.get('host') != item['host'] or native.get('epoch') != item['epoch'] \
                        or policy.configured_id(row) != target['id'] or row.get('available') is not True \
                        or native.get('lifecycle') != 'live' or (confirmed and (native.get('session') or {}).get('id') != old['session']):
                    raise VqroError('resumed destination changed; no continuation submitted', 'stale')
                caps = native.get('caps') or {}; send = caps.get('agent.send') or {}; modes = send.get('modes') or {}
                mode = 'start' if 'start' in modes else 'admit' if target['runtime'] in ('codex', 'claude') and 'admit' in modes else None
                if not mode or type(caps.get('rev')) is not int:
                    raise VqroError('destination cannot admit a new attributed turn', 'unsupported')
                wire = {'v': 0, 'caps_rev': caps['rev'], 'epoch': item['epoch'], 'mode': mode,
                        'content': [{'type': 'text', 'text': configured['continuation_text']}],
                        'origin': {'kind': 'tool', 'sender': 'vqro.accounts' if confirmed else 'vqro.accounts (resume unconfirmed)'},
                        'render': ['attributed_user']}
                if confirmed: wire['guard'] = {'session': old['session']}
                destinations.append((old, item, wire, confirmed))
            if plan['inference'] is not None:
                label = plan['inference']['label']
                self.ext.notify(label, 'warn')
                for _, _, wire, _ in destinations:
                    wire['origin']['sender'] += ' (' + label + ')'
            submitted = set()
            for old, item, wire, confirmed in destinations:
                # Credential membership is not permission to prompt idle siblings.
                if old['session'] != policy.facts(source)['session']['id'] or old['session'] in submitted: continue
                if not self.journal.continuation_allowed(claim):
                    return self.cancel_new_turns(claim, result)
                try:
                    delivery = self.journal.claim_continuation(claim, old, destination=item)
                except VqroError as error:
                    if error.kind == 'policy_changed':
                        return self.cancel_new_turns(claim, result)
                    raise
                wire['delivery'] = delivery
                if not self.journal.continuation_allowed(claim):
                    return self.cancel_new_turns(claim, result)
                try:
                    sent = self.ext.vqro.call('resource.invoke', {'resource': item['resource'], 'op': 'agent.send', 'args': wire})
                except Exception:
                    self.journal.continuation_result(claim, old, observed=False)
                    raise
                acknowledged = isinstance(sent, dict) and sent.get('outcome') in ('observed', 'accepted', 'dispatched')
                self.journal.continuation_result(claim, old, observed=acknowledged)
                if not acknowledged:
                    raise VqroError('new continuation submission unconfirmed; never resend it', 'indeterminate')
                submitted.add(old['session'])
                if not confirmed:
                    self.ext.notify('Claude process started; same-conversation resume UNCONFIRMED. One NEW vqro continuation submitted, not replayed.', 'warn')
        self.journal.complete(claim)
        self.ext.notify('Agent restarted on ' + target['label'] + '; ' +
                        ('one NEW vqro/tool continuation submitted.' if automatic else 'waiting for your explicit next turn.'), 'info')
        return result
