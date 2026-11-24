"""Shared Codex saved-login cutover. No credential/profile reads, claims or prompt replay.
Common accounts orchestration alone owns durable claims and continuation turns.
"""
import hashlib
import json
import time
import threading
from account_credentials import snapshot, fact, selection_problem, credential_identity, eligible_alternatives
from vqro_ext import VqroError

SETTINGS = 'doc/settings/codex-cutover'
MAX_MEMBERS = 200
HOST_COMMANDS = {'_account.inspect','_account.fence','_account.status','_account.switch','_account.resume','_account.restore','_account.release','_account.abort','_account.list'}


class _NativeRejected(VqroError):
    """An explicit provider no-effect rejection, not an uncertain RPC failure."""


def fail(message, kind='unavailable'):
    raise VqroError(message, kind)


def text(value, limit=256):
    return isinstance(value, str) and 0 < len(value.encode()) <= limit and value.isprintable()


def native(row):
    return (row.get('data') or {}).get('agent') or {}


def evidence_complete(facts, required):
    # Same producer-marker boundary as Claude; absence is legacy no cuts, not
    # permission to ignore malformed markers or required parent/child cuts.
    markers = facts.get('incomplete', [])
    if not isinstance(markers,list) or any(not isinstance(mark,str) or not mark or not mark.isprintable() for mark in markers):
        return False
    return not any(mark==path or mark.startswith((path+'.',path+'['))
                   or path.startswith((mark+'.',mark+'[')) for mark in markers for path in required)


