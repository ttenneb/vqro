"""Step-one agent right-click: mouse-only with existing live context; no turn/queue inference."""
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

    def menu(self):
        context = {'view': 'agents-r2', 'live': self.r.get('state') == 'live'}
        return [value for _, value in sorted(self.agents.declarations().items(), key=lambda e: e[1].get('order', 0))
                if value.get('where') == 'pane' and all(context.get(k) == v for k, v in value.get('when', {}).items())]

    def test_order_group_mouse_only_and_answer_is_never_offered(self):
        menu = self.menu()
        self.assertEqual([m['label'] for m in menu], ['Stop agent…', 'Agent settings…', 'Pane options…'])
        self.assertTrue(all(m.get('group') == 'agent' for m in menu))
        self.assertTrue(all('input' not in m and m.get('args') == {'resource': 'r2'} for m in menu))
        self.assertTrue(all(set(m['when']) == {'view', 'live'} for m in menu))
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

    def test_abort_and_clear_queue_wait_for_the_followup_even_when_native_facts_exist(self):
        for state in ('running', 'idle', 'waiting', 'unknown', None):
            self.r['data']['agent']['state'] = state
            commands = [m['command'] for m in self.menu()]
            self.assertNotIn('agents.abort', commands)
            self.assertNotIn('agents.clear_queue', commands)

    def test_existing_live_context_gates_stop_settings_options_and_forget(self):
        self.r['available'] = False
        self.r['state'] = 'finished'
        self.assertEqual([m['label'] for m in self.menu()], ['Forget agent'])
        self.r['state'] = 'live'
        self.assertNotIn('Forget agent', [m['label'] for m in self.menu()], 'step one uses existing live context only')

    def test_state_changes_do_not_write_workspace_menu_transactions(self):
        self.agents.event('resource', {'id': 'r2'})
        before = self.agents.declarations()
        self.core.calls.clear()
        self.r['data']['agent']['state'] = 'idle'
        self.r['data']['agent']['queue'] = []
        self.agents.event('resource', {'id': 'r2'})
        self.assertEqual(before, self.agents.declarations())
        self.assertFalse(any(m == 'workspace.apply' for m, _ in self.core.calls))
        menus = {k: v for k, v in self.agents.declarations().items() if k.startswith('menu/agents.r2.')}
        self.assertFalse(any('input' in m for m in menus.values()))
        self.assertTrue(all(k.rsplit('.', 1)[-1] not in ('send', 'prompt', 'steer', 'follow_up', 'answer',
                                                      'abort', 'clear_queue') for k in menus))
