"""Revoke policy DURING the last owner read, not an earlier claim projection."""
import unittest
import test_accounts_pi as pi_fixture
import test_accounts_claude as claude_fixture
import test_account_cutover_dispatch as dispatch_fixture
import accounts_policy as policy
from vqro_ext import VqroError


def mutate(value,mode):
    if mode is None:value.pop('blocked',None)
    else:value['blocked']=mode


class LastOwnerPolicy(unittest.TestCase):
    def pi(self,mode):
        case=pi_fixture.PiPrepare();case.setUp();case.arm_restart(False)
        call=case.core.call;reads=[]
        def revoke(method,args=None):
            result=call(method,args)
            if method=='workspace.get' and args['key']=='doc/principals/owner':
                reads.append(True)
                if len(reads)==2:mutate(case.core.entries[policy.POLICY]['value'],mode)
            return result
        case.core.call=revoke;failure=None
        try:case.callback.restart(dict(case.args(False),_claim='a'*32))
        except VqroError as error:failure=error
        self.assertEqual(len(reads),2)
        if mode=='auto_restart':
            self.assertIsNone(failure);self.assertEqual(len(case.stops),1);self.assertEqual(len(case.resumes),1)
        else:
            self.assertEqual(case.stops,[]);self.assertEqual(case.resumes,[]);self.assertIsInstance(failure,VqroError)

    def claude(self,mode):
        case=claude_fixture.CutoverTests();case.setUp();case.offer_claim()
        call=case.ext.call;reads=[]
        def revoke(method,args):
            result=call(method,args)
            if method=='workspace.get' and args['key']=='doc/principals/owner':
                reads.append(True)
                if len(reads)==2:mutate(case.ext.policy,mode)
            return result
        case.ext.call=revoke;failure=None
        try:case.run_cutover()
        except VqroError as error:failure=error
        self.assertEqual(len(reads),2)
        if mode=='auto_restart':self.assertIsNone(failure);self.assertEqual(len(case.ext.native()),1)
        else:self.assertEqual(case.ext.native(),[]);self.assertIsInstance(failure,VqroError)

    def continuation(self,mode):
        case=dispatch_fixture.CutoverDispatch();case.setUp();self.addCleanup(case.doCleanups)
        case.nonready('offer');case.execute();token=case.owner.offered
        journal=case.dispatch.journal;claim=journal.claim_continuation;call=case.core.call;armed=[];reads=[]
        def intent(*args,**kwargs):
            delivery=claim(*args,**kwargs);armed.append(True);return delivery
        def revoke(method,args=None):
            result=call(method,args)
            if armed and method=='workspace.get' and args['key']=='doc/principals/owner':
                reads.append(True);mutate(case.core.entries[policy.POLICY]['value'],mode)
            return result
        journal.claim_continuation=intent;case.core.call=revoke;failure=None
        try:case.dispatch.confirm_continuation(token)
        except VqroError as error:failure=error
        self.assertEqual(len(reads),1)
        if mode=='auto_restart':self.assertIsNone(failure);self.assertEqual(len(case.core.sends),1)
        else:self.assertEqual(case.core.sends,[]);self.assertIsInstance(failure,VqroError)

    def test_pi_last_owner_read_policy_revocation(self):
        for mode in ('notify',None,'offer_restart'):
            with self.subTest(mode=mode):self.pi(mode)

    def test_claude_last_owner_read_policy_revocation(self):
        for mode in ('notify',None,'offer_restart'):
            with self.subTest(mode=mode):self.claude(mode)

    def test_shared_continuation_last_owner_read_policy_revocation(self):
        for mode in ('notify',None,'offer_restart'):
            with self.subTest(mode=mode):self.continuation(mode)

    def test_unchanged_owner_and_policy_remain_permitted(self):
        for surface in (self.pi,self.claude,self.continuation):
            with self.subTest(surface=surface.__name__):surface('auto_restart')
