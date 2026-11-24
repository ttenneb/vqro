"""Earlier fenced-preflight zero/unknown through the real callback + common journal."""
import unittest
from unittest.mock import patch
import test_codex_shared_pipeline as fixture
from public_provider_error import provider_error
from vqro_ext import VqroError


class PreflightCommon(unittest.TestCase):
    def setUp(self):
        clock=patch('accounts_policy.now_ms',return_value=fixture.NOW)
        clock.start();self.addCleanup(clock.stop)
        native=patch('codex_cutover.time.time',return_value=fixture.NOW/1000)
        native.start();self.addCleanup(native.stop)

    def check(self,reason,ack):
        core=fixture.Pipeline(True,True);original=core.native.call
        def native(method,args):
            result=original(method,args)
            if method!='resource.invoke' or args.get('op')!='agent.command':return result
            op=args['args']['name']
            if reason=='consent' and op=='_account.fence' and args['resource']=='y':
                core.entries[fixture.CONSENT]['value']={'setups':{}}
            if reason=='listing' and op=='_account.list':
                return provider_error({'kind':'unavailable','outcome':'indeterminate'},method,args,
                                      message='synthetic readonly discovery uncertainty')
            if op=='_account.abort' and args['resource']=='x':
                if ack=='error':
                    return provider_error({'kind':'unavailable','outcome':'indeterminate'},method,args)
                if ack=='missing':result['data'].pop('aborted')
                else:result['data']['aborted']=ack
            return result
        core.native.call=native;core.app.handle_blocked('x')
        value=core.op_entry['value'];self.assertEqual(len(value['claims']),1)
        token,record=next(iter(value['claims'].items()))
        names=[args['args']['name'] for method,args in core.native.calls
               if method=='resource.invoke' and args.get('op')=='agent.command']
        self.assertEqual(names.count('_account.fence'),2);self.assertEqual(names.count('_account.abort'),2)
        for forbidden in ('_account.prepare_switch','_account.switch','_account.resume','_account.restore','_account.release'):
            self.assertNotIn(forbidden,names)
        self.assertEqual(core.sends,[]);self.assertNotIn('accounts-continuation',core.live)
        self.assertEqual(record['authorized_targets'],['b','c'])
        if ack is True:
            self.assertEqual(record['state'],'rejected');self.assertEqual(record['attempted_targets'],[])
            self.assertEqual(value['groups'],{});self.assertEqual(core.app.cutovers.remaining('codex',record['affected']),3)
        else:
            self.assertEqual(record['state'],'unknown');self.assertIn(token,value['groups'].values())
            self.assertEqual(core.app.cutovers.remaining('codex',record['affected']),1)
        calls=len(core.native.calls)
        core.app.handle_blocked('x')
        self.assertEqual(len(core.native.calls),calls);self.assertEqual(len(core.op_entry['value']['claims']),1)

    def test_post_second_fence_consent_off_all_ack_settles_zero(self):self.check('consent',True)
    def test_readonly_listing_unknown_all_ack_settles_zero(self):self.check('listing',True)

    def test_post_second_fence_consent_off_uncertain_ack_keeps_full_reservation(self):
        for ack in (False,1,None,'missing','error'):
            with self.subTest(ack=ack):self.check('consent',ack)

    def test_readonly_listing_unknown_uncertain_ack_keeps_full_reservation(self):
        for ack in (False,1,None,'missing','error'):
            with self.subTest(ack=ack):self.check('listing',ack)
