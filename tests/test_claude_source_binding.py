"""Actual common/Claude source accounting with synthetic facts; no native or credential I/O."""
import copy
import unittest
from unittest.mock import patch
from accounts import Accounts
from account_credentials import SETTINGS, credential_identity
import accounts_policy as policy
from test_account_cutover_claims import Core as JournalCore
from test_accounts_claude import Fake


class Core(JournalCore):
    def invoke(self, name, args=None):
        if name in self.handlers:
            return self.handlers[name](args)
        raise AssertionError(name)


def fixture():
    native=Fake();core=Core()
    source=dict(native.target,id='a',label='A',secret_ref='a'*32)
    target=copy.deepcopy(native.target)
    native.source['data']['agent']['_claude']['credential_binding']={
        'account':'a','host':'backend','ref':'a'*32,'epoch':'e1'}
    core.providers=copy.deepcopy(native.providers)
    core.resources={'r2':native.source,'r1':native.host_resource}
    core.entries[SETTINGS]={'version':7,'value':{'accounts':[source,target]}}
    app=Accounts(core);clock=[1000.0];app.cutovers.clock=lambda:clock[0]
    return core,app,copy.deepcopy(source),clock


class ClaudeSourceBinding(unittest.TestCase):
    @patch('accounts_policy.now_ms',return_value=1_000_000)
    def test_old_child_percentage_never_becomes_capacity_of_a_rebound_reference(self, _clock):
        core,app,old,_=fixture()
        native=core.resources['r2']['data']['agent']
        native['availability']={'state':'available'}
        native['usage_limits']=[{'scope':{'kind':'account','account':'a'},'used_percent':10,
                                'observed_at_ms':1_000_000,'source':'native:fixture'}]
        new=dict(old,secret_ref='c'*32)
        alias=dict(old,id='old-alias',label='Old actual credential')
        identities={row['id']:credential_identity(row) for row in (new,alias)}
        scores=policy.capacities(list(core.resources.values()),[new,alias],credential_identities=identities)
        self.assertIsNone(scores['a'],'old child percentage is not new-ref evidence')
        self.assertEqual(scores['old-alias'],90)

    @patch('accounts_policy.now_ms',return_value=1_000_000)
    def test_old_blocked_child_does_not_block_new_reference_in_new_agent_strategy(self, _clock):
        core,app,old,_=fixture()
        new=dict(old,secret_ref='c'*32)
        core.entries[policy.POLICY]['value']['new_agents']='round_robin'
        chosen=policy.pick_new(core,[new],'claude',credential_identities={'a':credential_identity(new)})
        self.assertEqual(chosen,new)

    @patch('accounts_policy.now_ms',return_value=1_000_000)
    def test_running_child_rebind_keeps_old_reference_block_and_original_clock(self, _clock):
        core,app,old,clock=fixture()
        app.cutovers.observe_limits(list(core.resources.values()),core.value(SETTINGS)['accounts'])
        before=copy.deepcopy(core.op_entry['value']['blocks'])
        new=dict(old,secret_ref='c'*32)
        rows=core.value(SETTINGS)['accounts'];rows[0]=new
        core.apply([{'op':'set','key':SETTINGS,'value':{'accounts':rows}}],'synthetic registry rebind')
        clock[0]+=10
        app.cutovers.observe_limits(list(core.resources.values()),rows)
        self.assertTrue(app.cutovers.known_blocked(old))
        self.assertFalse(app.cutovers.known_blocked(new),'old child limit is not evidence about the new credential')
        self.assertEqual(core.op_entry['value']['blocks'],before,'same old episode never renews clocks')

    @patch('accounts_policy.now_ms',return_value=1_000_000)
    def test_ambiguous_live_registry_rebind_refuses_before_claim_or_native_restart(self, _clock):
        core,app,old,_=fixture()
        rows=core.value(SETTINGS)['accounts'];rows[0]=dict(old,secret_ref='c'*32)
        core.apply([{'op':'set','key':SETTINGS,'value':{'accounts':rows}}],'synthetic registry rebind')
        app.handle_blocked('r2')
        self.assertTrue(app.cutovers.known_blocked(old))
        self.assertFalse(app.cutovers.known_blocked(rows[0]))
        self.assertEqual(core.op_entry['value']['claims'],{})
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))


if __name__=='__main__':unittest.main()
