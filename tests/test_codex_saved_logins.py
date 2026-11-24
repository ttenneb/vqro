"""Saved-login registration uses public primitives and synthetic native replies only."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from accounts import Accounts
from account_credentials import SETTINGS, snapshot
from agent_fixture import FakeCore
from vqro_ext import VqroError
from public_provider_error import provider_error

VIEW = 'accounts-codex-logins'


def managed(ident='codex-a'):
    return {'id':ident,'host':'execution-a','kind':'agent','state':'live','available':True,
            'status':{'app':'codex'},'data':{'agent':{'v':0,'lifecycle':'live','epoch':'epoch-a','host':{'id':'native-a'},
            'account_setup':{'id':'shared'},'caps':{'rev':3,'agent.command':{}},
            'commands':[{'name':'_account.list'}]}}}


class NativeCore(FakeCore):
    def __init__(self):
        super().__init__('accounts')
        self.resources['codex-a']=managed()
        self.logins=[{'sessionId':'saved-a','accountId':'workspace-a','active':True},
                     {'sessionId':'saved-b','accountId':'workspace-b','active':False}]
        self.native_hook=None;self.outcome='observed';self.fail=False
    def call(self,method,args):
        if method=='resource.invoke':
            self.calls.append((method,copy.deepcopy(args)))
            assert args['op']=='agent.command' and args['args']['name']=='_account.list', 'no credential/input effects'
            if self.native_hook:self.native_hook()
            if self.fail:provider_error({'kind':'backend','outcome':'unknown','detail':'DO-NOT-ECHO'},message='DO-NOT-ECHO')
            return {'outcome':self.outcome,'data':{'logins':copy.deepcopy(self.logins)}}
        return super().call(method,args)


class SavedLoginTests(unittest.TestCase):
    def setUp(self):
        self.core=NativeCore();self.app=Accounts(self.core)
    def invoke(self,name,args=None):return self.core.handlers[name](args or {})
    def selection(self):
        self.invoke('accounts.codex.logins')
        item=next(i for i in self.core.live[VIEW]['items'] if i.get('command')=='accounts.codex.logins.list')
        return copy.deepcopy(item['args'])
    def choice(self,index=1):
        self.invoke('accounts.codex.logins.list',self.selection())
        return copy.deepcopy([i['args'] for i in self.core.live[VIEW]['items'] if i.get('command')=='accounts.codex.logins.add'][index])
    def save(self,args):return self.invoke('accounts.codex.logins.add',dict(args,text='My second account'))
    def test_accounts_has_add_and_explicit_refresh_actions(self):
        self.app.show({})
        texts=[i.get('text') for i in self.core.live['accounts']['items']]
        self.assertIn('Add Codex saved login…',texts);self.assertIn('Refresh saved logins',texts)
    def test_host_choice_is_explicit_and_scoped_before_listing(self):
        self.core.resources['codex-b']=managed('codex-b')
        self.core.resources['other']=dict(managed('other'),status={'app':'pi'})
        self.invoke('accounts.codex.logins')
        items=[i for i in self.core.live[VIEW]['items'] if i.get('command')=='accounts.codex.logins.list']
        self.assertEqual(len(items),2);self.assertIn('shared',items[0]['text']);self.assertIn('execution-a',items[0]['text'])
        self.assertFalse(any(m=='resource.invoke' for m,_ in self.core.calls))
    def test_registers_human_label_and_exact_reference_in_one_transaction(self):
        with patch.object(Path,'open',side_effect=AssertionError('never inspect credentials')):
            args=self.choice();before=len(self.core.transactions);self.save(args)
        rows,_=snapshot(self.core);self.assertEqual(len(self.core.transactions),before+1)
        self.assertEqual(rows[0]['label'],'My second account');self.assertTrue(rows[0]['id'].startswith('codex-'))
        self.assertEqual(rows[0]['login_ref'],{'sessionId':'saved-b','accountId':'workspace-b'})
        self.assertEqual(set(rows[0]),{'id','label','runtime','kind','login_ref'})
        self.assertEqual(len([1 for m,_ in self.core.calls if m=='resource.invoke']),2)
    def test_listing_shows_only_ids_active_and_context_not_native_extra_fields(self):
        self.core.logins[0].update(email='not-approved@example.invalid',accessToken='DO-NOT-DISPLAY')
        self.choice();content=str(self.core.live[VIEW])
        for value in ('saved-a','workspace-a','active','shared','native-a'):self.assertIn(value,content)
        self.assertNotIn('DO-NOT-DISPLAY',content);self.assertNotIn('not-approved@',content)
    def test_native_failure_is_not_retried_or_registered(self):
        args=self.selection();self.core.fail=True
        with self.assertRaises(VqroError) as raised:self.invoke('accounts.codex.logins.list',args)
        self.assertNotIn('DO-NOT-ECHO',str(raised.exception));self.assertNotIn('DO-NOT-ECHO',str(self.core.live))
        self.assertEqual(len([1 for m,_ in self.core.calls if m=='resource.invoke']),1)
        self.assertEqual(snapshot(self.core)[0],[])
    def test_nonobserved_listing_cannot_register(self):
        args=self.selection();self.core.outcome='accepted'
        with self.assertRaises(VqroError):self.invoke('accounts.codex.logins.list',args)
        self.assertEqual(snapshot(self.core)[0],[])
    def test_malformed_native_rows_cannot_be_used(self):
        for bad in (None,{},[{'sessionId':'s','accountId':'a','active':1}],
                    [{'sessionId':'s\n','accountId':'a','active':True}],
                    [{'sessionId':'s','accountId':'a','active':False}]*2):
            with self.subTest(bad=bad):
                args=self.selection();self.core.logins=bad
                with self.assertRaises(VqroError):self.invoke('accounts.codex.logins.list',args)
                self.assertEqual(snapshot(self.core)[0],[])
    def test_unknown_host_no_native_read(self):
        args=self.selection();args['selection']['resource']='absent'
        with self.assertRaises(VqroError):self.invoke('accounts.codex.logins.list',args)
        self.assertFalse(any(m=='resource.invoke' for m,_ in self.core.calls))
    def test_changed_host_or_scope_after_listing_cannot_save(self):
        for field in ('epoch','native_host','setup','execution','caps','incomplete','incarnation','command'):
            with self.subTest(field=field):
                self.setUp();args=self.choice();row=self.core.resources['codex-a'];a=row['data']['agent']
                if field=='epoch':a['epoch']='new-epoch'
                elif field=='native_host':a['host']['id']='new-native'
                elif field=='setup':a['account_setup']['id']='other-setup'
                elif field=='execution':row['host']='other-execution'
                elif field=='caps':a['caps']['rev']+=1
                elif field=='incomplete':a['incomplete']=['account_setup']
                elif field=='incarnation':row['incarnation']='replacement'
                else:a['commands']=[]
                before=len(self.core.calls)
                with self.assertRaises(VqroError):self.save(args)
                self.assertFalse(any(m=='resource.invoke' for m,_ in self.core.calls[before:]));self.assertEqual(snapshot(self.core)[0],[])
    def test_changed_resource_during_native_read_is_rejected(self):
        args=self.selection();self.core.native_hook=lambda:self.core.resources['codex-a']['data']['agent'].update(epoch='new')
        with self.assertRaises(VqroError):self.invoke('accounts.codex.logins.list',args)
        self.assertEqual(snapshot(self.core)[0],[])
    def test_removed_or_changed_native_choice_is_rejected_at_save(self):
        for change in ('removed','active'):
            with self.subTest(change=change):
                self.setUp();args=self.choice()
                if change=='removed':self.core.logins.pop()
                else:self.core.logins[1]['active']=True;self.core.logins[0]['active']=False
                with self.assertRaises(VqroError):self.save(args)
                self.assertEqual(snapshot(self.core)[0],[])
    def test_registry_change_while_listing_or_before_save_is_rejected(self):
        for during in (False,True):
            with self.subTest(during=during):
                self.setUp();args=self.choice()
                def change():self.core.apply([{'op':'set','key':SETTINGS,'value':{'accounts':[]}}],'external edit')
                if during:self.core.native_hook=change
                else:change()
                with self.assertRaises(VqroError):self.save(args)
                self.assertEqual(snapshot(self.core)[0],[])
    def test_duplicate_reference_cannot_be_registered_twice(self):
        args=self.choice();self.save(args)
        self.invoke('accounts.codex.logins.list',self.selection())
        items=self.core.live[VIEW]['items']
        self.assertTrue(any('Already registered' in i.get('text','') for i in items))
        self.assertEqual(len([i for i in items if i.get('command')=='accounts.codex.logins.add']),1)
    def test_bad_human_name_cannot_register(self):
        args=self.choice()
        for text in ('','\n','x'*129):
            with self.subTest(text=text),self.assertRaises(VqroError):self.invoke('accounts.codex.logins.add',dict(args,text=text))
        self.assertEqual(snapshot(self.core)[0],[])
    def test_host_change_during_final_registry_read_is_rejected(self):
        args=self.choice();original=self.core.call
        def hook(method,params):
            result=original(method,params)
            native_reads=sum(m=='resource.invoke' for m,_ in self.core.calls)
            if native_reads==2 and method=='workspace.get' and params['key']==SETTINGS:
                self.core.resources['codex-a']['data']['agent']['epoch']='replacement'
            return result
        self.core.call=hook
        with self.assertRaises(VqroError):self.save(args)
        self.assertEqual(snapshot(self.core)[0],[])
    def test_registry_transaction_conflict_never_replays_discovery_or_save(self):
        args=self.choice();original=self.core.apply
        def conflict(ops,note):
            if note=='accounts: add metadata':
                original([{'op':'set','key':SETTINGS,'value':{'accounts':[]}}],'competing edit')
            return original(ops,note)
        self.core.apply=conflict
        with self.assertRaises(VqroError):self.save(args)
        self.assertEqual(snapshot(self.core)[0],[])
        self.assertEqual(sum(m=='resource.invoke' for m,_ in self.core.calls),2)
    def test_empty_listing_and_no_managed_host_are_explicit_not_fallbacks(self):
        self.core.logins=[];self.invoke('accounts.codex.logins.list',self.selection())
        self.assertIn('No saved logins reported',str(self.core.live[VIEW]))
        self.core.resources={};before=len(self.core.calls);self.invoke('accounts.codex.logins')
        self.assertIn('No available managed Codex host',str(self.core.live[VIEW]))
        self.assertFalse(any(m=='resource.invoke' for m,_ in self.core.calls[before:]))
    def test_untyped_foreign_resource_metadata_is_not_a_picker_host(self):
        for field in ('caps','host','account_setup'):
            with self.subTest(field=field):
                self.core.resources['codex-a']['data']['agent'][field]='malformed'
                self.invoke('accounts.codex.logins')
                self.assertIn('No available managed Codex host',str(self.core.live[VIEW]))
                self.core.resources['codex-a']=managed()
    def test_explicit_refresh_after_signin_no_discovery_or_credential_switch(self):
        result=self.app.sign_in({'account':'normal','runtime':'codex'})
        self.assertTrue(any('external Codex' in n for n,_ in self.core.notices))
        self.assertTrue(any('Refresh saved logins' in n for n,_ in self.core.notices))
        self.assertFalse(any(m=='resource.invoke' for m,_ in self.core.calls))
        spawn=next(a for m,a in self.core.calls if m=='resource.spawn');self.assertEqual(spawn['argv'],['codex','login'])
        self.invoke('accounts.codex.logins.refresh');self.assertIn(VIEW,self.core.live)
        self.assertIn('resource',result)


if __name__=='__main__':unittest.main()
