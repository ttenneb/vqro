#!/usr/bin/env python3
"""Replaceable account registry and native sign-in UI. Never opens a profile or credential store."""
import json
from pathlib import Path
import sys
import threading
import uuid
import accounts_policy as policy
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from account_profiles import SETTINGS, SELECTORS, MAX_ACCOUNTS, snapshot, validate, environment, provider_argv, login_environment, native_login_argv, require_fresh
from agent_launch import runtimes, valid_argv

COMMANDS = {'pi':'pi.new', 'codex':'codex.new', 'claude':'tier1.claude'}


class Accounts:
    def __init__(self, ext):
        self.ext = ext
        self.effects = policy.Effects(ext)
        self.seen = {}
        self.pending = set()
        self.observations = {}
        self.overloaded = False
        self.warned_overflow = False
        self.pending_lock = threading.Lock()
        self.wake = threading.Event()
        for name, method, title in [('policy',self.configure_policy,'Account capacity strategies'),
                ('offer',self.offer,'Fresh-work restart options'), ('restart',self.restart,'Start fresh work on another account'),
                ('show',self.show,'Accounts'), ('add',self.add,'Add runtime account'),
                ('remove',self.remove,'Remove account metadata'), ('sign_in',self.sign_in,'Sign in with native runtime')]:
            ext.command('accounts.'+name,title)(method)
        ext.contribute({'view/accounts':{'kind':'panel','title':'Accounts','items':[]},
                        'view/accounts-restarts':{'kind':'panel','title':'Fresh work (never move a conversation)','items':[]},
                        'menu/accounts':{'where':'status','label':'accounts','command':'accounts.show','order':55}})
        ext.on_event(self.changed)

    def show(self, args):
        rows, revision = snapshot(self.ext)
        items = [{'type':'text','text':'Runtime-owned logins. vqro stores only labels and literal profile paths; no login state is inspected.'}]
        entry=self.ext.vqro.call('workspace.get',{'key':policy.POLICY})
        try: configured=policy.validate_settings(entry['value'] if entry else {})
        except VqroError:
            configured=dict(policy.DEFAULTS)
            items.append({'type':'text','style':'warn','text':'Unrecognized account policy: using ask + notify, no API-key overflow. Saving replaces the invalid policy.'})
        for field,title,choices in [('new_agents','New agents',['ask','most_remaining_capacity','round_robin']),
                ('blocked','When quota/rate limited',['notify','offer_restart','auto_restart']),
                ('api_key_overflow','Allow automatic API-key profiles',[False,True])]:
            items.append({'type':'select','text':title,'value':configured[field], 'command':'accounts.policy',
                          'args':{'field':field,'expect':entry['version'] if entry else 0},
                          'options':[{'label':str(value).replace('_',' '),'value':value} for value in choices]})
        items.append({'type':'text','style':'dim','text':'Capacity = fresh reported percentage headroom, not entitlement. Automatic restart creates ONE empty agent, never transfers history/input. API-key overflow is off by default.'})
        with self.ext.cache_lock:
            blocked_ids=[rid for rid,row in self.ext.resources.items() if policy.blocked(row,limit_only=True)][:64]
        items.extend({'text':'Fresh-work options for '+label(rid),'command':'accounts.offer','args':{'resource':rid}} for rid in blocked_ids)
        for runtime in SELECTORS:
            items.append({'type':'text','style':'bold','text':runtime})
            items.append({'text':'Use my normal login', 'command':COMMANDS[runtime], 'args':{'account':'normal'}})
            items.append({'text':'Sign in… (normal '+runtime+')', 'command':'accounts.sign_in',
                          'args':{'account':'normal','runtime':runtime}})
            for row in rows:
                if row['runtime'] != runtime: continue
                items.append({'type':'text','text':label(row['label']+' · '+row.get('provider',runtime)+' · '+row['kind'])})
                items.append({'text':'New agent on '+label(row['label']), 'command':COMMANDS[runtime],
                              'args':{'account':row['id'],'_account_revision':revision}})
                items.append({'text':'Sign in…', 'description':'Pi: use /login in the native terminal' if runtime=='pi' else 'Native browser/device flow',
                              'command':'accounts.sign_in', 'args':{'account':row['id'],'_account_revision':revision}})
                items.append({'text':'Remove account metadata', 'command':'accounts.remove',
                              'args':{'account':row['id'],'expect':revision}, 'style':'dim'})
        items.append({'text':'Add account…', 'input':'JSON {id,label,runtime,provider?,kind,profile}; metadata only, never tokens',
                      'command':'accounts.add'})
        self.ext.publish('accounts',{'title':'Accounts','requires':['menu','input'],'items':items})
        return {'client':{'popup':{'view':'accounts','width':90}}}

    def add(self, args):
        try: row = json.loads(args['text']) if 'text' in args else args
        except (ValueError,TypeError): raise VqroError('enter a metadata-only account JSON object','invalid') from None
        account = validate(row)
        rows, revision = snapshot(self.ext)
        if len(rows) >= MAX_ACCOUNTS: raise VqroError('accounts registry is full','overloaded')
        if any(r['id']==account['id'] for r in rows): raise VqroError('account id already exists','conflict')
        self.ext.apply([{'op':'set','key':SETTINGS,'expect':revision,'value':{'accounts':rows+[account]}}], 'accounts: add metadata')
        self.ext.notify('Account metadata saved; use Sign in… to open the runtime. No profile or credential was read.','ok')
        return {'client':{'close':True}}

    def remove(self, args):
        rows, revision = snapshot(self.ext)
        if type(args.get('expect')) is not int or args['expect'] != revision:
            raise VqroError('account registry changed; reopen Accounts before removing metadata','stale')
        remaining = [r for r in rows if r['id'] != args.get('account')]
        if len(remaining)==len(rows): raise VqroError('account metadata not found','not_found')
        self.ext.apply([{'op':'set','key':SETTINGS,'expect':revision,'value':{'accounts':remaining}}], 'accounts: remove metadata only')
        self.ext.notify('Account metadata removed. Native profiles/logins and running agents are unchanged.','ok')
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
        if '_account_revision' in args and args['_account_revision'] != revision:
            raise VqroError('account registry changed; reopen Accounts before signing in','stale')
        account = None
        if args.get('account')=='normal': runtime=args.get('runtime')
        else:
            account=next((r for r in rows if r['id']==args.get('account')),None)
            if account is None: raise VqroError('account metadata not found','not_found')
            runtime=account['runtime']
            if account['kind']=='api_key':
                raise VqroError('API-key import is not implemented; configure keys only through the native runtime, never Accounts','unsupported')
        if runtime not in SELECTORS: raise VqroError('choose a native program','invalid')
        native, env = self.native_argv(runtime,args.get('launcher'))
        argv = provider_argv(native,account)
        params={'argv':argv,'name':runtime+'-sign-in-'+uuid.uuid4().hex[:12], 'env':environment(account, env)}
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
        value=policy.validate_settings(dict(policy.effective(entry['value'] if entry else {}), **{args['field']:args.get('value')}))
        self.ext.apply([{'op':'set','key':policy.POLICY,'expect':revision,'value':value}], 'accounts: capacity policy')
        return self.show({})

    def source(self,rid):
        rows=self.ext.vqro.call('resource.list',{'id':rid}).get('resources',[])
        if len(rows)!=1 or not policy.blocked(rows[0],limit_only=True):
            raise VqroError('source is no longer a live quota/rate-limited agent','stale')
        policy.episode(rows[0])  # require an observed incarnation and native epoch
        return rows[0]

    def alternatives(self,source,configured):
        rows,revision=snapshot(self.ext)
        observed=policy.resources(self.ext)
        blocked_ids={policy.configured_id(row) for row in observed if policy.blocked(row)}
        candidates=[row for row in rows if row['id']!=policy.configured_id(source) and row['id'] not in blocked_ids
                    and (row['kind']!='api_key' or configured['api_key_overflow'])]
        return candidates,revision,policy.capacities(observed,candidates)

    def notify_once(self,source,mode,text):
        key=(source['id'],policy.episode(source),mode)
        if key in self.seen: return
        if len(self.seen)>=256: self.seen.pop(next(iter(self.seen)))
        self.seen[key]=True
        self.ext.notify(text,'warn')

    def offer(self,args):
        source=self.source(args.get('resource'))
        candidates,revision,scores=self.alternatives(source,policy.settings(self.ext))
        token=policy.episode(source)
        items=[{'type':'text','text':'ONE new empty agent. The blocked conversation stays where it is; no prompt, history, queue, model choice or session ID is copied.'}]
        for target in candidates:
            score=scores.get(target['id'])
            items.append({'text':'Fresh '+target['runtime']+' on '+label(target['label']),
                          'description':('Reported headroom '+str(round(score,1))+'%' if score is not None else 'Capacity unknown/incomparable')+'; launcher defaults',
                          'command':'accounts.restart','args':{'resource':source['id'],'episode':token,
                                                               'account':target['id'],'_account_revision':revision}})
        if not candidates: items.append({'type':'text','text':'No configured alternative allowed by this policy. Native login and API-key setup are separate, explicit actions.'})
        self.ext.publish('accounts-restarts',{'title':'Fresh work, not a conversation migration','requires':['menu'],'items':items})
        return {'client':{'popup':{'view':'accounts-restarts','width':90}}}

    def restart(self,args):
        source=self.source(args.get('resource'))
        if args.get('episode')!=policy.episode(source): raise VqroError('blocked agent changed; reopen fresh-work options','stale')
        candidates,revision,_=self.alternatives(source,policy.settings(self.ext))
        if type(args.get('_account_revision')) is not int or args['_account_revision']!=revision:
            raise VqroError('account registry changed; reopen fresh-work options','stale')
        target=next((row for row in candidates if row['id']==args.get('account')),None)
        if target is None: raise VqroError('alternative is no longer allowed','stale')
        return self.launch_fresh(source,target,revision)

    def automatic_argv(self,runtime):
        key={'pi':'doc/settings/pi','codex':'doc/settings/codex','claude':'doc/settings/tier1'}[runtime]
        entry=self.ext.vqro.call('workspace.get',{'key':key});value=entry['value'] if entry else None
        if runtime=='claude': argv=value.get('claude',['claude']) if isinstance(value,dict) else ['claude']
        else:
            default=[{'name':runtime,'argv':['pi1','--mode','rpc'] if runtime=='pi' else ['codex','app-server','--stdio']}]
            options=runtimes(value if value is not None else {'runtimes':default})
            if len(options)!=1: raise VqroError('choose a runtime executable explicitly before fresh work','unavailable')
            argv=options[0]['argv']
        require_fresh(runtime,argv)
        return argv

    def launch_fresh(self,source,target,revision,automatic=False):
        def permitted(configured):
            return (not automatic or configured['blocked']=='auto_restart') \
                and (target['kind']!='api_key' or configured['api_key_overflow'])
        if not permitted(policy.settings(self.ext)):
            raise VqroError('fresh-work policy changed; no launch','stale')
        token=policy.episode(source)
        self.effects.claim(source,token,target)  # acknowledged protected CAS BEFORE any launcher command
        # Recheck the native source. A failed/stale check consumes this claim; never retry it automatically.
        current=self.source(source['id'])
        if policy.episode(current)!=token: raise VqroError('source changed after fresh-launch claim; no launch','stale')
        candidates,current_revision,scores=self.alternatives(current,policy.settings(self.ext))
        selected=next((row for row in candidates if row['id']==target['id']),None)
        if current_revision!=revision or selected!=target:
            raise VqroError('target/registry changed after fresh-launch claim; claim consumed, no launch','stale')
        if automatic:
            headroom=scores.get(target['id'])
            if headroom is None or headroom<=0:
                raise VqroError('target capacity is no longer fresh/known/positive; claim consumed, no launch','stale')
            provider_argv(self.automatic_argv(target['runtime']),target)
        # Last policy read follows the potentially delayed claim AND fresh observation/runtime reads.
        # This is not an atomic native quota reservation; later native changes remain possible.
        if not permitted(policy.settings(self.ext)):
            raise VqroError('fresh-work policy changed after claim; claim consumed, no launch','stale')
        try:
            result=self.ext.invoke(COMMANDS[target['runtime']],{'account':target['id'],'_account_revision':revision,'fresh':True})
        except VqroError:
            self.ext.notify('Fresh launch outcome unconfirmed. Do not repeat it automatically; old conversation untouched.','warn')
            raise
        try: self.effects.finish(source,token,target,result)
        except VqroError:
            self.ext.notify('Fresh launch result could not be journaled; the original claim remains. No automatic retry.','warn')
        self.ext.notify('Fresh launch '+str(result.get('outcome','unconfirmed'))+'. No source input/history was copied; inspect the new native session.','info')
        return result

    def handle_blocked(self,rid):
        source=None
        try:
            source=self.source(rid);configured=policy.settings(self.ext);mode=configured['blocked']
            message='Agent '+label(rid)+' is blocked by a native quota/rate limit. Old conversation untouched.'
            if mode=='notify': return self.notify_once(source,mode,message)
            if mode=='offer_restart':
                self.offer({'resource':rid})
                return self.notify_once(source,mode,message+' Open Accounts for explicit fresh-work options.')
            candidates,revision,scores=self.alternatives(source,configured)
            candidates=[row for row in candidates if scores.get(row['id']) is not None and scores[row['id']]>0]
            if not policy.configured_id(source) or not candidates:
                return self.notify_once(source,mode,message+' No known positive reported alternative; no automatic launch.')
            journal,_=self.effects.read()
            if policy.episode(source) in journal['attempts']: return
            if self.effects.suppressed(journal,source):
                return self.notify_once(source,mode,message+' Automatic fresh-launch chain suppressed; choose new work manually.')
            candidates=[row for row in candidates if row['id'] not in journal['uncertain']]
            if not candidates: return self.notify_once(source,mode,message+' Alternative launch outcomes are uncertain; no automatic retry.')
            programs={};launchable=[]
            for target in candidates:
                runtime=target['runtime']
                if runtime not in programs:
                    try: programs[runtime]=self.automatic_argv(runtime)
                    except VqroError: programs[runtime]=None
                if programs[runtime] is None: continue
                try: provider_argv(programs[runtime],target)
                except VqroError: continue
                launchable.append(target)
            if not launchable:
                return self.notify_once(source,mode,message+' Fresh runtime executable/options need explicit selection; no automatic launch.')
            target=max(launchable,key=lambda row:scores[row['id']])
            self.launch_fresh(source,target,revision,automatic=True)
        except VqroError as error:
            # No replay, even after durability_unconfirmed/indeterminate. Do not echo native error bodies.
            text='Account fresh-work policy took no further action ('+str(error.kind)+'); inspect Accounts/native status.'
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
                           limits,native.get('incomplete'),declared]
                if self.observations.get(rid)==signature: return
                if len(self.observations)>=256: self.observations.pop(next(iter(self.observations)))
                self.observations[rid]=signature
                cached=self.ext.value(policy.POLICY,{})
                if not (isinstance(cached,dict) and cached.get('blocked')=='auto_restart' and limits):
                    ids=[rid] if policy.blocked(row,limit_only=True) else []
                else: ids=[key for key,value in self.ext.resources.items() if policy.blocked(value,limit_only=True)]
            else: ids=[key for key,value in self.ext.resources.items() if policy.blocked(value,limit_only=True)]
        with self.pending_lock:
            fresh=[rid for rid in ids if rid not in self.pending]
            room=64-len(self.pending)
            self.overloaded=self.overloaded or len(fresh)>room
            self.pending.update(fresh[:room])
        self.wake.set()

    def worker(self):
        while True:
            self.wake.wait();self.wake.clear()
            if self.overloaded and not self.warned_overflow:
                self.warned_overflow=True
                self.ext.notify('Account limit-policy queue is full; inspect Agent status. Excess work was not automatically launched.','warn')
            while True:
                with self.pending_lock:
                    if not self.pending: break
                    rid=self.pending.pop()
                try: self.handle_blocked(rid)
                except Exception:
                    # Keep the worker alive, but never retry an unexpected failure's popped action.
                    print('accounts: fresh-work handling failed; no automatic retry',file=sys.stderr,flush=True)


def main():
    ext=Extension('accounts',event_types=['entries','resource','resource.removed'])
    accounts=Accounts(ext)
    threading.Thread(target=accounts.worker,daemon=True,name='accounts-capacity').start()
    accounts.changed('missed',{})
    ext.run()

if __name__=='__main__': main()
