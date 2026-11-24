"""Core-recorded OFFER authority and durable AUTO budgets; no native effects."""
import unittest
from test_account_cutover_claims import Core, member, source
from accounts_cutover import Cutovers
from accounts_policy import POLICY, validate_settings
from vqro_ext import VqroError


class CutoverAuthority(unittest.TestCase):
    def fixture(self, maximum=3):
        core = Core()
        core.entries[POLICY] = {'version': 1, 'value': {'blocked': 'auto_restart', 'auto_cutover_max_per_hour': maximum, 'continuation_text': 'One NEW turn.'}}
        core.entries['doc/settings/accounts']={'version':7,'value':{'accounts':[
            {'id':value,'label':value.upper(),'runtime':'codex','kind':'subscription',
             'login_ref':{'sessionId':'login-'+value,'accountId':'account-'+value}} for value in ('a','b')]}}
        now = [10000.0]
        return core, Cutovers(core, clock=lambda: now[0]), now

    def claim(self, journal, episode='turn-1', automatic=True, epoch='epoch-1'):
        row = source(); row['data']['agent']['epoch'] = epoch
        affected = dict(member(), epoch=epoch)
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1',
                'native_episode': episode, 'affected': [affected]}
        target = {'id': 'b', 'label': 'B', 'runtime': 'codex', 'kind': 'subscription'}
        return journal.claim(row, target, 7, plan, automatic, 'One NEW turn.')

    def complete(self, journal, token):
        journal.native_result(token, observed=True)
        journal.claim_continuation(token, member())
        journal.continuation_result(token, member(), observed=True)
        journal.complete(token)

    def group_claim(self, journal, leader, sessions, episode):
        row=source();row['id']='r'+leader;row['data']['agent']['session']['id']='session-'+leader
        affected=[dict(member(),resource='r'+value,session='session-'+value) for value in sessions]
        plan={'setup_id':'shared-setup','membership_revision':'members-1','native_episode':episode,'affected':affected}
        target={'id':'b','label':'B','runtime':'codex','kind':'subscription'}
        return journal.claim(row,target,7,plan,True,'One NEW turn.'),row

    def finish_group(self,journal,token):
        journal.native_result(token,observed=True)
        record=journal.lookup(token)
        for item in record['affected']:
            if item['session']!=record['session']:continue
            journal.claim_continuation(token,item)
            journal.continuation_result(token,item,observed=True)
        journal.complete(token)

    def test_alternating_group_leaders_cannot_bypass_affected_agents_cap(self):
        _,journal,_=self.fixture(maximum=1)
        token,_=self.group_claim(journal,'1',['1','2'],'turn-1')
        self.finish_group(journal,token)
        with self.assertRaises(VqroError) as caught:self.group_claim(journal,'2',['2'],'turn-2')
        self.assertEqual(caught.exception.kind,'offer_required')

    def test_fresh_leader_cannot_move_an_exhausted_sibling(self):
        core,journal,_=self.fixture(maximum=2)
        token,_=self.group_claim(journal,'2',['2'],'turn-1');self.finish_group(journal,token)
        token,row=self.group_claim(journal,'1',['1','2'],'turn-2')
        core.entries[POLICY]['value']['auto_cutover_max_per_hour']=1
        with self.assertRaises(VqroError):journal.validate(token,row,'turn-2')
        journal.native_result(token,observed=True)
        self.assertFalse(journal.continuation_allowed(token))
        journal.cancel_continuations(token)
        with self.assertRaises(VqroError):self.group_claim(journal,'1',['1','2'],'turn-3')

    def test_setting_is_integer_zero_through_ten_default_three(self):
        self.assertEqual(validate_settings({})['auto_cutover_max_per_hour'], 3)
        for count in (0, 10): self.assertEqual(validate_settings({'auto_cutover_max_per_hour': count})['auto_cutover_max_per_hour'], count)
        for invalid in (True, -1, 11, 3.0, '3'):
            with self.subTest(invalid=invalid), self.assertRaises(VqroError):
                validate_settings({'auto_cutover_max_per_hour': invalid})

    def test_offer_records_only_authenticated_effective_owner_principal(self):
        core, journal, _ = self.fixture()
        token = self.claim(journal, automatic=False)
        self.assertEqual(journal.lookup(token)['authorization'],
                         {'mode': 'offer', 'principal': 'owner', 'owner_revision': 1})
        other, denied, _ = self.fixture()
        other.caller_record = {'principal': 'agent', 'initiator': 'owner'}
        with self.assertRaises(VqroError): self.claim(denied, automatic=False)
        self.assertIsNone(other.op_entry)
        other.caller_record = None
        with self.assertRaises(VqroError): self.claim(denied, automatic=False)
        self.assertIsNone(other.op_entry)

    def test_background_warning_is_protected_owner_offer_only(self):
        core, journal, _ = self.fixture()
        target={'id':'b','label':'B','runtime':'claude','kind':'subscription','host':'synthetic-host','secret_ref':'b'*32}
        core.entries['doc/settings/accounts']['value']['accounts'][1]=target
        plan={'setup_id':'shared-setup','membership_revision':'members-1','native_episode':'turn-1','affected':[member()]}
        token=journal.claim(source(),target,7,plan,False,'One NEW turn.',background_loss_disclosed=True)
        self.assertEqual(journal.lookup(token)['authorization'],
                         {'mode':'offer','principal':'owner','owner_revision':1,'background_loss_disclosed':True})
        with self.assertRaises(VqroError):journal.authorization(True,background_loss_disclosed=True)
        other, denied, _ = self.fixture();other.caller_record={'principal':'agent','initiator':'owner'}
        with self.assertRaises(VqroError):
            denied.claim(source(),target,7,plan,False,'One NEW turn.',background_loss_disclosed=True)
        self.assertIsNone(other.op_entry)

    def test_automatic_claim_has_no_fabricated_human_principal(self):
        core, journal, _ = self.fixture()
        core.caller_record = None
        token = self.claim(journal)
        self.assertEqual(journal.lookup(token)['authorization'],
                         {'mode': 'auto', 'principal': None, 'owner_revision': None})

    def test_zero_cap_falls_back_before_any_durable_intent(self):
        core, journal, _ = self.fixture(0)
        with self.assertRaises(VqroError) as caught: self.claim(journal)
        self.assertEqual(caught.exception.kind, 'offer_required')
        self.assertIsNone(core.op_entry)
        token = self.claim(journal, automatic=False)
        self.assertEqual(journal.lookup(token)['authorization']['mode'], 'offer')

    def test_hourly_cap_is_per_native_conversation_across_epochs(self):
        core, journal, now = self.fixture()
        for index in range(3):
            token = self.claim(journal, episode='turn-' + str(index), epoch='epoch-' + str(index))
            self.complete(journal, token)
        before = core.op_entry['version']
        with self.assertRaises(VqroError) as caught:
            self.claim(journal, episode='turn-4', epoch='epoch-4')
        self.assertEqual(caught.exception.kind, 'offer_required')
        self.assertEqual(core.op_entry['version'], before)
        now[0] += 3601
        self.assertIsInstance(self.claim(journal, episode='turn-4', epoch='epoch-4'), str)

    def test_postclaim_check_rejects_owner_revision_and_native_episode_changes(self):
        core, journal, _ = self.fixture()
        token = self.claim(journal, automatic=False)
        journal.validate(token, source(), 'turn-1')
        with self.assertRaises(VqroError): journal.validate(token, source(), 'different-native-turn')
        core.entries['doc/principals/owner']['version'] = 2
        with self.assertRaises(VqroError): journal.validate(token, source(), 'turn-1')

    def test_postclaim_check_observes_lowered_rolling_hour_budget(self):
        core, journal, _ = self.fixture()
        first = self.claim(journal, episode='turn-1')
        self.complete(journal, first)
        second = self.claim(journal, episode='turn-2')
        core.entries[POLICY]['value']['auto_cutover_max_per_hour'] = 1
        with self.assertRaises(VqroError): journal.validate(second, source(), 'turn-2')

    def test_native_episode_dedup_survives_adapter_epoch_change(self):
        _, journal, _ = self.fixture()
        token = self.claim(journal)
        self.complete(journal, token)
        with self.assertRaises(VqroError): self.claim(journal, epoch='restarted-epoch')


if __name__ == '__main__': unittest.main()
