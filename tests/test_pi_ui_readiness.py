"""Startup registration is not launcher completion; transient notices are not readiness frames."""
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import pi_provider_e2e as e2e


class StartupDone(Exception):
    pass


class StartupProbe(e2e.E2E):
    def __init__(self):
        self.env, self.tracked, self.log = {}, [], []
        self.args = SimpleNamespace(vqro=Path('vqro'))
        self.completed = False
        self.agent_record = {'id': 'r2', 'kind': 'agent', 'host': 'r1', 'available': True}

    def snapshot(self):
        return {'epoch': 'e', 'seq': 1, 'resources': [self.agent_record], 'entries': {}}

    def ui(self, *args):
        return '0' if args[0] == 'display-message' else ''

    def wait(self, predicate, *args, **kwargs):
        return predicate()

    def audit_error(self, cursor, outcome, event_method='resource.invoke', *, ready=None, timeout=None):
        self.assert_launch = (cursor, outcome, event_method, timeout)
        assert ready is not None, 'startup must observe completion, not mere availability'
        assert ready('resource', self.agent_record) is None
        assert ready('notify', {'text': 'Pi agent r2 created (indeterminate)'}) is None
        self.completed = True
        record = ready('notify', {'text': 'Pi agent r2 created (observed); no prompt sent.'})
        return {'outcome': 'observed'}, record

    def ui_frame(self, name, *texts):
        if name == 'agent-ready':
            assert self.completed, 'agent exists but launcher navigation is still pending: No layout yet.'
            assert texts == ('Ready for a prompt',), 'do not require overlap with a four-second toast'

    def ui_navigation(self, *args):
        raise StartupDone()


class Readiness(unittest.TestCase):
    def test_registered_agent_does_not_start_frame_wait_before_launch_completion(self):
        probe = StartupProbe()
        with patch.object(e2e, 'identity', return_value=None), self.assertRaises(StartupDone):
            probe.ui_launch()
        self.assertEqual(probe.assert_launch, ({'epoch': 'e', 'seq': 1}, 'observed', 'resource.spawn', 15))

    def test_nonmatching_frame_is_preserved_in_failure_diagnostic(self):
        probe = object.__new__(e2e.E2E)
        probe.log = []
        probe.ui = lambda *args: 'No layout yet.\n'
        def fail(predicate, *args, **kwargs):
            predicate()
            raise AssertionError('last=None')
        probe.wait = fail
        with self.assertRaisesRegex(AssertionError, 'No layout yet'):
            probe.ui_frame('agent-ready', 'Ready for a prompt')
        self.assertEqual(probe.log[-1]['missing'], ['Ready for a prompt'])


class Wire:
    def __init__(self, rows):
        self.rows, self.writes = iter(rows), []
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def write(self, data): self.writes.append(json.loads(data))
    def readline(self):
        row = next(self.rows, None)
        return (json.dumps(row) + '\n').encode() if row is not None else b''


class Socket:
    def __init__(self, rows): self.wire = Wire(rows)
    def __enter__(self): return self
    def __exit__(self, *args): pass
    def settimeout(self, timeout): pass
    def connect(self, path): pass
    def makefile(self, *args, **kwargs): return self.wire


class EventBarrier(unittest.TestCase):
    def test_observed_effect_alone_does_not_complete_readiness(self):
        probe = object.__new__(e2e.E2E)
        probe.env, probe.log = {'VQRO_SOCKET': 'unused.sock'}, []
        events = [{'id': 1, 'result': {}}, {'id': 2, 'result': {}}]
        for kind, data in [('effect', {'method': 'resource.spawn', 'outcome': 'observed'}),
                           ('resource', {'id': 'r2', 'kind': 'agent', 'available': True}),
                           ('notify', {'text': 'creation completed'})]:
            events.append({'method': 'event', 'params': {'type': kind, 'data': data}})
        seen = []
        def ready(kind, data):
            seen.append(kind)
            return {'id': 'r2'} if kind == 'notify' else None
        sock = Socket(events)
        with patch.object(e2e.socket, 'AF_UNIX', 0, create=True), patch.object(e2e.socket, 'socket', return_value=sock):
            effect, agent = probe.audit_error({'epoch': 'e', 'seq': 1}, 'observed', 'resource.spawn', ready=ready, timeout=15)
        self.assertEqual(effect['outcome'], 'observed')
        self.assertEqual(agent['id'], 'r2')
        self.assertEqual(seen, ['effect', 'resource', 'notify'])
        self.assertEqual([row['method'] for row in sock.wire.writes], ['hello', 'events.subscribe'])

    def test_creation_notice_does_not_substitute_for_observed_spawn(self):
        probe = object.__new__(e2e.E2E)
        probe.env, probe.log = {'VQRO_SOCKET': 'unused.sock'}, []
        rows = [{'id': 1, 'result': {}}, {'id': 2, 'result': {}},
                {'method': 'event', 'params': {'type': 'notify', 'data': {'text': 'created'}}}]
        with patch.object(e2e.socket, 'AF_UNIX', 0, create=True), patch.object(e2e.socket, 'socket', return_value=Socket(rows)):
            with self.assertRaisesRegex(AssertionError, 'core closed'):
                probe.audit_error({'epoch': 'e', 'seq': 1}, 'observed', 'resource.spawn',
                                  ready=lambda *args: {'id': 'r2'}, timeout=15)

    def test_rejected_spawn_still_fails_before_readiness(self):
        probe = object.__new__(e2e.E2E)
        probe.env, probe.log = {'VQRO_SOCKET': 'unused.sock'}, []
        rows = [{'id': 1, 'result': {}}, {'id': 2, 'result': {}},
                {'method': 'event', 'params': {'type': 'effect', 'data': {'method': 'resource.spawn', 'outcome': 'rejected'}}}]
        with patch.object(e2e.socket, 'AF_UNIX', 0, create=True), patch.object(e2e.socket, 'socket', return_value=Socket(rows)):
            with self.assertRaises(AssertionError):
                probe.audit_error({'epoch': 'e', 'seq': 1}, 'observed', 'resource.spawn',
                                  ready=lambda *args: {'id': 'r2'}, timeout=15)
