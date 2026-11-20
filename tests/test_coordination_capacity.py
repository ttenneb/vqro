"""Recovery at native-control saturation, through public engine mutations only."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import test_coordination_engine as fixtures
from coordination_policy import PolicyError
from coordination_engine import Coordinator
from coordination_store import MemoryStore


class RecoveryCapacityTests(unittest.TestCase):
    def test_content_flood_leaves_normal_ledger_headroom_for_interrupt(self):
        f = fixtures.EngineTests()
        f.setUp()
        f.resources = {'r' + str(n): fixtures.resource('r' + str(n)) for n in range(1, 201)}
        f.config['doc/agents/grants/senders'] = {'version': 1, 'value': {'who': '*', 'can': ['send:start'], 'on': '*'}}
        refused = False
        for n in range(1, 201):
            op = f.op()
            principal = 'p:bulk' + str(n)
            try:
                f.engine.send({'op': op, 'submission': op, 'to': 'r' + str(n), 'mode': 'start',
                               'content': [{'type': 'text', 'text': 'x' * 30000}]},
                              {'principal': principal, 'actor': principal, 'author': 'client:bulk:' + str(n)})
            except PolicyError as error:
                self.assertIn(error.kind, ('quota', 'busy'))
                refused = True
                break
        self.assertTrue(refused)
        self.assertEqual(f.ledger, [])
        result = f.engine.control({'op': f.op(), 'target': 'r1', 'action': 'interrupt'}, fixtures.OWNER)
        self.assertEqual(result['state'], 'accepted')
        self.assertEqual([(target, operation) for target, operation, _ in f.ledger], [('r1', 'agent.interrupt')])

    def test_saturated_normal_ledger_reports_cached_quota_before_and_after_window_roll(self):
        f = fixtures.EngineTests(); f.setUp()
        f.resources['r2'] = fixtures.resource('r2')
        f.config['doc/agents/grants/senders'] = {'version': 1, 'value': {'who': '*', 'can': ['send:start'], 'on': '*'}}
        for resource in f.resources.values():
            resource['data']['agent']['caps']['agent.command'] = {}
        f.engine.invoke = lambda target, operation, args, deadline: f.ledger.append((target, operation, args)) or {'outcome': 'indeterminate'}
        for size in (180000, 30000, 4000, 0):
            for _ in range(256):
                try:
                    f.engine.control({'op': f.op(), 'target': 'r2', 'action': 'command',
                                      'args': {'name': 'fixture', 'args': 'x' * size}}, fixtures.OWNER)
                except PolicyError as error:
                    self.assertEqual(error.kind, 'quota')
                    break
        charge, _ = f.engine._reservation(f.store.view())
        self.assertGreater(charge, f.store.byte_limit * .9 - 65536)
        native_count = len(f.ledger)
        def refused(principal, op):
            args = {'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
                    'content': [{'type': 'text', 'text': 'x'}]}
            with self.assertRaises(PolicyError) as caught:
                f.engine.send(args, {'principal': principal, 'actor': principal, 'author': 'client:probe'})
            self.assertEqual(caught.exception.kind, 'quota',
                             (str(caught.exception), f.engine._reservation(f.store.view()), f.engine.window()))
            self.assertEqual(f.engine.operation(principal, op)['reason'], 'quota')
        for phase in range(2):
            for n in range(4):
                principal, op = 'p:new-' + str(phase) + '-' + str(n), f.op()
                refused(principal, op)
                refused(principal, op)  # the compact refusal remains idempotent
            if phase == 0:
                epoch = f.engine.window()['epoch']
                principal, serial = 'p:concentrated', 0
                bucket = f.engine._bucket(principal, epoch + ':concentrated-1')
                existing = f.store.view().get(bucket, {'value': {}})['value']
                available = 64 - sum(value.get('window') == epoch for value in existing.values())
                for n in range(available + 1):
                    while True:
                        serial += 1
                        op = epoch + ':concentrated-' + str(serial)
                        candidate = f.engine._bucket(principal, op)
                        if candidate == bucket:
                            break
                    if n < available:
                        refused(principal, op)
                    else:
                        with self.assertRaises(PolicyError) as caught:
                            f.engine.send({'op': op}, {'principal': principal})
                        self.assertEqual(caught.exception.kind, 'busy')
                self.assertNotEqual(f.engine.window()['epoch'], epoch)
        self.assertEqual(len(f.ledger), native_count)
        self.assertTrue(f.engine.current)
        self.assertIsNone(f.engine.maintenance_error)

    def test_known_control_metadata_cannot_take_the_last_archive_record_slots(self):
        f = fixtures.EngineTests()
        f.setUp()
        f.resources['r2'] = fixtures.resource('r2')
        f.store = MemoryStore(record_limit=256)
        f.engine = Coordinator(f.store, config=lambda: f.config, resources=lambda: f.resources,
                               invoke=lambda *args: {'outcome': 'indeterminate'}, clock=lambda: f.now,
                               core_epoch='core1', owner_incarnation='coord1')
        f.engine.control({'op': f.op(), 'target': 'r1', 'action': 'interrupt'}, fixtures.OWNER)
        for _ in range(256):
            try:
                f.engine.control({'op': f.op(), 'target': 'r2', 'action': 'interrupt'}, fixtures.OWNER)
            except PolicyError as error:
                self.assertIn(error.kind, ('quota', 'busy'))
                break
        for _ in range(256):
            try:
                f.resume(action='pause')
            except PolicyError as error:
                self.assertIn(error.kind, ('quota', 'busy'))
                break
        with tempfile.TemporaryDirectory(prefix='vqro-coord-archive-') as destination:
            frozen = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': destination}, fixtures.OWNER)
            self.assertEqual(frozen['records'], 1)
            f.engine.archive({'op': f.op(), 'action': 'export', 'archive': frozen['archive']}, fixtures.OWNER)
            freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': frozen['archive']}, fixtures.OWNER)
            self.assertEqual(freed['state'], 'completed')
        self.assertTrue(f.engine.current)
        self.assertIsNone(f.engine.maintenance_error)

    def test_unknown_control_flood_cannot_consume_single_record_archive_capacity(self):
        f = fixtures.EngineTests()
        f.setUp()
        f.resources['r2'] = fixtures.resource('r2')
        for resource in f.resources.values():
            resource['data']['agent']['caps']['agent.command'] = {}
        ledger = []
        def invoke(target, operation, args, deadline):
            ledger.append((target, operation, hashlib.sha256(json.dumps(args, sort_keys=True).encode()).hexdigest()))
            return {'outcome': 'indeterminate', 'error': {'kind': 'backend', 'message': 'native reply lost'}}
        f.engine.invoke = invoke
        def control(target, size):
            return f.engine.control({'op': f.op(), 'target': target, 'action': 'command',
                                     'args': {'name': 'fixture', 'args': 'x' * size}}, fixtures.OWNER)
        self.assertEqual(control('r1', 0)['state'], 'unknown')
        refusals = []
        for size in (180000, 90000, 45000, 22500, 11250, 5625, 2812, 1406, 703, 351, 128, 0):
            consecutive = 0
            for _ in range(256):
                before = len(ledger)
                try:
                    self.assertEqual(control('r2', size)['state'], 'unknown')
                    consecutive = 0
                except PolicyError as error:
                    self.assertIn(error.kind, ('busy', 'quota'))
                    self.assertEqual(len(ledger), before, 'quota rejection must precede native invocation')
                    refusals.append(error.kind)
                    consecutive += 1
                    if consecutive >= (24 if size == 0 else 4):
                        break
        self.assertIn('quota', refusals)
        self.assertTrue(f.engine.current)
        self.assertIsNone(f.engine.maintenance_error)
        charge, _ = f.engine._reservation(f.store.view())
        self.assertGreater(f.store.byte_limit - charge, 256 * 1024)
        before = len(ledger)
        with tempfile.TemporaryDirectory(prefix='vqro-coord-archive-') as destination:
            frozen = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': destination}, fixtures.OWNER)
            self.assertEqual(frozen['state'], 'frozen')
            self.assertEqual(frozen['records'], 1)
            exported = f.engine.archive({'op': f.op(), 'action': 'export', 'archive': frozen['archive']}, fixtures.OWNER)
            self.assertEqual(exported['state'], 'accepted')
            freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': frozen['archive']}, fixtures.OWNER)
            self.assertEqual(freed['state'], 'completed')
            self.assertEqual(len(freed['freed']), 1)
            self.assertTrue(Path(freed['locator']).is_file())
        self.assertEqual(len(ledger), before, 'archive/recovery never invokes native work')
        self.assertTrue(f.engine.current)
        self.assertIsNone(f.engine.maintenance_error)


if __name__ == '__main__':
    unittest.main()
