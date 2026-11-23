"""Mouse-only agent menus use reported live facts, never inferred work or queue state."""
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'extensions'))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from agents import Agents
from agent_fixture import FakeCore


class AgentPaneMenu(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.core.add_agent()
        self.core.providers['other']['capabilities'] = {
            'agent.interrupt': {}, 'agent.queue': {}, 'agent.stop': {}, 'agent.configure': {},
            'agent.command': {}, 'agent.send': {}, 'agent.respond': {}}
        self.r = self.core.resources['r2']
        self.r.pop('host', None)
        self.r['data'] = {'agent': {'v': 0, 'lifecycle': 'live', 'state': 'running', 'epoch': 'e1',
            'caps': {'rev': 1, 'agent.interrupt': {}, 'agent.queue': {'actions': ['list', 'clear']},
                     'agent.stop': {'covers': ['foreground']}, 'agent.configure': {}, 'agent.command': {},
                     'agent.send': {'modes': {'start': {}, 'boundary': {}, 'settle': {}}},
                     'agent.respond': {'methods': ['choice']}},
            'queue': [{'id': 'held', 'mode': 'settle'}],
            'config': [{'id': 'model', 'label': 'Model', 'type': 'choice', 'value': 'a', 'choices': [{'label': 'A', 'value': 'a'}]}],
            'requests': [{'request': 'q', 'method': 'choice', 'status': 'pending'}]}}
        self.agents = Agents(self.core)

    def menu(self, supports_context=True):
        context = {'view': 'agents-r2', 'live': self.r.get('state') == 'live'}
        if supports_context:
            context.update({'menu.' + k: v for k, v in self.agents.panel(self.r)['menu_context'].items()})
        return [value for _, value in sorted(self.agents.declarations().items(), key=lambda e: e[1].get('order', 0))
                if value.get('where') == 'pane' and all(context.get(k) == v for k, v in value.get('when', {}).items())]

    def test_order_group_mouse_only_and_answer_is_never_offered(self):
        menu = self.menu()
        self.assertEqual([m['label'] for m in menu], ['Abort', 'Clear queue', 'Stop agent…', 'Agent settings…', 'Pane options…'])
        self.assertTrue(all(m.get('group') == 'agent' for m in menu))
        self.assertTrue(all('input' not in m and m.get('args') == {'resource': 'r2'} for m in menu))
        self.assertTrue(all(set(m['when']) <= {'view', 'live', 'menu.agent_working', 'menu.agent_queued'} for m in menu))
        self.assertNotIn('confirm', next(m for m in menu if m['command'] == 'agents.stop')['args'])
        before = list(self.core.calls)
        self.core.handlers['agents.stop']({'resource': 'r2'})
        self.assertFalse(any(m in ('resource.invoke', 'resource.kill') for m, _ in self.core.calls[len(before):]),
                         'Stop first opens the existing confirming click popup')
        for command in ('agents.prompt', 'agents.steer', 'agents.follow_up', 'agents.answer', 'agents.send',
                        'agents.abort', 'agents.clear_queue'):
            self.assertIn(command, self.core.handlers, 'right-click removal never removes command-list/key access')
        waiting = next(row for row in self.agents.bottom_rows(self.r) if row.get('id') == 'waiting')
        self.assertEqual(waiting['actions'][0]['command'], 'agents.answer', 'Answer stays as a pane action')
        self.assertNotIn('menu', waiting, 'Answer is never reintroduced by waiting-row right-click')

    def test_only_explicit_work_and_known_nonempty_queue_enable_actions(self):
        facts = self.r['data']['agent']
        for state in ('running', 'idle', 'waiting', 'unknown', None):
            facts['state'] = state
            self.assertEqual('agents.abort' in [m['command'] for m in self.menu()], state == 'running')
        facts['state'] = 'running'
        for incomplete in (['state'], ['lifecycle']):
            facts['incomplete'] = incomplete
            self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_working'])
        facts.pop('incomplete')
        for queue in (None, [], {}, ['invalid'], [{}], [{'mode': ''}]):
            facts['queue'] = queue
            self.assertNotIn('agents.clear_queue', [m['command'] for m in self.menu()])
        facts['queue'] = [{'id': None, 'mode': 'future-mode', 'preview': 'native'}]
        self.assertIn('agents.clear_queue', [m['command'] for m in self.menu()])
        self.r['available'] = False
        self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_working'])
        self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_queued'])

    def test_unknown_or_ended_lifecycle_and_undeclared_capabilities_are_not_inferred(self):
        facts = self.r['data']['agent']
        for lifecycle in (None, 'ended'):
            facts['lifecycle'] = lifecycle
            self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_working'])
        facts['lifecycle'] = 'live'
        self.core.providers.clear()
        flags = self.agents.pane_menu_flags(self.r)
        self.assertFalse(flags['agent_working'])
        self.assertFalse(flags['agent_queued'])

    def test_v0_unknown_facts_do_not_fall_back_to_legacy_fields(self):
        self.r['data']['agent'].pop('state')
        self.r['data']['agent'].pop('queue')
        self.r['status'] = {'state': 'working'}
        self.r['data']['queues'] = {'steering': ['stale legacy value']}
        flags = self.agents.pane_menu_flags(self.r)
        self.assertFalse(flags['agent_working'])
        self.assertFalse(flags['agent_queued'])

    def test_forget_matches_handler_availability_not_resource_state(self):
        for state in ('live', 'finished'):
            self.r['state'] = state
            for available in (True, False):
                self.r['available'] = available
                self.assertEqual('Forget agent' in [m['label'] for m in self.menu()], not available)
        self.r['available'] = None
        self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_forgettable'])

    def test_old_clients_hide_conditional_actions_and_legacy_native_facts_work(self):
        self.assertFalse(any(m['command'] in ('agents.abort', 'agents.clear_queue', 'agents.forget')
                             for m in self.menu(supports_context=False)))
        # The actual legacy Pi profile publishes a v:0 config object, but no v0 interrupt/queue ops.
        self.r['data'] = {'agent': {'v': 0, 'config': []},
                          'queues': {'steering': ['native queued'], 'follow_up': []}}
        self.core.providers['other']['capabilities'] = {'agent.abort': {}, 'agent.clear_queue': {}}
        self.r['status'] = {'state': 'working'}
        self.assertTrue(self.agents.pane_menu_flags(self.r)['agent_working'])
        self.assertTrue(self.agents.pane_menu_flags(self.r)['agent_queued'])
        self.r['status'] = {'state': 'unknown'}
        self.r['data'] = {'queues': {'steering': None}}
        self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_working'])
        self.assertFalse(self.agents.pane_menu_flags(self.r)['agent_queued'])

    def test_state_changes_do_not_write_workspace_menu_transactions(self):
        self.agents.event('resource', {'id': 'r2'})
        before = self.agents.declarations()
        transactions = len(self.core.transactions)
        self.core.calls.clear()
        self.r['data']['agent']['state'] = 'idle'
        self.r['data']['agent']['queue'] = []
        self.agents.event('resource', {'id': 'r2'})
        self.assertEqual(before, self.agents.declarations())
        self.assertFalse(any(m == 'workspace.apply' for m, _ in self.core.calls))
        self.assertEqual(len(self.core.transactions), transactions)
        menus = {k: v for k, v in self.agents.declarations().items() if k.startswith('menu/agents.r2.')}
        self.assertFalse(any('input' in m for m in menus.values()))
        self.assertTrue(all(k.rsplit('.', 1)[-1] not in ('send', 'prompt', 'steer', 'follow_up', 'answer') for k in menus))
        self.assertEqual(self.agents.panel(self.r)['menu_context'],
                         {'agent_working': False, 'agent_queued': False, 'agent_forgettable': False})