class Cutover:
    def __init__(self, ext):
        self.ext = ext
        # The same extension owns launches: do not admit a new managed host while a
        # shared-login effect is between its membership check and final resume.
        self.admission = threading.RLock()
        ext.command('accounts.prepare.codex', 'Prepare shared Codex account switch')(self.prepare)
        ext.command('accounts.cutover.codex', 'Continue conversations on another Codex account')(self.cutover)
        ext.command('codex.cutover.settings', 'Codex automatic account switching')(self.settings)
        ext.command('codex.cutover.consent', 'Set shared Codex login-switch consent')(self.consent)

    def settings(self, args):
        from vqro_display import label
        runtime = self.ext.vqro.call('workspace.get', {'key': 'doc/settings/codex'})
        rows = (runtime or {}).get('value', {}).get('runtimes', [])
        setups = sorted({r['setup_id'] for r in rows if isinstance(r,dict) and text(r.get('setup_id'),64)})
        entry = self.ext.vqro.call('workspace.get', {'key': SETTINGS})
        value = (entry or {}).get('value', {})
        items = [{'type':'text','text':"Switches automatically by restarting the agent on the other account; the conversation continues. All managed Codex agents sharing the setup move together. This also affects Codex CLI/IDE outside vqro; external clients cannot be fenced."},
                 {'type':'text','text':'Without consent, limit policy falls back to notify/offer. No failed or earlier input is replayed.'}]
        for setup in setups:
            enabled = ((value.get('setups') or {}).get(setup) or {}).get('automatic_login_switch') is True
            items.append({'text':label(setup + (': automatic login switching on' if enabled else ': automatic login switching off')),
                'description':"Let vqro switch this Codex setup's login automatically (also affects Codex used outside vqro)",
                'command':'codex.cutover.consent','args':{'setup_id':setup,'allow':not enabled,'accept_shared_login_effects':True}})
        if not setups: items.append({'type':'text','text':'Set a stable setup_id in Codex runtime settings and restart existing agents to declare shared setup membership.'})
        self.ext.publish('codex-cutover-settings', {'title':'Codex shared-login consent','requires':['menu'],'items':items})
        return {'client':{'popup':{'view':'codex-cutover-settings','width':90}}}

    def consent(self, args):
        setup = args.get('setup_id')
        if not text(setup,64) or type(args.get('allow')) is not bool or args.get('accept_shared_login_effects') is not True:
            fail('explicit shared-login consent is required','invalid')
        entry = self.ext.vqro.call('workspace.get', {'key':SETTINGS})
        value = (entry or {}).get('value', {'setups':{}})
        if not isinstance(value,dict) or not isinstance(value.get('setups',{}),dict): fail('Codex consent settings malformed','invalid')
        setups = dict(value.get('setups',{}));setups[setup]={'automatic_login_switch':args['allow']}
        if len(setups)>64: fail('too many Codex shared setups','overloaded')
        self.ext.apply([{'op':'set','key':SETTINGS,'expect':entry['version'] if entry else 0,'value':{'setups':setups}}], 'codex: explicit shared-login consent')
        return self.settings({})

    def _automatic_allowed(self, setup):
        entry = self.ext.vqro.call('workspace.get',{'key':SETTINGS})
        value = entry.get('value',{}) if isinstance(entry,dict) else {}
        setups = value.get('setups',{}) if isinstance(value,dict) else {}
        setting = setups.get(setup,{}) if isinstance(setups,dict) else {}
        return isinstance(setting,dict) and setting.get('automatic_login_switch') is True

    def resources(self):
        rows = self.ext.vqro.call('resource.list', {}).get('resources', [])
        if not isinstance(rows, list): fail('Codex membership unavailable')
        return rows

    def prepare(self, args):
        return self._prepare(args, require_idle=True)

    def _prepare(self, args, require_idle=False):
        if not isinstance(args,dict) or not text(args.get('resource')) or not text(args.get('account'),64) \
                or not text(args.get('episode')) or type(args.get('automatic')) is not bool \
                or type(args.get('_account_revision')) is not int or args['_account_revision']<0:
            fail('Codex prepare/effect requires typed source, episode, target, registry revision and automatic mode','invalid')
        rows = self.resources()
        source = next((r for r in rows if r.get('id') == args.get('resource')), None)
        if not source: fail('Codex source disappeared', 'stale')
        a = native(source)
        setup = (a.get('account_setup') or {}).get('id')
        if not text(setup, 64): fail('Codex shared setup is undeclared; restart it through the configured launcher')
        if (source.get('status') or {}).get('app') != 'codex': fail('source is not Codex', 'invalid')
        if source.get('available') is not True or type(a.get('v')) is not int or a['v']!=0:
            fail('source Codex observation is unavailable or unsupported','stale')
        if require_idle:
            from accounts_policy import episode
            if args['episode']!=episode(source):fail('common source episode changed before native prepare','stale')
        session = (a.get('session') or {}).get('id')
        availability = a.get('availability') or {}
        witness = availability.get('native') or {}
        info = witness.get('codexErrorInfo')
        declared = ((a.get('caps') or {}).get('agent.availability') or {}).get('reasons',[])
        if availability.get('state') != 'blocked' or availability.get('reason') not in ('quota', 'rate_limited') \
                or availability.get('reason') not in declared \
                or info != {'quota':'usageLimitExceeded','rate_limited':'rateLimitExceeded'}.get(availability.get('reason')) or witness.get('willRetry') is not False \
                or witness.get('threadId') != session or not text(witness.get('turnId'), 128):
            fail('native subscription-limit episode is unavailable', 'stale')
        target_rows, revision = snapshot(self.ext)
        if args.get('_account_revision') != revision: fail('account registry changed', 'stale')
        target = next((r for r in target_rows if r['id'] == args.get('account')), None)
        if not target or target['runtime'] != 'codex' or target['kind'] != 'subscription' or not target.get('login_ref'):
            fail('target needs a native Codex saved-login/workspace reference', 'unsupported')
        from accounts_policy import configured_id
        # Common's protected live projection owns reset/hour-bound block policy.
        # Raw persistent availability cannot override its expiry decision here.
        eligible = eligible_alternatives(target_rows, configured_id(source),set(),allow_api_key=False)
        if selection_problem(target) or target not in eligible:
            fail('target route is incomplete, blocked, duplicated or aliases the source account','stale')
        if args.get('automatic') is True:
            registered = {r['id']:r for r in target_rows if r['runtime']=='codex' and r['kind']=='subscription' and not selection_problem(r)}
            def verified(row):
                account = native(row).get('account') or {}; record = registered.get(account.get('id'))
                return (row.get('status') or {}).get('app')=='codex' and record is not None \
                    and type(account.get('rev')) is int and account['rev']>0 \
                    and (account.get('native') or {}).get('login_ref')==record['login_ref']
            if (a.get('account') or {}).get('kind') != 'subscription' or not verified(source):
                fail('current native subscription binding is unverified; use notify/offer','unavailable')
            # Common authorizes UNKNOWN capacity too. Cached percentages are not a
            # native permission grant; inspect the selected login before any continuation.
        consent = self._automatic_allowed(setup)
        affected = []
        for row in rows:
            b = native(row)
            if (row.get('status') or {}).get('app') != 'codex' or b.get('lifecycle') == 'ended': continue
            # A managed host without setup metadata cannot safely be assumed disjoint.
            if not (b.get('account_setup') or {}).get('id'): fail('an existing Codex host has unknown shared-setup membership')
            if (b.get('account_setup') or {}).get('id') != setup: continue
            member = dict(resource=row.get('id'), host=row.get('host'), session=(b.get('session') or {}).get('id'),
                          epoch=b.get('epoch'), incarnation=row.get('incarnation'), account=target['id'])
            if any(not text(member[k]) for k in member) or row.get('available') is not True \
                    or type(b.get('v')) is not int or b['v']!=0 or b.get('lifecycle') != 'live' \
                    or 'agent.command' not in (b.get('caps') or {}) \
                    or not isinstance(b.get('commands'),list) or not HOST_COMMANDS.issubset({c.get('name') for c in b.get('commands',[]) if isinstance(c,dict)}):
                fail('an affected Codex endpoint lacks native cutover/session evidence')
            private = b.get('_codex')
            ledger = private.get('input_ledger') if isinstance(private,dict) else None
            if (not evidence_complete(b,('_codex.input_ledger','epoch'))
                    or not isinstance(ledger,dict) or type(ledger.get('v')) is not int or ledger['v']!=0
                    or ledger.get('source')!='vqro-ledger' or ledger.get('epoch')!=b.get('epoch')
                    or ledger.get('complete') is not True or type(ledger.get('pending')) is not int or ledger['pending']!=0
                    or type(ledger.get('unknown')) is not int or ledger['unknown']!=0):
                fail('Waiting for a complete, empty vqro input ledger; native queue remains unknown','busy')
            if require_idle and (b.get('state') != 'idle' or b.get('run') is not None or b.get('requests')):
                fail('Waiting for all Codex turns and pending approvals to finish; no switch or durable intent yet','busy')
            affected.append(member)
        if not affected or len(affected) > MAX_MEMBERS: fail('Codex shared membership exceeds the cutover bound')
        if len({m['session'] for m in affected}) != len(affected):
            fail('a native conversation has multiple managed writers; cannot transfer ownership safely','conflict')
        affected.sort(key=lambda r: r['resource'])
        membership = hashlib.sha256(json.dumps(affected, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
        return dict(setup_id=setup, membership_revision=membership, affected=affected,
                    automatic_allowed=consent, native_episode=witness['turnId'])

    def _invoke(self, row, name, payload):
        # Each fence has explicit ALL-slot epoch witnesses; later phases use the
        # one-use fenced operation token, not the intentionally replaced old epoch.
        result = self.ext.vqro.call('resource.invoke', {'resource': row['id'], 'op': 'agent.command',
            'args': {'v': 0, 'name': name, 'args': payload},
            'deadline_ms': 30000})
        if isinstance(result,dict) and result.get('outcome')=='rejected':
            raise _NativeRejected('native Codex command rejected before effect','stale')
        if not isinstance(result, dict) or result.get('outcome') != 'observed' or not isinstance(result.get('data'), dict):
            fail('Codex cutover native outcome is unconfirmed; no automatic retry or continuation', 'indeterminate')
        return result['data']

    def _authorization(self, claim):
        authority = claim.get('authorization')
        if claim.get('automatic') is True:
            if authority != {'mode':'auto','principal':None,'owner_revision':None}:
                fail('AUTO cutover lacks common policy authority','forbidden')
        else:
            if not isinstance(authority,dict) or set(authority)!={'mode','principal','owner_revision'} \
                    or authority.get('mode')!='offer' or not text(authority.get('principal')) \
                    or type(authority.get('owner_revision')) is not int or authority['owner_revision']<1:
                fail('OFFER cutover needs an authenticated human click recorded in its claim','forbidden')
            owner = self.ext.vqro.call('workspace.get',{'key':'doc/principals/owner'})
            if not isinstance(owner,dict) or owner.get('value')!=authority['principal'] or owner.get('version')!=authority['owner_revision']:
                fail('OFFER human authority changed after the claim','stale')
        from accounts_policy import settings
        configured = settings(self.ext)
        mode = configured['blocked']
        if claim.get('automatic') is True:
            if not self._automatic_allowed(claim['setup_id']):
                fail('Automatic shared-login consent was revoked; offer instead','offer_required')
            maximum = configured.get('auto_cutover_max_per_hour',3)
            if mode!='auto_restart' or type(maximum) is not int or maximum<1:
                fail('AUTO policy was revoked or disabled; offer instead, no native switch','offer_required')
        elif mode not in ('offer_restart','auto_restart'):
            fail('NOTIFY policy never authorizes a native restart','forbidden')

    def _authorized_plan(self,claim):
        targets,revision=snapshot(self.ext)
        if revision!=claim.get('_account_revision'):fail('ordered target registry changed','stale')
        ids=claim.get('authorized_targets')
        if not isinstance(ids,list) or not 1<=len(ids)<=3 or any(not text(ident) for ident in ids) or len(set(ids))!=len(ids) or ids[0]!=claim.get('account'):
            fail('protected ordered target plan is missing or malformed','forbidden')
        if claim.get('automatic') is not True and len(ids)!=1:fail('manual owner authority permits only its singleton target','forbidden')
        eligible=eligible_alternatives(targets,claim.get('source_account'),set(),allow_api_key=False)
        by_id={r['id']:r for r in eligible if r['runtime']=='codex' and r['kind']=='subscription' and not selection_problem(r)}
        if any(ident not in by_id for ident in ids):fail('ordered target aliases source or has no usable native route','stale')
        plan=[by_id[ident] for ident in ids]
        if len({credential_identity(r) for r in plan})!=len(plan):fail('ordered targets duplicate credential identity','stale')
        return plan

    def _validate_claim(self, args, prepared, claim):
        from accounts_policy import episode, configured_id
        if not isinstance(claim,dict):fail('cutover claim is unavailable','forbidden')
        source = next((r for r in self.resources() if r.get('id')==args['resource']),None)
        if not source:fail('cutover source disappeared','stale')
        a = native(source)
        expected = dict(state='claimed',resource=args['resource'],host=source.get('host'),account=args['account'],
            _account_revision=args['_account_revision'],automatic=args['automatic'],runtime='codex',
            setup_id=prepared['setup_id'],membership_revision=prepared['membership_revision'],affected=prepared['affected'],
            native_episode=prepared['native_episode'],episode=episode(source),session=(a.get('session') or {}).get('id'),
            source_incarnation=source.get('incarnation'),source_epoch=a.get('epoch'),source_account=configured_id(source))
        if args['automatic'] and prepared['automatic_allowed'] is not True:
            fail('Automatic switching consent is off; use notify/offer. Shared login also affects external Codex clients.','offer_required')
        if args.get('episode')!=prepared['native_episode'] or any(claim.get(k)!=v for k,v in expected.items()):
            fail('protected Codex claim or source membership changed; no native effect','stale')
        if type(claim.get('version')) is not int or claim['version']<1:fail('claim acknowledgement unavailable','forbidden')
        self._authorization(claim)
        plan=self._authorized_plan(claim)
        if not self._common_target_allowed(claim,0):fail('common target budget or known-blocked policy no longer permits first switch','stale')
        return plan

    @staticmethod
    def _same_claim(old, current):
        projected={'version','max_switches','known_blocked_targets'}
        return isinstance(current,dict) and type(current.get('version')) is int and current['version']>=old['version'] \
            and {k:v for k,v in old.items() if k not in projected}=={k:v for k,v in current.items() if k not in projected}

    @staticmethod
    def _common_view_valid(claim):
        maximum=claim.get('max_switches');blocked=claim.get('known_blocked_targets')
        return type(maximum) is int and 0<=maximum<=3 and isinstance(blocked,list) and len(blocked)<=256 and all(text(ident) for ident in blocked)

    @staticmethod
    def _common_target_allowed(claim,index):
        if not Cutover._common_view_valid(claim):return False
        return claim.get('automatic') is not True or (index<claim['max_switches'] and claim['authorized_targets'][index] not in claim['known_blocked_targets'])

    def _fence_hosts(self, rows, hosts, payload, prepared, args, claim):
        fenced, attempted = [], []
        try:
            # All hosts fenced before checking idle; no credential effect in this phase.
            for members in hosts.values():
                row = rows[members[0]['resource']]; attempted.append(row)
                result = self._invoke(row, '_account.fence', dict(payload, members=[{'session':m['session'],'epoch':m['epoch']} for m in members]))
                fenced.append((row,result))
            end = time.monotonic() + 25
            while not all(result.get('quiescent') is True for _,result in fenced):
                if time.monotonic() >= end: fail('Codex turns are still running; no login switch or continuation','busy')
                time.sleep(.05)
                fenced = [(row,self._invoke(row,'_account.status',payload)) for row,_ in fenced]
            self._authorization(claim)
            current_claim = self.ext.vqro.call('command.invoke',{'name':'accounts.claim.read','args':{'claim':args['_claim']}})
            if self._prepare(args)!=prepared or not self._same_claim(claim,current_claim):
                fail('Codex source/target/claim changed after fencing; no switch','stale')
            self._validate_claim(args,prepared,current_claim)
            targets,_ = snapshot(self.ext)
            plan = self._authorized_plan(claim)
            active = []
            # Prove the target exists on EVERY native host before changing ANY login.
            for row,state in fenced:
                logins = self._invoke(row,'_account.list',payload).get('logins')
                selected=[]
                for target in plan:
                    matches = [r for r in logins if isinstance(r,dict) and all(r.get(k)==v for k,v in target['login_ref'].items())] if isinstance(logins,list) else []
                    if len(matches)!=1 or type(matches[0].get('active')) is not bool:
                        fail('ordered target is not uniquely available on every managed host','unavailable')
                    selected.append(matches[0])
                original = [r for r in logins if isinstance(r,dict) and r.get('active') is True]
                if len(original)!=1 or not all(text(original[0].get(k)) for k in ('sessionId','accountId')):
                    fail('original saved login is not uniquely evidenced; cannot restore safely','unavailable')
                original_ref = {k:original[0][k] for k in ('sessionId','accountId')}
                source = next((r for r in targets if r.get('login_ref')==original_ref and r.get('kind')=='subscription' and not selection_problem(r)),None)
                state['_restore_accounts'] = [{'login_ref':target['login_ref'],'account':fact(target)} for target in plan]
                if not any(candidate['login_ref']==original_ref for candidate in state['_restore_accounts']):
                    state['_restore_accounts'].append({'login_ref':original_ref,'account':fact(source) if source else None})
                active.append(selected[0]['active'])
            if args['automatic'] and all(active):
                fail('target native account is already active everywhere; no automatic restart loop','stale')
            # Native listing may block; repeat target/consent/authority validation afterwards.
            self._authorization(claim)
            current_claim = self.ext.vqro.call('command.invoke',{'name':'accounts.claim.read','args':{'claim':args['_claim']}})
            if self._prepare(args)!=prepared or not self._same_claim(claim,current_claim):
                fail('Codex plan changed during native target discovery; no switch','stale')
            self._validate_claim(args,prepared,current_claim)
            return fenced
        except Exception:
            # Abort releases ONLY local fences in the unattempted-switch stage. It can
            # never cancel/replay a native credential effect, even after an unknown fence reply.
            for row in attempted:
                try: self._invoke(row,'_account.abort',payload)
                except Exception: pass  # unknown cleanup stays visibly blocked; never retry it
            raise

    def cutover(self, args):
        with self.admission:
            return self._cutover(args)

    def _cutover(self, args):
        if not text(args.get('_claim')): fail('a durable common claim is required', 'forbidden')
        prepared = self._prepare(args)
        claim = self.ext.vqro.call('command.invoke', {'name': 'accounts.claim.read', 'args': {'claim': args['_claim']}})
        plan = self._validate_claim(args,prepared,claim)
        rows = {r['id']: r for r in self.resources()}
        hosts = {}
        for member in prepared['affected']:
            row = rows[member['resource']]
            host = native(row).get('host') or {}
            ident = host.get('id')
            if not text(ident): fail('native Codex host identity unavailable')
            hosts.setdefault((member['host'], ident), []).append(member)
        payload = {'operation': args['_claim'], 'setup_id': prepared['setup_id'],
                   'authorized_targets':[{'login_ref':r['login_ref'],'account':fact(r)} for r in plan]}
        fenced = self._fence_hosts(rows, hosts, payload, prepared, args, claim)
        attempted, blocked = [], []
        disposition = 'offer'
        for target_index,target in enumerate(plan):
            switched, dispatched = [], False
            try:
                for row, _ in fenced:
                    issue = self._walk_issue(args,claim,plan,target_index)
                    if issue:
                        if attempted:return self._walk_result(self._restore_revoked(fenced,hosts,payload,prepared,issue),attempted,blocked,'offer')
                        fail('protected switch authority changed before dispatch','stale')
                    if not dispatched:attempted.append(target['id'])
                    dispatched = True
                    result = self._invoke(row, '_account.switch', dict(payload, login_ref=target['login_ref'], account=fact(target)))
                    if result.get('account_confirmed') is not True:fail('native account readback unconfirmed','indeterminate')
                    switched.append(row)
            except Exception as error:
                if isinstance(error,_NativeRejected):
                    if dispatched and not switched:attempted.pop()  # Explicit no-effect target, not a charged switch.
                    if attempted:return self._walk_result(self._restore_revoked(fenced,hosts,payload,prepared,'unconfirmed'),attempted,blocked,'offer')
                if not attempted or (not switched and not dispatched):
                    for row,_ in fenced:
                        try:self._invoke(row,'_account.abort',payload)
                        except Exception:pass
                # An uncertain credential effect can still change the shared login:
                # keep every fence, never guess a prefix or resume/replay it.
                raise
            permissions = []
            for row,_ in fenced:
                issue = self._walk_issue(args,claim,plan)
                if issue:return self._walk_result(self._restore_revoked(fenced,hosts,payload,prepared,issue),attempted,blocked,'offer')
                try:quota=self._invoke(row,'_account.inspect',payload)
                except VqroError:quota={}  # Read-only uncertainty: resume independently verified B, zero model input.
                value=quota.get('ordinary_usage_allowed')
                valid=quota.get('source')=='codex.account/rateLimits' and type(quota.get('observed_at_ms')) is int and quota['observed_at_ms']>0
                permissions.append(value if valid and type(value) is bool else None)
                if valid and value is False:
                    observed={'account':target['id'],'observed_at_ms':quota['observed_at_ms'],
                              'reset_at_ms':None,'source':'codex.account/rateLimits'}
                    if blocked and blocked[-1]['account']==target['id']:
                        blocked[-1]['observed_at_ms']=max(blocked[-1]['observed_at_ms'],observed['observed_at_ms'])
                    else:blocked.append(observed)
            if all(value is False for value in permissions):
                disposition='all_blocked'
                continue  # Native phases independently require this false before next target.
            disposition='ready' if all(value is True for value in permissions) else 'offer'
            break
        final_index=len(attempted)-1 if disposition=='ready' else None
        issue = self._walk_issue(args,claim,plan,final_index)
        if issue:return self._walk_result(self._restore_revoked(fenced,hosts,payload,prepared,issue),attempted,blocked,'offer')
        issue,admissions = self._resume_and_release(fenced,payload,claim)
        expected = {m['resource']:target['id'] for m in prepared['affected']}
        result = self._observed(prepared,expected,bool(issue))
        by_session={}
        for proof in admissions:
            if isinstance(proof,dict) and text(proof.get('session')) and text(proof.get('epoch')):
                key=(proof['session'],proof['epoch'])
                by_session[key]=proof if key not in by_session else None
        for member in result['affected']:
            proof=by_session.get((member['session'],member['epoch'])) or {}
            if type(proof.get('caps_rev')) is int and proof['caps_rev']>0 and type(proof.get('expires_at_ms')) is int and proof['expires_at_ms']>int(time.time()*1000):
                member.update(admission_caps_rev=proof['caps_rev'],admission_expires_at_ms=proof['expires_at_ms'])
        if disposition=='ready' and any('admission_caps_rev' not in member for member in result['affected']):disposition='offer'
        if not issue and self._walk_issue(args,claim,plan,final_index):result.update(restored_only=True,continuation_allowed=False)
        return self._walk_result(result,attempted,blocked,disposition)

    def _walk_issue(self,args,claim,plan,target_index=None):
        try:
            # Registry reads can block: refresh private consent/owner authority
            # afterwards, then consume the final protected policy/allowance view.
            if self._authorized_plan(claim)!=plan:return 'unconfirmed'
            issue=self._authority_issue(claim)
            if issue:return issue
            current=self.ext.vqro.call('command.invoke',{'name':'accounts.claim.read','args':{'claim':args['_claim']}})
            if not self._same_claim(claim,current) or not self._common_view_valid(current):return 'unconfirmed'
            if target_index is not None and not self._common_target_allowed(current,target_index):return 'disabled'
        except Exception:return 'unconfirmed'
        return None

    @staticmethod
    def _walk_result(result,attempted,blocked,disposition):
        result.update(attempted_targets=list(attempted),inspected_blocked=blocked,disposition=disposition,fenced=False,
                      continuation_allowed=disposition=='ready' and result.get('restored_only') is not True)
        return result

    def _resume_and_release(self,fenced,payload,claim):
        resumed, failed, issue, admissions = [], False, None, []
        for row, _ in fenced:
            try:
                result = self._invoke(row, '_account.resume', payload)
                if result.get('resumed') is not True:fail('same-thread native resume unconfirmed','indeterminate')
                resumed.append(row)
            except Exception:failed = True
            issue = issue or self._authority_issue(claim)
        for row in resumed:
            issue = issue or self._authority_issue(claim)
            try:
                released=self._invoke(row, '_account.release', dict(payload,revoked=bool(issue),revocation_kind=issue))
                if released.get('released') is not True:fail('native fence release unconfirmed','indeterminate')
                if isinstance(released.get('admissions'),list):admissions.extend(released['admissions'])
            except Exception:failed = True
        if failed:fail('native lifecycle partly unconfirmed; other confirmed peers processed, no continuation','indeterminate')
        return issue or self._authority_issue(claim),admissions

    def _authority_issue(self,claim):
        try:self._authorization(claim)
        except VqroError as error:return 'disabled' if claim.get('automatic') is True and error.kind=='offer_required' else 'unconfirmed'
        return None

    def _restore_revoked(self,fenced,hosts,payload,prepared,issue):
        expected, restored, failed = {}, [], False
        # Finish every known host independently, never retrying an uncertain effect.
        for row,state in fenced:
            try:
                result = self._invoke(row,'_account.restore',dict(payload,accounts=state['_restore_accounts']))
                if result.get('resumed') is not True or result.get('account_confirmed') is not True:
                    fail('native lifecycle restore unconfirmed','indeterminate')
                account = result.get('account')
                ident = account.get('id') if isinstance(account,dict) else None
                hostkey = (row['host'],native(row)['host']['id'])
                for member in hosts[hostkey]:expected[member['resource']] = ident
                restored.append(row)
            except Exception:failed = True
        for row in restored:
            try:
                released=self._invoke(row,'_account.release',dict(payload,revoked=True,revocation_kind=issue))
                if released.get('released') is not True:fail('native restore fence release unconfirmed','indeterminate')
            except Exception:failed = True
        if failed:fail('switch authorization changed; lifecycle restoration partly unconfirmed, no continuation','indeterminate')
        return self._observed(prepared,expected,True)

    def _observed(self,prepared,expected,restored_only):
        # Native ACKs can precede core facts. Do not return an old epoch or an invented account.
        end = time.monotonic() + 3
        while True:
            current = {r['id']:r for r in self.resources()}
            members_now = {r['id'] for r in current.values() if (r.get('status') or {}).get('app')=='codex'
                and native(r).get('lifecycle')!='ended' and (native(r).get('account_setup') or {}).get('id')==prepared['setup_id']}
            if members_now != {m['resource'] for m in prepared['affected']}:
                fail('Codex group changed during native cutover; partial outcome, no continuation','indeterminate')
            affected = []
            for member in prepared['affected']:
                row = current.get(member['resource']); a = native(row) if row else {}; selected = a.get('account') or {}
                ident = expected[member['resource']]
                if not row or row.get('host') != member['host'] or row.get('incarnation') != member['incarnation'] \
                        or (a.get('session') or {}).get('id') != member['session'] \
                        or a.get('epoch') == member['epoch'] or not text(a.get('epoch')) \
                        or selected.get('id') != ident or (ident is not None and (type(selected.get('rev')) is not int or selected['rev'] < 1)):
                    break
                affected.append(dict(member,account=ident,epoch=a['epoch'],session_confirmed=True,resumed=True))
            if len(affected) == len(prepared['affected']):break
            if time.monotonic() >= end:fail('resumed thread/account publication unconfirmed; no continuation','indeterminate')
            time.sleep(.05)
        return dict(outcome='observed',setup_id=prepared['setup_id'],affected=affected,
            restored_only=restored_only,continuation_allowed=not restored_only,
            account=next(iter(expected.values())) if len(set(expected.values()))==1 else None)
