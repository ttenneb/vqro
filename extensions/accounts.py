#!/usr/bin/env python3
"""Replaceable account registry and native sign-in UI. Never opens a profile or credential store."""
import json
from pathlib import Path
import sys
import uuid
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from account_profiles import SETTINGS, SELECTORS, MAX_ACCOUNTS, snapshot, validate, environment, provider_argv
from agent_launch import runtimes, valid_argv

COMMANDS = {'pi':'pi.new', 'codex':'codex.new', 'claude':'tier1.claude'}


class Accounts:
    def __init__(self, ext):
        self.ext = ext
        for name, method, title in [('show',self.show,'Accounts'), ('add',self.add,'Add runtime account'),
                ('remove',self.remove,'Remove account metadata'), ('sign_in',self.sign_in,'Sign in with native runtime')]:
            ext.command('accounts.'+name,title)(method)
        ext.contribute({'view/accounts':{'kind':'panel','title':'Accounts','items':[]},
                        'menu/accounts':{'where':'status','label':'accounts','command':'accounts.show','order':55}})

    def show(self, args):
        rows, revision = snapshot(self.ext)
        items = [{'type':'text','text':'Runtime-owned logins. vqro stores only labels and literal profile paths; no login state is inspected.'}]
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
            # Match the launcher's explicit profile selectors, not credentials or arbitrary env.
            selector = SELECTORS[runtime]
            if selector in selected.get('bridge_env', {}): env[selector] = selected['bridge_env'][selector]
            if runtime == 'codex':
                for field,key in (('codex_home','CODEX_HOME'),('home','HOME')):
                    if field in selected: env[key] = selected[field]
        if runtime=='codex':
            if 'app-server' not in argv: raise VqroError('Codex sign-in needs a runtime argv containing app-server','unsupported')
            return argv[:argv.index('app-server')] + ['login'], env
        if runtime=='claude': return [*argv,'auth','login'], env
        interactive=[]; i=0
        while i<len(argv):
            if argv[i]=='--mode':
                if i+1>=len(argv) or argv[i+1]!='rpc': raise VqroError('Pi native sign-in requires rpc or interactive mode','unsupported')
                i+=2; continue
            if argv[i]=='--mode=rpc': i+=1; continue
            interactive.append(argv[i]); i+=1
        return interactive, env

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


def main():
    ext=Extension('accounts',event_types=['entries'])
    Accounts(ext)
    ext.run()

if __name__=='__main__': main()
