"""Small routing UI changes; actual permission/native fences live in coord/native tests."""
import unittest
import test_agents as fixtures
from coordination_requests import REQUEST_ROUTES, request_key
from vqro_ext import VqroError


class AgentRequestRoutingTests(unittest.TestCase):
    def fixture(self):
        f = fixtures.AgentV0Tests(); f.setUp()
        r = f.core.resources['r2']
        q = r['data']['agent']['requests'][0]
        q.update(scope={'session': 's1'}, status='pending')
        r['data']['agent']['requests'] = [q]
        return f, r

    def waiting(self, f, r):
        return [row for row in f.agents.bottom_rows(r) if row.get('id') == 'waiting']

    def test_coordinator_only_both_and_missing_publisher_fallback(self):
        f, r = self.fixture(); key = REQUEST_ROUTES + r['id']
        f.core.entries[key] = {'version': 3, 'value': {'route': 'coordinator'}}
        self.assertIn('fallback', self.waiting(f, r)[0]['description'])
        projection = {'version': 3, 'request_keys': [request_key(r, r['data']['agent']['requests'][0])], 'human': False}
        f.core.live['coord-requests-r2'] = projection
        routed = self.waiting(f, r)
        self.assertEqual(len(routed), 1)
        self.assertIn('is waiting for', routed[0]['text'])
        self.assertNotIn('command', routed[0])
        f.core.entries[key]['value']['route'] = 'both'; projection['human'] = True
        self.assertEqual(len(self.waiting(f, r)), 1)
        f.core.live.clear()  # core clears a disconnected publisher's derived data
        self.assertIn('fallback', self.waiting(f, r)[0]['description'])

    def test_absent_coord_commands_use_raw_fenced_response_for_recovery(self):
        f, r = self.fixture(); original = f.core.call
        f.core.call = lambda method, args: {'commands': []} if method == 'command.list' else original(method, args)
        f.command('answer', resource='r2', request='q1', epoch='e7', revision=2, value=True)
        self.assertFalse(any(method == 'coord.control' for method, _ in f.core.calls))
        native = f.native()[-1][1]
        self.assertEqual(native['op'], 'agent.respond')
        self.assertEqual((native['args']['epoch'], native['args']['revision']), ('e7', 2))

    def test_registered_coord_unsupported_error_never_falls_back(self):
        f, r = self.fixture()
        f.core.invoke = lambda *_: (_ for _ in ()).throw(VqroError('registered handler error', 'unsupported'))
        with self.assertRaises(VqroError):
            f.command('answer', resource='r2', request='q1', epoch='e7', revision=2, value=True)
        self.assertEqual(f.native(), [])

    def test_changed_native_question_never_uses_stale_hidden_notice_projection(self):
        f, r = self.fixture(); key = REQUEST_ROUTES + r['id']
        f.core.entries[key] = {'version': 1, 'value': {'route': 'coordinator'}}
        f.core.live['coord-requests-r2'] = {'version': 1, 'request_keys': ['old'], 'human': False}
        self.assertEqual(len(self.waiting(f, r)), 1)

    def test_answer_delegates_once_and_does_not_fall_back_when_coord_unavailable(self):
        f, r = self.fixture()
        f.command('answer', resource='r2', request='q1', epoch='e7', revision=2, value=True)
        calls = [args for method, args in f.core.calls if method == 'coord.control']
        self.assertEqual(len(calls), 1)
        self.assertEqual((calls[0]['action'], calls[0]['target'], calls[0]['args']['revision']), ('respond', 'r2', 2))
        count = len(f.native())
        def unavailable(*_):
            raise VqroError('coord unavailable', 'unavailable')
        f.core.invoke = unavailable
        with self.assertRaises(VqroError):
            f.command('answer', resource='r2', request='q1', epoch='e7', revision=2, value=True)
        self.assertEqual(len(f.native()), count)
