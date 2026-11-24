"""Pi-only credential-alias preparation. No credential/history reads or model calls.

Message inference is the explicit human-approved Pi exception, kept entirely in
pi_accounts/limits.py and its sourced/versioned table, never native availability.
"""
import hashlib
import json
import re
import time

import accounts_policy as policy
from account_credentials import snapshot, selection_problem, credential_identity, require_fresh, provider_argv
from agent_launch import runtimes
from pi_accounts.limits import match_limit
from accounts_cutover import validate_claim_authority, validate_offer_policy_after_owner
from vqro_ext import VqroError

UUID=re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',re.I)
COMMAND='vqro-account-quiesce'


def bounded(value, maximum=256):
    return isinstance(value,str) and bool(value) and len(value)<=maximum and value.isprintable()


def history_file(native):
    value=((native.get('session') or {}).get('native') or {}).get('file')
    if not bounded(value,4096):
        raise VqroError('Pi native history is not persisted/resumable; keep this process and use native controls','unsupported')
    return value


def same_model(original,current,target):
    """Native model/alias/API and exact conversation thinking-level readback."""
    if not isinstance(original,dict) or not isinstance(current,dict):return False
    before=original.get('native') or {};after=current.get('native') or {}
    return (isinstance(before,dict) and isinstance(after,dict)
            and bounded(original.get('id')) and bounded(original.get('effort'),64)
            and current.get('id')==original['id'] and current.get('effort')==original['effort']
            and after.get('provider')==target.get('native_provider') and after.get('api')==before.get('api'))


def membership_revision(affected,episode,revision,model,history,principal):
    members=[affected] if isinstance(affected,dict) else affected
    return hashlib.sha256(json.dumps([members,episode,revision,model,history,principal],sort_keys=True,separators=(',',':')).encode()).hexdigest()


def error_record(row):
    native=policy.facts(row);completion=native.get('completion')
    if not isinstance(completion,dict) or completion.get('reason')!='error': return None
    witness=completion.get('native');error=completion.get('error')
    if not isinstance(witness,dict) or witness.get('stopReason')!='error' or not isinstance(error,dict):return None
    timestamp=witness.get('timestamp')
    if type(timestamp) is not int or not 0<=timestamp<=9007199254740991 or not isinstance(error.get('message'),str):return None
    return {'stopReason':'error','timestamp':timestamp,'errorMessage':error['message'],
            'complete':witness.get('error_message_complete') is True}


def eligible_error(row):
    """Pure listener predicate: native error evidence, not quota classification."""
    native=policy.facts(row);session=native.get('session');caps=native.get('caps');commands=native.get('commands')
    return (row.get('kind')=='agent' and row.get('available') is True and native.get('lifecycle')=='live'
            and native.get('state')=='idle' and native.get('queue')==[] and error_record(row) is not None
            and isinstance(session,dict) and isinstance(session.get('id'),str) and UUID.fullmatch(session['id']) is not None
            and isinstance(caps,dict) and isinstance(caps.get('agent.command'),dict)
            and isinstance(commands,list) and any(isinstance(command,dict) and command.get('name')==COMMAND for command in commands))


