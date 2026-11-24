#!/usr/bin/env python3
"""Replaceable account registry and native sign-in UI. Never opens a profile or credential store."""
import json
import hashlib
from pathlib import Path
import sys
import threading
import uuid
import accounts_policy as policy
from accounts_cutover import Cutovers
from accounts_claude import ClaudeCutover, eligible_episode as claude_episode
from accounts_pi import PiCutover, eligible_error as pi_error
from accounts_dispatch import Dispatch
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from account_credentials import SETTINGS, SELECTORS, MAX_ACCOUNTS, snapshot, validate, native_registration, migrate, environment, provider_argv, login_environment, native_login_argv, require_fresh, selection_problem, credential_identity, distinct_accounts, eligible_alternatives
from agent_launch import runtimes, valid_argv

COMMANDS = {'pi':'pi.new', 'codex':'codex.new', 'claude':'tier1.claude'}


class Accounts:
    def __init__(self, ext):
        self.ext = ext
        ext.retrying(lambda: migrate(ext))
        self.cutovers = Cutovers(ext)
        self.claude = ClaudeCutover(ext)
        self.pi = PiCutover(ext)
        self.dispatch = Dispatch(self)
        self.seen = {}
        self.pending = set()
        self.observations = {}
        self.overloaded = False
        self.warned_overflow = False
        self.pending_lock = threading.Lock()
        self.wake = threading.Event()
        for name, method, title in [('claim.read',self.read_claim,'Read acknowledged account cutover claim'),
                ('claim.resume.consume',self.consume_resume,'Consume acknowledged Pi resume launch'),
                ('claim.resume.result',self.resume_result,'Record Pi resume launch result'),
                ('policy',self.configure_policy,'Account capacity strategies'),
                ('offer',self.offer,'Continue this conversation on another account'), ('restart',self.restart,'Continue on another account'),
                ('restart.confirm',self.confirm_restart,'Confirm Claude background-loss warning'),
                ('continuation.confirm',self.confirm_continuation,'Continue after unreadable Codex usage'),
                ('prepare.claude',self.prepare_claude,'Prepare same-conversation Claude restart'),
                ('show',self.show,'Accounts'), ('add',self.add,'Add runtime account'),
                ('remove',self.remove,'Remove account metadata'), ('sign_in',self.sign_in,'Sign in with native runtime')]:
            ext.command('accounts.'+name,title)(method)
        ext.contribute({'view/accounts':{'kind':'panel','title':'Accounts','items':[]},
                        'view/accounts-restarts':{'kind':'panel','title':'Continue this conversation','items':[]},
                        'view/accounts-restart-warning':{'kind':'panel','title':'Claude background-loss warning','items':[]},
                        'view/accounts-continuation':{'kind':'panel','title':'Confirm continuation','items':[]},
                        'menu/accounts':{'where':'status','label':'accounts','command':'accounts.show','order':55}})
        ext.on_event(self.changed)

    def read_claim(self, args):
        """Read the acknowledged intent; no native effects or independent claims."""
        return self.cutovers.lookup(args.get('claim'))

    def consume_resume(self,args):
        if not isinstance(args,dict) or set(args)!={'claim'}:raise VqroError('only the claim reference is accepted','invalid')
        return self.cutovers.consume_resume(args['claim'])

    def resume_result(self,args):
        if not isinstance(args,dict) or set(args)!={'claim','observed'} or type(args['observed']) is not bool:
            raise VqroError('invalid resume result','invalid')
        self.cutovers.resume_result(args['claim'],args['observed'])
        return {'recorded':True}

    def show(self, args):
        rows, revision = snapshot(self.ext)
        primary = {credential_identity(row): row['id'] for row in distinct_accounts(rows)}
        items = [{'type':'text','text':'One shared runtime setup. Accounts select credentials only; Pi owns native provider-alias login and refresh in one shared auth store. vqro never opens credential files.'}]
        entry=self.ext.vqro.call('workspace.get',{'key':policy.POLICY})
        try: configured=policy.validate_settings(entry['value'] if entry else {})
        except VqroError:
            configured=dict(policy.DEFAULTS)
            items.append({'type':'text','style':'warn','text':'Unrecognized account policy: using ask + notify, no API-key overflow. Saving replaces the invalid policy.'})
        for field,title,choices in [('new_agents','New agents',['ask','most_remaining_capacity','round_robin']),
                ('blocked','When quota/rate limited',['notify','offer_restart','auto_restart']),
                ('api_key_overflow','Allow automatic API-key accounts',[False,True]),
                ('auto_cutover_max_per_hour','Automatic cutovers per agent per rolling hour (0 = offer)',list(range(11))) ]:
            items.append({'type':'select','text':title,'value':configured[field], 'command':'accounts.policy',
                          'args':{'field':field,'expect':entry['version'] if entry else 0},
                          'options':[{'label':str(value).replace('_',' '),'value':value} for value in choices]})
        items.append({'type':'text','style':'dim','text':'Capacity = fresh reported percentage headroom, not entitlement. Automatic cutover restarts on the other account; the same conversation continues with one NEW vqro-labelled turn, never earlier input. API-key overflow is off by default.'})
        items.append({'type':'text','style':'dim','text':label(configured['continuation_text'])})
        items.append({'text':'Edit automatic continuation text','input':'Literal new-turn text (4096 UTF-8 bytes maximum)',
                      'command':'accounts.policy','args':{'field':'continuation_text','expect':entry['version'] if entry else 0}})
        with self.ext.cache_lock:
            blocked_ids=[rid for rid,row in self.ext.resources.items() if policy.blocked(row,limit_only=True)][:64]
        items.extend({'text':'Fresh-work options for '+label(rid),'command':'accounts.offer','args':{'resource':rid}} for rid in blocked_ids)
        for runtime in SELECTORS:
            items.append({'type':'text','style':'bold','text':runtime})
            items.append({'text':'Shared runtime login', 'command':COMMANDS[runtime], 'args':{'account':'normal'}})
            items.append({'text':'Sign in… (shared '+runtime+' login)', 'command':'accounts.sign_in',
                          'args':{'account':'normal','runtime':runtime}})
            for row in rows:
                if row['runtime'] != runtime: continue
                items.append({'type':'text','text':label(row['label']+' · '+row.get('provider',runtime)+' · '+row['kind'])})
                problem = selection_problem(row)
                if problem:
                    items.append({'type':'text','style':'warn','text':'Invalid / not selectable: '+problem})
                else:
                    canonical = primary[credential_identity(row)]
                    if canonical != row['id']:
                        items.append({'type':'text','style':'dim','text':'Alias of '+canonical+'; same credential reference, never an independent alternative.'})
                    items.append({'text':'New agent on '+label(row['label']), 'command':COMMANDS[runtime],
                                  'args':{'account':row['id'],'_account_revision':revision}})
                    items.append({'text':'Sign in…', 'description':'Pi: use /login in the native terminal' if runtime=='pi' else 'Native browser/device flow',
                                  'command':'accounts.sign_in', 'args':{'account':row['id'],'_account_revision':revision}})
                items.append({'text':'Remove account metadata', 'command':'accounts.remove',
                              'args':{'account':row['id'],'expect':revision}, 'style':'dim'})
        items.append({'text':'Add account…', 'input':'JSON {id,label,runtime,provider?,kind,native_provider?|login_ref?|secret_ref?,host?}; opaque references, never tokens',
                      'command':'accounts.add'})
        self.ext.publish('accounts',{'title':'Accounts','requires':['menu','input'],'items':items})
        return {'client':{'popup':{'view':'accounts','width':90}}}

    def add(self, args):
        try: row = json.loads(args['text']) if 'text' in args else args
        except (ValueError,TypeError): raise VqroError('enter a metadata-only account JSON object','invalid') from None
        account = validate(native_registration(validate(row)))
        rows, revision = snapshot(self.ext)
        if len(rows) >= MAX_ACCOUNTS: raise VqroError('accounts registry is full','overloaded')
        if any(r['id']==account['id'] for r in rows): raise VqroError('account id already exists','conflict')
        self.ext.apply([{'op':'set','key':SETTINGS,'expect':revision,'value':{'accounts':rows+[account]}}], 'accounts: add metadata')
        self.ext.notify('Account metadata saved; use Sign in… to open the runtime. No credential file was read.','ok')
        return {'client':{'close':True}}

    def remove(self, args):
        rows, revision = snapshot(self.ext)
        if type(args.get('expect')) is not int or args['expect'] != revision:
            raise VqroError('account registry changed; reopen Accounts before removing metadata','stale')
        remaining = [r for r in rows if r['id'] != args.get('account')]
        if len(remaining)==len(rows): raise VqroError('account metadata not found','not_found')
        self.ext.apply([{'op':'set','key':SETTINGS,'expect':revision,'value':{'accounts':remaining}}], 'accounts: remove metadata only')
        self.ext.notify('Account metadata removed. Native credential files/logins and running agents are unchanged.','ok')
        return {'client':{'close':True}}

    def native_argv(self, runtime, launcher=None):
        key = {'claude':'doc/settings/tier1', 'pi':'doc/settings/pi', 'codex':'doc/settings/codex'}[runtime]
        entry = self.ext.vqro.call('workspace.get',{'key':key})
        value = entry['value'] if entry else None
        env = {}
        if runtime=='claude':
            argv = value.get('claude') if isinstance(value,dict) else ['claude']
            if not valid_argv(argv): raise VqroError('Claude runtime argv is invalid','invalid')
        else:
            default = [{'name':runtime,'argv':['pi1','--mode','rpc'] if runtime=='pi' else ['codex','app-server','--stdio']}]
            options = runtimes(value if value is not None else {'runtimes':default})
            selected = next((r for r in options if r['name']==launcher), None) if launcher else options[0]
            if selected is None: raise VqroError('native sign-in runtime not found','invalid')
            argv = selected['argv']
            env = login_environment(runtime, selected)
        return native_login_argv(runtime, argv), env

    def sign_in(self, args):
        rows, revision = snapshot(self.ext)
        if '_account_revision' in args and (type(args['_account_revision']) is not int or args['_account_revision'] != revision):
            raise VqroError('account registry changed; reopen Accounts before signing in','stale')
        account = None
        if args.get('account')=='normal': runtime=args.get('runtime')
        else:
            account=next((r for r in rows if r['id']==args.get('account')),None)
            if account is None: raise VqroError('account metadata not found','not_found')
            problem = selection_problem(account)
            if problem: raise VqroError('account is not selectable: '+problem, 'invalid')
            runtime=account['runtime']
            if account['kind']=='api_key':
                raise VqroError('API-key import is not implemented; configure keys only through the native runtime, never Accounts','unsupported')
        if runtime not in SELECTORS: raise VqroError('choose a native program','invalid')
        native, env = self.native_argv(runtime,args.get('launcher'))
        argv = provider_argv(native,account)
        if runtime == 'pi' and account:
            if not account.get('native_provider') or not account.get('provider'):
                raise VqroError('Pi account needs an explicit base provider and native alias before sign-in', 'unavailable')
            metadata = {key:account[key] for key in ('id','label','provider','native_provider')}
            argv += ['-e', str(Path(__file__).resolve().parent / 'pi_accounts' / 'index.mjs'),
                     '--vqro-account', json.dumps(metadata,ensure_ascii=False,separators=(',',':')), '--vqro-account-login']
        params={'argv':argv,'name':runtime+'-sign-in-'+uuid.uuid4().hex[:12], 'env':env}
        # No terminal input, shell, auth inspection or retry. A native runtime may open its own browser.
        result=self.ext.vqro.call('resource.spawn',params)
        host=(result.get('resource') or {}).get('id') if isinstance(result,dict) else None
        if not isinstance(host,str): raise VqroError('sign-in terminal outcome unknown; do not replay','indeterminate')
        shown={}
        try: shown=self.ext.invoke('tabs.show',{'resource':host}) or {}
        except VqroError: self.ext.notify('Sign-in terminal '+host+' exists; open it from the resource picker. Do not repeat the launch.','warn')
        self.ext.notify('Native sign-in terminal opened'+('; enter /login in Pi' if runtime=='pi' else '')+'. vqro does not inspect login success.','info')
        return {'outcome':result.get('outcome'),'resource':host,'client':shown.get('client',{})}


    def configure_policy(self,args):
        entry=self.ext.vqro.call('workspace.get',{'key':policy.POLICY})
        revision=entry['version'] if entry else 0
        if type(args.get('expect')) is not int or args['expect']!=revision:
            raise VqroError('account policy changed; reopen Accounts','stale')
        if not isinstance(args.get('field'),str) or args['field'] not in policy.DEFAULTS:
            raise VqroError('unknown account policy field','invalid')
        selected=args.get('text',args.get('value')) if args['field']=='continuation_text' else args.get('value')
        value=policy.validate_settings(dict(policy.effective(entry['value'] if entry else {}), **{args['field']:selected}))
        self.ext.apply([{'op':'set','key':policy.POLICY,'expect':revision,'value':value}], 'accounts: capacity policy')
        return self.show({})

    @staticmethod
    def actionable(row):
        return policy.blocked(row,limit_only=True) or claude_episode(row,automatic=False) is not None or bool(pi_error(row))

    def source(self,rid):
        rows=self.ext.vqro.call('resource.list',{'id':rid}).get('resources',[])
        if len(rows)!=1 or not self.actionable(rows[0]):
            raise VqroError('source has no current eligible native limit/error episode','stale')
        policy.episode(rows[0])
        return rows[0]

    def continuation_offer(self,claim,target):
        self.ext.publish('accounts-continuation',{'title':'Continue on '+label(target['label']),'requires':['menu'],'items':[
            {'type':'text','text':'Switched to account '+label(target['label'])+'; its usage could not be read. Continue?'},
            {'type':'text','text':'One NEW visible vqro/tool turn on the original conversation. No native switch or earlier input replay.'},
            {'text':'Continue','command':'accounts.continuation.confirm','args':{'claim':claim}}]})
        self.ext.notify('Switched to account '+label(target['label'])+'; usage could not be read. Owner continuation choice available.','warn')

    def confirm_continuation(self,args):
        if not isinstance(args,dict) or set(args)!={'claim'}:raise VqroError('only the same claim reference is accepted','invalid')
        return self.dispatch.confirm_continuation(args['claim'])

    def prepare_claude(self,args):
        if type(args.get('automatic')) is not bool: raise VqroError('automatic must be boolean','invalid')
        source,host=self.claude.source(args.get('resource'),args.get('episode'),False)
        if args['automatic'] and claude_episode(source,automatic=True) is None:
            raise VqroError('native background work is not proven empty; owner warning required','offer_required')
        native=policy.facts(source)
        self.claude.target(args,policy.configured_id(source),host)
        affected={'resource':source['id'],'host':host,'session':native['session']['id'],
                  'epoch':native['epoch'],'incarnation':source['incarnation'],'account':args['account']}
        revision=hashlib.sha256(json.dumps([affected,native.get('_claude'),native.get('background'),
                  native.get('incomplete')],sort_keys=True).encode()).hexdigest()
        return {'setup_id':'claude:'+source['id'],'membership_revision':revision,'affected':[affected],
                'native_episode':claude_episode(source,automatic=args['automatic']),'automatic_allowed':True}

    def alternatives(self,source,configured):
        rows,revision=snapshot(self.ext)
        observed=policy.resources(self.ext)
        self.cutovers.observe_limits(observed,rows)
        blocked_ids=self.cutovers.blocked_accounts(rows)
        source_row=next((row for row in rows if row['id']==policy.configured_id(source)),None)
        runtime=source_row['runtime'] if source_row else ('claude' if claude_episode(source,automatic=False) else 'pi' if pi_error(source) else None)
        # API-key keychain helpers remain; named launch binding and same-conversation cutover are unavailable.
        candidates=[row for row in eligible_alternatives(rows, policy.configured_id(source), blocked_ids, False)
                    if runtime is not None and row['runtime']==runtime]
        identities={row['id']:credential_identity(row) for row in rows}
        return candidates,revision,policy.capacities(observed,candidates,credential_identities=identities),observed

    def notify_once(self,source,mode,text):
        key=(source['id'],policy.episode(source),mode)
        if key in self.seen: return
        if len(self.seen)>=256: self.seen.pop(next(iter(self.seen)))
        self.seen[key]=True
        self.ext.notify(text,'warn')

    def offer(self,args):
        source=self.source(args.get('resource'))
        candidates,revision,scores,_=self.alternatives(source,policy.settings(self.ext))
        token=policy.episode(source)
        items=[{'type':'text','text':'Continue the same native conversation. Earlier input is never replayed. AUTO submits one NEW visible vqro/tool continuation; an owner choice waits for your explicit next turn.'},
               {'type':'text','text':'Continuing a conversation on an API-key account: not yet. API-key accounts: launching is not available yet; the keychain entry is kept.'}]
        for target in candidates:
            score=scores.get(target['id'])
            items.append({'text':'Continue on '+label(target['label']),
                          'description':('Reported headroom '+str(round(score,1))+'%' if score is not None else 'Capacity unknown/incomparable')+'; native same-conversation restart',
                          'command':'accounts.restart','args':{'resource':source['id'],'episode':token,
                                                               'account':target['id'],'_account_revision':revision}})
        if not candidates: items.append({'type':'text','text':'No configured alternative allowed by this policy. Native login and API-key setup are separate, explicit actions.'})
        self.ext.publish('accounts-restarts',{'title':'Continue this conversation','requires':['menu'],'items':items})
        return {'client':{'popup':{'view':'accounts-restarts','width':90}}}

    def restart(self,args):
        source=self.source(args.get('resource'))
        self.cutovers.authorization(False)
        if args.get('episode')!=policy.episode(source): raise VqroError('native episode changed; reopen continuation options','stale')
        candidates,revision,_,_=self.alternatives(source,policy.settings(self.ext))
        if type(args.get('_account_revision')) is not int or args['_account_revision']!=revision:
            raise VqroError('account registry changed; reopen continuation options','stale')
        target=next((row for row in candidates if row['id']==args.get('account')),None)
        if target is None: raise VqroError('alternative is no longer allowed','stale')
        if target['runtime']=='claude':
            authority=self.cutovers.authorization(False)
            native=claude_episode(source,automatic=False)
            wire={key:args[key] for key in ('resource','episode','account','_account_revision')}
            wire.update(warning_version=1,native_episode=native,owner_revision=authority['owner_revision'])
            self.ext.publish('accounts-restart-warning',{'title':'Confirm Claude restart','requires':['menu'],'items':[
                {'type':'text','text':'Background work may be lost when restarting Claude. Earlier input is never replayed.','style':'warn'},
                {'text':'I understand: continue on '+label(target['label']),'command':'accounts.restart.confirm','args':wire}]})
            return {'client':{'popup':{'view':'accounts-restart-warning','width':90}}}
        return self.dispatch.execute(source,target,revision,automatic=False)

    def confirm_restart(self,args):
        authority=self.cutovers.authorization(False)
        if type(args.get('warning_version')) is not int or args['warning_version']!=1 \
                or type(args.get('owner_revision')) is not int or args['owner_revision']!=authority['owner_revision']:
            raise VqroError('owner warning changed; reopen continuation options','stale')
        source=self.source(args.get('resource'))
        if args.get('episode')!=policy.episode(source): raise VqroError('native episode changed','stale')
        candidates,revision,_,_=self.alternatives(source,policy.settings(self.ext))
        target=next((row for row in candidates if row['id']==args.get('account') and row['runtime']=='claude'),None)
        if target is None or type(args.get('_account_revision')) is not int or revision!=args['_account_revision'] \
                or args.get('native_episode')!=claude_episode(source,automatic=False):
            raise VqroError('warning no longer names this native episode/account','stale')
        return self.dispatch.execute(source,target,revision,automatic=False,background_loss_disclosed=True,
                                     expected_native_episode=args['native_episode'])

    def handle_blocked(self,rid):
        source=None
        try:
            source=self.source(rid);configured=policy.settings(self.ext);mode=configured['blocked']
            message='Agent '+label(rid)+' reported a native limit/error episode. Earlier input is never replayed.'
            if mode=='notify': return self.notify_once(source,mode,message)
            if mode=='offer_restart':
                self.offer({'resource':rid})
                return self.notify_once(source,mode,message+' Open Accounts to continue on another account.')
            candidates,revision,scores,_=self.alternatives(source,configured)
            if not candidates or configured['auto_cutover_max_per_hour']==0:
                self.offer({'resource':rid})
                return self.notify_once(source,mode,message+' No eligible AUTO target or hourly allowance; owner choice available.')
            target=max(candidates,key=lambda row:(scores.get(row['id']) is not None,scores.get(row['id']) or 0))
            self.dispatch.execute(source,target,revision,automatic=True)
        except VqroError as error:
            # Busy prepare owns no intent; native work-level changes schedule reprepare.
            # Never replay dispatched restarts or turns, even after uncertain outcomes.
            if source is not None and error.kind=='offer_required': self.offer({'resource':rid})
            text='Account continuation took no further action ('+str(error.kind)+'); inspect Accounts/native status.'
            if source is not None: self.notify_once(source,'error:'+str(error.kind),text)
            elif error.kind!='stale': self.ext.notify(text,'warn')

    def changed(self,kind,data):
        if kind not in ('entries','resource','resource.removed','missed'): return
        # Listener only schedules bounded work; no RPC/native launch on this thread.
        with self.ext.cache_lock:
            if kind=='resource.removed':
                self.observations.pop(data.get('id'),None)
                return  # absence is not native quota recovery; never clear protected claims/child guards
            if kind=='entries' and not any(change.get('key') in (policy.POLICY,SETTINGS,'doc/settings/pi','doc/settings/codex','doc/settings/tier1')
                                               for change in data.get('changes',[])): return
            if kind=='resource':
                rid=data.get('id');row=self.ext.resources.get(rid,{})
                native=policy.facts(row);availability=native.get('availability')
                availability=availability if isinstance(availability,dict) else {}
                limits=native.get('usage_limits');caps=native.get('caps')
                declared=caps.get('agent.availability') if isinstance(caps,dict) else None
                signature=[row.get('incarnation'),native.get('epoch'),row.get('available'),native.get('lifecycle'),
                           policy.configured_id(row),availability.get('state'),availability.get('reason'),
                           limits,native.get('incomplete'),declared,native.get('state'),native.get('queue'),
                           native.get('background'),native.get('_claude'),native.get('session'),native.get('completion'),
                           availability.get('native')]
                if self.observations.get(rid)==signature: return
                if len(self.observations)>=256: self.observations.pop(next(iter(self.observations)))
                self.observations[rid]=signature
                cached=self.ext.value(policy.POLICY,{})
                if not (isinstance(cached,dict) and cached.get('blocked')=='auto_restart' and limits):
                    ids=[rid] if self.actionable(row) else []
                else: ids=[key for key,value in self.ext.resources.items() if self.actionable(value)]
            else: ids=[key for key,value in self.ext.resources.items() if self.actionable(value)]
        with self.pending_lock:
            fresh=[rid for rid in ids if rid not in self.pending]
            room=64-len(self.pending)
            self.overloaded=self.overloaded or len(fresh)>room
            self.pending.update(fresh[:room])
        self.wake.set()

    @staticmethod
    def worker_error(text):
        try: print(text,file=sys.stderr,flush=True)
        except Exception: pass  # reporting must not terminate the sole daemon

    def worker(self):
        while True:
            self.wake.wait();self.wake.clear()
            if self.overloaded and not self.warned_overflow:
                self.warned_overflow=True
                try: self.ext.notify('Account limit-policy queue is full; inspect Agent status. Excess work was not automatically launched.','warn')
                except Exception: self.worker_error('accounts: overflow notification failed; worker continues without retry')
            while True:
                with self.pending_lock:
                    if not self.pending: break
                    rid=self.pending.pop()
                try: self.handle_blocked(rid)
                except Exception:
                    # Keep the worker alive, but never retry an unexpected failure's popped action.
                    self.worker_error('accounts: fresh-work handling failed; no automatic retry')


def main():
    ext=Extension('accounts',event_types=['entries','resource','resource.removed'])
    accounts=Accounts(ext)
    threading.Thread(target=accounts.worker,daemon=True,name='accounts-capacity').start()
    accounts.changed('missed',{})
    ext.run()

if __name__=='__main__': main()
