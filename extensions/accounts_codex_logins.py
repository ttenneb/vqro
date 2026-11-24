"""Explicit, read-only Codex saved-login discovery. Only registration writes state."""
import uuid
from account_credentials import snapshot, validate, credential_identity
from accounts_policy import facts
from codex_cutover import evidence_complete
from vqro_display import label
from vqro_ext import VqroError

VIEW = 'accounts-codex-logins'


def fail(message, kind='stale'):
    raise VqroError(message, kind)


def text(value):
    return isinstance(value,str) and bool(value) and value.isprintable() and len(value.encode())<=256


class SavedLogins:
    def __init__(self, ext, register):
        self.ext, self.register = ext, register
        for suffix,method,title in [('',self.open,'Add Codex saved login…'),
                ('.refresh',self.open,'Refresh saved logins'),('.list',self.list,'List Codex saved logins'),
                ('.add',self.add,'Register Codex saved login')]:
            ext.command('accounts.codex.logins'+suffix,title)(method)

    def _witness(self,row):
        if not isinstance(row,dict):fail('Invalid Codex resource.','invalid')
        a=facts(row);commands=a.get('commands')
        caps=a.get('caps') if isinstance(a.get('caps'),dict) else {}
        host=a.get('host') if isinstance(a.get('host'),dict) else {}
        setup=a.get('account_setup') if isinstance(a.get('account_setup'),dict) else {}
        status=row.get('status') if isinstance(row.get('status'),dict) else {}
        values={'resource':row.get('id'),'execution_host':row.get('host'),
                'native_host':host.get('id'),'setup_id':setup.get('id'),'epoch':a.get('epoch')}
        if row.get('kind')!='agent' or row.get('available') is not True or a.get('lifecycle')!='live' \
                or status.get('app')!='codex' or type(a.get('v')) is not int or a['v']!=0 \
                or not all(text(v) for v in values.values()) or type(caps.get('rev')) is not int or caps['rev']<0 \
                or not isinstance(caps.get('agent.command'),dict) or not isinstance(commands,list) \
                or not any(isinstance(c,dict) and c.get('name')=='_account.list' for c in commands) \
                or not evidence_complete(a,('epoch','host','account_setup','caps','commands','lifecycle')):
            fail('Choose an available managed Codex agent with a declared setup and saved-login listing.', 'unavailable')
        return dict(values,caps_rev=caps['rev'],**{key:row.get(key) for key in ('provider','instance','incarnation')})

    def _host(self,selection):
        if not isinstance(selection,dict) or not text(selection.get('resource')):
            fail('Choose a managed Codex host first.','invalid')
        rows=self.ext.vqro.call('resource.list',{'id':selection['resource']}).get('resources',[])
        if len(rows)!=1 or self._witness(rows[0])!=selection:
            fail('Codex host/setup changed; reopen the saved-login picker.')
        return selection

    def _publish(self,items):
        self.ext.publish(VIEW,{'title':'Codex saved logins','requires':['menu','input'],'items':items})
        return {'client':{'popup':{'view':VIEW,'width':90}}}

    def open(self,args):
        items=[{'type':'text','text':'Choose the managed Codex agent/setup to inspect. Listing and registration do not sign in or switch accounts.'}]
        rows=self.ext.vqro.call('resource.list',{}).get('resources',[])
        for row in rows:
            try:selection=self._witness(row)
            except VqroError:continue
            items.append({'text':label('Agent '+selection['resource']+' · setup '+selection['setup_id']+' · execution host '+selection['execution_host']),
                          'command':'accounts.codex.logins.list','args':{'selection':selection}})
        if len(items)==1:items.append({'type':'text','style':'warn','text':'No available managed Codex host with setup_id. Launch through Codex runtime settings first.'})
        return self._publish(items)

    def _logins(self,selection):
        self._host(selection)
        try:
            result=self.ext.vqro.call('resource.invoke',{'resource':selection['resource'],'op':'agent.command',
                'args':{'v':0,'epoch':selection['epoch'],'caps_rev':selection['caps_rev'],'name':'_account.list','args':{}},'deadline_ms':5000})
        except VqroError:
            # Do not echo native error payloads or retry a runtime operation.
            fail('Codex saved-login listing unavailable; no registration made. Refresh explicitly.','unavailable')
        if not isinstance(result,dict) or result.get('outcome')!='observed' or not isinstance(result.get('data'),dict):
            fail('Codex saved-login listing unconfirmed; no registration made.','unavailable')
        rows=result['data'].get('logins')
        if not isinstance(rows,list) or len(rows)>128:fail('Invalid Codex saved-login listing.','invalid')
        logins=[];seen=set()
        for row in rows:
            if not isinstance(row,dict) or not text(row.get('sessionId')) or not text(row.get('accountId')) or type(row.get('active')) is not bool:
                fail('Invalid Codex saved-login reference.','invalid')
            ref=(row['sessionId'],row['accountId'])
            if ref in seen:fail('Ambiguous Codex saved-login listing.','invalid')
            seen.add(ref)
            # Never publish arbitrary native metadata, identity labels or tokens.
            logins.append({key:row[key] for key in ('sessionId','accountId','active')})
        if sum(row['active'] for row in logins)>1:fail('Ambiguous active Codex login.','invalid')
        self._host(selection)
        return logins

    @staticmethod
    def _account(login,name):
        return validate({'id':'codex-'+uuid.uuid4().hex[:16],'label':name,'runtime':'codex','kind':'subscription',
                         'login_ref':{key:login.get(key) for key in ('sessionId','accountId')}})

    def list(self,args):
        selection=args.get('selection');logins=self._logins(selection)
        rows,revision=snapshot(self.ext)
        self._host(selection)
        existing={credential_identity(row):row for row in rows}
        items=[{'type':'text','text':label('Setup '+selection['setup_id']+' · execution host '+selection['execution_host']+' · native host '+selection['native_host'])},
               {'type':'text','text':'Native IDs only, not verified email/name labels. Type your own account name to register; this does not change the active login.'},
               {'text':'Refresh saved logins','command':'accounts.codex.logins.list','args':{'selection':selection}},
               {'text':'Choose another Codex host','command':'accounts.codex.logins'}]
        for login in logins:
            title=label('sessionId: '+login['sessionId']+' · accountId: '+login['accountId']+' · active: '+str(login['active']).lower(),1024)
            known=existing.get(credential_identity(self._account(login,'saved login')))
            if known:
                items.append({'type':'text','style':'dim','text':title+' · Already registered: '+label(known['label'])})
            else:
                items.append({'text':title,'input':'Name for this saved login (1–128 printable characters)',
                              'command':'accounts.codex.logins.add','args':{'selection':selection,'login':login,'_account_revision':revision}})
        if not logins:items.append({'type':'text','text':'No saved logins reported. Sign in natively, then choose Refresh saved logins.'})
        return self._publish(items)

    def add(self,args):
        selected=args.get('login')
        if not isinstance(selected,dict) or set(selected)!={'sessionId','accountId','active'} or type(selected.get('active')) is not bool:
            fail('Choose a saved login from the picker.','invalid')
        account=self._account(selected,args.get('text'))
        expected=args.get('_account_revision')
        if type(expected) is not int or expected<0:fail('Invalid account registry revision.','invalid')
        rows,revision=snapshot(self.ext)
        if revision!=expected:fail('Account registry changed; refresh saved logins.')
        if any(credential_identity(row)==credential_identity(account) for row in rows):
            fail('This Codex saved login is already registered.','conflict')
        selection=args.get('selection')
        if selected not in self._logins(selection):fail('Saved login changed or disappeared; refresh saved logins.')
        # The canonical registry writer checks the original revision again and
        # uses one CAS transaction. No native credential effect follows discovery.
        return self.register(account,expected=expected,before_save=lambda:self._host(selection))