class PiCutover:
    def __init__(self,ext):
        self.ext=ext
        ext.command('accounts.prepare.pi','Prepare same-conversation Pi account restart')(self.prepare)
        ext.command('accounts.cutover.pi','Restart this Pi conversation on another account')(self.restart)

    def source(self,args):
        rows=self.ext.vqro.call('resource.list',{'id':args.get('resource')}).get('resources',[])
        if len(rows)!=1 or not eligible_error(rows[0]):
            raise VqroError('Pi requires an idle, unqueued native error with a stable record; nothing restarted','busy')
        row=rows[0];native=policy.facts(row)
        if policy.episode(row)!=args.get('episode'):
            raise VqroError('Pi source changed; reopen account options','stale')
        provider=self.ext.providers.get(row.get('provider')) or {}
        if provider.get('role')!='runtime' or provider.get('host')!=row.get('host') or not bounded(row.get('host'),128) \
                or not isinstance((provider.get('capabilities') or {}).get('agent.command'),dict):
            raise VqroError('Pi source execution host is not authenticated for native commands','unsupported')
        history_file(native)
        if not bounded(native.get('tool_principal'),128) or (native.get('caps',{}).get('agent') or {}).get('tool_principal')!='env':
            raise VqroError('Pi logical work principal is unavailable; do not invent a fresh work scope','unsupported')
        model=native['session'].get('model');native_model=model.get('native') if isinstance(model,dict) else None
        if not isinstance(native_model,dict) or not bounded(native_model.get('provider'),128) \
                or not bounded(native_model.get('api'),128) or not bounded(model.get('id')):
            raise VqroError('native Pi model metadata is unavailable; upgrade the matching bridge','unsupported')
        if not bounded(model.get('effort'),64):
            raise VqroError('native Pi thinking level is unavailable; nothing stopped','unsupported')
        if native_model['api']=='pi-virtual':
            raise VqroError('account switching unavailable for this model','unsupported')
        return row

    def prepare(self,args):
        if not isinstance(args,dict) or type(args.get('automatic')) is not bool:
            raise VqroError('invalid Pi cutover preparation','invalid')
        source=self.source(args);native=policy.facts(source)
        rows,revision=snapshot(self.ext)
        if type(args.get('_account_revision')) is not int or args['_account_revision']!=revision:
            raise VqroError('Pi account registry changed','stale')
        target=next((row for row in rows if row['id']==args.get('account')),None)
        if target is None or selection_problem(target) or target['runtime']!='pi' or target['kind']!='subscription':
            raise VqroError('Pi target requires a complete native subscription alias','unsupported')
        old=next((row for row in rows if row['id']==policy.configured_id(source)),None)
        if old and credential_identity(old) is not None and credential_identity(old)==credential_identity(target):
            raise VqroError('Pi source and target are aliases of the same credential','invalid')
        model=native['session']['model'];current_provider=model['native']['provider']
        if current_provider==target['native_provider']:
            raise VqroError('Pi target already names the current native credential','invalid')
        base=(native.get('account') or {}).get('provider') or current_provider
        allowed={base}
        if old and old.get('runtime')=='pi' and selection_problem(old) is None:
            allowed.add(old['native_provider'])
        if current_provider not in allowed:
            raise VqroError('Pi native model no longer matches its configured account context','stale')
        if target['provider']!=base:
            raise VqroError('Pi account cutover must preserve the native model provider','unsupported')
        entry=self.ext.vqro.call('workspace.get',{'key':'doc/settings/pi'})
        programs=runtimes(entry['value'] if entry else {'runtimes':[{'name':'pi','argv':['pi1','--mode','rpc']}]})
        if len(programs)!=1:
            raise VqroError('Pi restart requires one unambiguous shared native runtime','unsupported')
        require_fresh('pi',programs[0]['argv']);provider_argv(programs[0]['argv'],target)
        session=native['session']['id'];record=error_record(source)
        episode='pi-error:'+hashlib.sha256(json.dumps([session,record['timestamp']],separators=(',',':')).encode()).hexdigest()
        affected={'resource':source['id'],'host':source['host'],'session':session,'epoch':native['epoch'],
                  'incarnation':source['incarnation'],'account':target['id']}
        inference=match_limit(base,record)
        membership=membership_revision(affected,episode,entry['version'] if entry else 0,model,history_file(native),native['tool_principal'])
        return {'setup_id':'pi:'+session,'membership_revision':membership,'native_episode':episode,
                'automatic_allowed':inference is not None,'affected':[affected],'inference':inference}

    def claimed(self,args,source,plan):
        claim=self.ext.invoke('accounts.claim.read',{'claim':args.get('_claim')})
        native=policy.facts(source)
        expected={key:args[key] for key in ('resource','episode','account','_account_revision','automatic')}
        expected.update(state='claimed',runtime='pi',host=source['host'],session=native['session']['id'],
                        source_incarnation=source['incarnation'],source_epoch=native['epoch'],
                        source_account=policy.configured_id(source),native_episode=plan['native_episode'],
                        setup_id=plan['setup_id'],membership_revision=plan['membership_revision'],
                        affected=plan['affected'],inference=plan['inference'],source_history=history_file(native),source_principal=native['tool_principal'])
        if not isinstance(claim,dict) or any(claim.get(key)!=value for key,value in expected.items()):
            raise VqroError('protected Pi source/session/error claim changed; nothing restarted','conflict')
        mode=claim.get('blocked_mode')
        if mode not in ('auto_restart','offer_restart') or args['automatic'] and mode!='auto_restart':
            raise VqroError('current protected mode does not allow this switch; nothing stopped','offer_required')
        validate_claim_authority(self.ext,claim,args['automatic'])
        if not args['automatic']:
            validate_offer_policy_after_owner(self.ext,mode)
        if args['automatic']:
            allowance=claim.get('max_switches');blocked=claim.get('known_blocked_targets')
            if type(allowance) is not int or allowance<1 or not isinstance(blocked,list) or args['account'] in blocked:
                raise VqroError('Pi switch no longer has protected budget/target eligibility; nothing stopped','offer_required')
            # The protected projection computes allowance from its final policy
            # read. Do not follow it with a separate blocking settings read and
            # accidentally combine an older allowance with a newer positive cap.
        return claim

    def restart(self,args):
        plan=self.prepare(args)
        if args['automatic'] and not plan['automatic_allowed']:
            raise VqroError('unrecognized Pi native error; owner-click OFFER only','offer_required')
        source=self.source(args);self.claimed(args,source,plan)
        if self.prepare(args)!=plan:
            raise VqroError('Pi error/session/runtime changed after claim','stale')
        source=self.source(args);self.claimed(args,source,plan)
        native=policy.facts(source);model=native['session']['model'];record=error_record(source)
        request={'v':0,'caps_rev':native['caps']['rev'],'epoch':native['epoch'],
                 'guard':{'session':native['session']['id']},'name':COMMAND,
                 'args':json.dumps({'session':native['session']['id'],'provider':model['native']['provider'],
                                    'model':model['id'],'error_timestamp':record['timestamp'],'file':history_file(native)},separators=(',',':'))}
        stopped=self.ext.vqro.call('resource.invoke',{'resource':source['id'],'op':'agent.command','args':request})
        if not isinstance(stopped,dict) or stopped.get('outcome') not in ('accepted','observed','dispatched'):
            raise VqroError('native Pi quiescent stop unconfirmed; do not retry','indeterminate')
        # This waits only for our old native process, never another agent. Native
        # ordinary shutdown settles credential writes. A timeout is NOT success.
        deadline=time.monotonic()+8
        while True:
            rows=self.ext.vqro.call('resource.list',{'id':source['id']}).get('resources',[])
            if len(rows)!=1:
                raise VqroError('old Pi exit evidence unavailable; no replacement started','indeterminate')
            ended=rows[0];old=policy.facts(ended);exit_info=ended.get('exit')
            if old.get('lifecycle')=='ended':
                if not isinstance(exit_info,dict) or exit_info.get('success') is not True or exit_info.get('code')!=0:
                    raise VqroError('old Pi did not confirm successful native shutdown; no replacement started','indeterminate')
                break
            if time.monotonic()>=deadline:
                raise VqroError('old Pi is still settling; no replacement started; never replay','indeterminate')
            time.sleep(.1)
        # Exactly one public frontend launch request. Common owns the only
        # protected intent and any NEW continuation; this callback sends none.
        launched=self.ext.invoke('pi.resume',{'_claim':args['_claim']})
        rid=launched.get('agent') if isinstance(launched,dict) else None
        if not bounded(rid) or launched.get('outcome')!='observed':
            raise VqroError('Pi replacement start unconfirmed; do not retry','indeterminate')
        rows=self.ext.vqro.call('resource.list',{'id':rid}).get('resources',[])
        registry,revision=snapshot(self.ext)
        target=next((row for row in registry if row['id']==args['account']),None)
        if revision!=args['_account_revision'] or target is None:
            raise VqroError('Pi account registry changed during restart; no continuation','indeterminate')
        if len(rows)!=1:
            raise VqroError('resumed Pi endpoint unavailable; no continuation','indeterminate')
        resumed=rows[0];state=policy.facts(resumed);session=state.get('session') or {}
        next_model=session.get('model') or {}
        provider=self.ext.providers.get(resumed.get('provider')) or {}
        if resumed.get('available') is not True or state.get('lifecycle')!='live' \
                or state.get('epoch')==native['epoch'] or not bounded(state.get('epoch')) \
                or state.get('tool_principal')!=native['tool_principal'] \
                or ((state.get('caps') or {}).get('agent') or {}).get('tool_principal')!='env' \
                or session.get('id')!=native['session']['id'] or policy.configured_id(resumed)!=target['id'] \
                or not same_model(model,next_model,target) or history_file(state)!=history_file(native) or provider.get('role')!='runtime' \
                or provider.get('host')!=resumed.get('host') or resumed.get('host')!=launched.get('host'):
            raise VqroError('Pi native same-session/account/model readback unconfirmed; no continuation','indeterminate')
        affected={'resource':rid,'host':resumed['host'],'session':session['id'],'requested_session':native['session']['id'],
                  'epoch':state['epoch'],'account':target['id'],'process_started':True,'session_confirmed':True,'resumed':True}
        return {'outcome':'observed','account':target['id'],'setup_id':plan['setup_id'],'affected':[affected],
                'notice':plan['inference']['label'] if plan['inference'] else 'Pi resumed after a human-judged native error'}
