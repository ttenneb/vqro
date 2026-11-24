"""Protected full-plan reservation and definite-prefix settlement, synthetic only."""
import copy
from test_account_known_blocked import KnownBlocked
from test_account_cutover_claims import source,member
from account_credentials import SETTINGS
from vqro_ext import VqroError


class Reservations(KnownBlocked):
    def setUp(self):
        super().setUp()
        for name in ('c','d'):
            self.core.entries[SETTINGS]['value']['accounts'].append(dict(self.b,id=name,label=name,
                login_ref={'sessionId':'saved-'+name,'accountId':'native-'+name}))

    def reserve(self,targets=('b','c','d'),setup='shared',episode='turn-1',affected=None):
        plan={'setup_id':setup,'membership_revision':'members-1','native_episode':episode,
              'affected':affected or [member()],'authorized_targets':list(targets)}
        return self.journal.claim(source(),self.b,7,plan,True,'One NEW turn.')

    def test_definite_predispatch_reject_refunds_full_plan_for_every_member_without_replay(self):
        affected=[member(),member(resource='r2',session='session-2')]
        token=self.reserve(affected=affected)
        for item in affected:self.assertEqual(self.journal.remaining('codex',[item]),0)
        self.journal.reject_before_effect(token)
        record=self.journal.lookup(token)
        self.assertEqual(record['state'],'rejected');self.assertEqual(record['attempted_targets'],[])
        self.assertEqual(record['authorized_targets'],['b','c','d'])
        self.assertEqual(record['native_episode'],'turn-1')
        self.assertEqual(self.journal.read()[0]['groups'],{})
        for item in affected:self.assertEqual(self.journal.remaining('codex',[item]),3)
        with self.assertRaises(VqroError):self.reserve(affected=affected)
        self.assertNotEqual(self.reserve(episode='turn-2',affected=affected),token)

    def test_unknown_multi_member_result_keeps_full_plan_reserved_and_tombstone(self):
        affected=[member(),member(resource='r2',session='session-2')]
        token=self.reserve(affected=affected)
        self.journal.native_result(token,observed=False,attempted_targets=[])
        record=self.journal.lookup(token)
        self.assertEqual(record['state'],'unknown');self.assertIsNone(record['attempted_targets'])
        self.assertIn(token,self.journal.read()[0]['groups'].values())
        for item in affected:self.assertEqual(self.journal.remaining('codex',[item]),0)
        with self.assertRaises(VqroError):self.reserve(episode='turn-2',affected=affected)

    def test_two_claims_cannot_jointly_reserve_more_than_hourly_cap(self):
        self.reserve()
        with self.assertRaises(VqroError) as caught:self.reserve(('b',),setup='another',episode='turn-2')
        self.assertEqual(caught.exception.kind,'offer_required')
        self.assertEqual(len(self.journal.read()[0]['claims']),1)

    def test_unknown_retains_full_reservation_even_with_partial_prefix(self):
        token=self.reserve()
        self.journal.native_result(token,observed=False,attempted_targets=['b'])
        self.assertIsNone(self.journal.lookup(token)['attempted_targets'])
        self.assertEqual(self.journal.remaining('codex',[member()]),0)

    def test_definite_prefix_refunds_unused_slots_and_preserves_original_source(self):
        token=self.reserve()
        self.journal.native_result(token,observed=True,attempted_targets=['b','c'],selected_account='c')
        record=self.journal.lookup(token)
        self.assertEqual(record['account'],'c')
        self.assertEqual(record['source_account'],'a')
        self.assertEqual(record['native_episode'],'turn-1')
        self.assertEqual(self.journal.remaining('codex',[member()]),1)

    def test_missing_multi_target_prefix_is_uncertain_never_a_single_switch_refund(self):
        token=self.reserve()
        with self.assertRaises(VqroError):self.journal.native_result(token,observed=True)
        self.assertEqual(self.journal.lookup(token)['state'],'unknown')
        self.assertEqual(self.journal.remaining('codex',[member()]),0)

    def test_skipped_or_reordered_prefix_cannot_refund_reservation(self):
        token=self.reserve()
        with self.assertRaises(VqroError):self.journal.native_result(token,observed=True,attempted_targets=['c'])
        self.assertEqual(self.journal.remaining('codex',[member()]),0)
