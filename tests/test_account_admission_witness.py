"""Common AUTO consumes the native included-usage caps snapshot, never latest caps."""
import unittest
import test_account_cutover_dispatch as fixture
from vqro_ext import VqroError
from public_provider_error import provider_error


class AdmissionWitness(unittest.TestCase):
    def setUp(self):
        self.fixture=fixture.CutoverDispatch();self.fixture.setUp();self.addCleanup(self.fixture.doCleanups)
        self.core=self.fixture.core;self.owner=self.fixture.owner;self.dispatch=self.fixture.dispatch

    def missing(self,kind):
        invoke=self.core.invoke
        def changed(name,args):
            result=invoke(name,args)
            if name=='accounts.cutover.codex':
                member=result['affected'][0]
                if kind=='missing':member.pop('admission_caps_rev')
                elif kind=='expired':member['admission_expires_at_ms']=fixture.NOW-1
                elif kind=='revision':member['admission_caps_rev']=1
                elif kind=='boolean':member['admission_caps_rev']=True
                elif kind=='permission':
                    self.core.resources['r1']['data']['agent']['caps']['agent.send'].pop('native_included_usage')
            return result
        self.core.invoke=changed
        result=self.fixture.execute();token,record=next(iter(self.core.op_entry['value']['claims'].items()))
        self.assertEqual(self.core.sends,[])
        self.assertEqual(result.get('disposition'),'offer');self.assertFalse(result['continuation_allowed'])
        self.assertEqual(record['state'],'awaiting_owner');self.assertEqual(record['continuations'],{})
        self.assertEqual(self.core.effect_count,1)
        self.assertEqual(self.core.op_entry['value']['groups'],{});self.assertEqual(self.owner.offered,token)

    def test_missing_witness_offers_without_auto_input(self):self.missing('missing')
    def test_expired_witness_offers_without_auto_input(self):self.missing('expired')
    def test_newer_generic_caps_cannot_replace_witness(self):self.missing('revision')
    def test_boolean_is_not_a_caps_revision(self):self.missing('boolean')
    def test_absent_positive_native_capability_offers(self):self.missing('permission')

    def test_positive_witness_is_the_submitted_revision(self):
        self.fixture.execute();self.assertEqual(len(self.core.sends),1)
        self.assertEqual(self.core.sends[0]['args']['caps_rev'],2)

    def test_late_prewrite_guard_reject_offers_once_without_replaying_auto(self):
        claim=self.dispatch.journal.claim_continuation;call=self.core.call;attempts=[]
        def changed(token,old,**kwargs):
            delivery=claim(token,old,**kwargs)
            caps=self.core.resources['r1']['data']['agent']['caps']
            caps['rev']=3;caps['agent.send'].pop('native_included_usage',None)
            return delivery
        def native(method,params=None):
            if method=='resource.invoke' and params.get('op')=='agent.send':
                attempts.append(params)
                if params['args']['caps_rev']!=self.core.resources['r1']['data']['agent']['caps']['rev']:
                    return provider_error({'outcome':'rejected','kind':'stale'},method,params,
                                          message='capability revision changed')
            return call(method,params)
        self.dispatch.journal.claim_continuation=changed;self.core.call=native
        self.fixture.execute();token,record=next(iter(self.core.op_entry['value']['claims'].items()))
        self.assertEqual(len(attempts),1);self.assertEqual(attempts[0]['args']['caps_rev'],2)
        self.assertEqual(self.core.sends,[]);self.assertEqual(record['state'],'awaiting_owner')
        child=record['continuations']['session-1'];rejected=child['delivery']
        self.assertEqual(child['state'],'cancelled');self.assertEqual(child['rejected_delivery'],rejected)
        self.dispatch.confirm_continuation(token)
        self.assertEqual(len(attempts),2);self.assertEqual(len(self.core.sends),1)
        self.assertEqual(self.core.sends[0]['args']['caps_rev'],3)  # explicit owner, not AUTO
        self.assertNotEqual(self.core.sends[0]['args']['delivery'],rejected)
        child=self.core.op_entry['value']['claims'][token]['continuations']['session-1']
        self.assertEqual(child['rejected_delivery'],rejected);self.assertEqual(child['state'],'observed')
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.effect_count,1)

    def test_actual_sdk_preserves_authoritative_rejection_data(self):
        with self.assertRaises(VqroError) as caught:
            provider_error({'kind':'stale','outcome':'rejected'})
        error=caught.exception
        self.assertEqual(error.kind,'stale');self.assertEqual(error.data['outcome'],'rejected')
        self.assertIs(error.data,error.wire_error['data'])

    def test_kind_alone_or_uncertain_malformed_and_raw_replies_never_rearm(self):
        variants=(None,{'kind':'stale','outcome':'indeterminate'},
                  {'kind':'timeout','outcome':'rejected'},['rejected'],
                  {'kind':'stale'},'raw-rejected','mismatched-kind')
        for evidence in variants:
            with self.subTest(evidence=evidence):
                case=fixture.CutoverDispatch();case.setUp();self.addCleanup(case.doCleanups)
                call=case.core.call
                def rejected(method,params=None):
                    if method=='resource.invoke' and params.get('op')=='agent.send':
                        if evidence is None:raise VqroError('stale without outcome','stale')
                        if evidence=='raw-rejected':return {'outcome':'rejected','error':{'kind':'stale'}}
                        if evidence=='mismatched-kind':
                            raise VqroError('contradictory error','unavailable',data={'outcome':'rejected','kind':'stale'})
                        return provider_error(evidence,method,params)
                    return call(method,params)
                case.core.call=rejected
                with self.assertRaises(VqroError):case.execute()
                token,record=next(iter(case.core.op_entry['value']['claims'].items()))
                self.assertEqual(record['continuations']['session-1']['state'],'unknown')
                self.assertIn(token,case.core.op_entry['value']['groups'].values())
                self.assertFalse(hasattr(case.owner,'offered'))
                with self.assertRaises(VqroError):case.dispatch.confirm_continuation(token)

    def test_manual_original_offer_can_authorize_its_explicit_new_turn(self):
        self.fixture.nonready('offer');self.fixture.execute(automatic=False)
        self.dispatch.confirm_continuation(self.owner.offered)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.effect_count,1)

    def test_notify_revokes_the_pending_owner_continuation(self):
        self.missing('missing')
        self.core.entries[fixture.policy.POLICY]['value']['blocked']='notify'
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(self.owner.offered)
        self.assertEqual(self.core.sends,[])

    def test_native_abort_ack_before_first_commit_refunds_definite_zero(self):
        invoke=self.core.invoke
        def rejected(name,args):
            if name=='accounts.cutover.codex':
                return {'outcome':'rejected','attempted_targets':[],'fenced':False,
                        'continuation_allowed':False,'reason':'offer_required'}
            return invoke(name,args)
        self.core.invoke=rejected;self.fixture.execute()
        token,record=next(iter(self.core.op_entry['value']['claims'].items()))
        self.assertEqual(record['state'],'rejected');self.assertEqual(record['attempted_targets'],[])
        self.assertEqual(self.core.op_entry['value']['groups'],{})
        self.assertEqual(self.dispatch.journal.remaining('codex',record['affected']),3)
        self.assertEqual(self.core.sends,[]);self.assertEqual(self.core.effect_count,0)
        self.assertFalse(hasattr(self.owner,'offered'))  # no switched destination to continue

    def test_unconfirmed_abort_retains_full_unknown_reservation(self):
        invoke=self.core.invoke
        def rejected(name,args):
            if name=='accounts.cutover.codex':
                return {'outcome':'rejected','attempted_targets':[],'fenced':True,'continuation_allowed':False}
            return invoke(name,args)
        self.core.invoke=rejected;self.fixture.execute()
        token,record=next(iter(self.core.op_entry['value']['claims'].items()))
        self.assertEqual(record['state'],'unknown');self.assertIn(token,self.core.op_entry['value']['groups'].values())
        self.assertEqual(self.dispatch.journal.remaining('codex',record['affected']),2)
        self.assertEqual(self.core.sends,[])

    def test_unknown_submission_never_becomes_an_owner_retry(self):
        call=self.core.call
        def unknown(method,params=None):
            if method=='resource.invoke' and params.get('op')=='agent.send':
                return provider_error({'outcome':'indeterminate','kind':'unavailable'},method,params)
            return call(method,params)
        self.core.call=unknown
        with self.assertRaises(VqroError):self.fixture.execute()
        token,record=next(iter(self.core.op_entry['value']['claims'].items()))
        self.assertEqual(record['continuations']['session-1']['state'],'unknown')
        self.assertIn(token,self.core.op_entry['value']['groups'].values());self.assertFalse(hasattr(self.owner,'offered'))
        with self.assertRaises(VqroError):self.dispatch.journal.await_owner(token,record['resumed_members'])
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
