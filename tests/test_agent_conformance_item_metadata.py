# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Pure O13 checker controls; none is a public/native conformance receipt."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch, AsyncMock

# Owner integrates module bootstrap at END of cases.py; avoid circular first import.
import agent_conformance.cases
from agent_conformance.item_metadata_cases import (
    U64_MAX, check_format_rows, check_notice_role, check_times, expected_times,
    native_ms, unique_item, render_production_blocks, observe_native_metadata,
    observe_native_absence, optional_item_metadata, observe_public_rendering, PUBLIC_PROVIDER,
    qualified_authority_refusal,
)
from agent_conformance.results import CheckFailed, HarnessGap, FixtureFailed
from agent_conformance.wire import ItemReducer, RpcError


class ItemMetadataTests(unittest.TestCase):
    def test_pi_runtime_ms_exact_including_zero(self):
        for value in (0, 123, 1700000000123, (1 << 53) - 1):
            self.assertEqual(check_times('message', {'timestamp': value}, {'at_ms': value}, 'pi'), {'at_ms': value})

    def test_pi_unsafe_js_integer_is_unknown_even_when_it_fits_u64(self):
        for value in (1 << 53, U64_MAX):
            check_times('message', {'timestamp': value}, {}, 'pi')
            with self.assertRaises(CheckFailed):
                check_times('message', {'timestamp': value}, {'at_ms': value}, 'pi')

    def test_codex_native_turn_seconds_checked_not_item_milliseconds(self):
        self.assertEqual(check_times('run', {'startedAt': 1700000001, 'completedAt': 1700000002},
                                     {'started_ms': 1700000001000, 'ended_ms': 1700000002000}, 'codex'),
                         {'started_ms': 1700000001000, 'ended_ms': 1700000002000})
        with self.assertRaises(CheckFailed):
            check_times('run', {'startedAt': 2}, {'started_ms': 2}, 'codex')

    def test_no_monotonicity_or_observation_order_assumption(self):
        for value in (9000, 1000, 7000):
            check_times('message', {'timestamp': value}, {'at_ms': value}, 'pi')

    def test_missing_native_times_remain_absent_never_clock_or_sequence(self):
        for kind, wire, field in (('message', 'pi', 'at_ms'), ('tool', 'pi', 'started_ms'),
                                  ('run', 'pi', 'ended_ms'), ('run', 'codex', 'started_ms')):
            check_times(kind, {}, {}, wire)
            for invented in (0, 1, 1700000000000, None):
                with self.subTest(kind=kind, wire=wire, invented=invented):
                    with self.assertRaises(CheckFailed): check_times(kind, {}, {field: invented}, wire)

    def test_pi_tool_duration_is_the_native_duration_ms_only(self):
        # Pi 1.5.2 tool_execution_end.durationMs (pi-agent-core types.d.ts:498-500): a non-negative integer, or absent.
        check_times('tool', {'durationMs': 1234}, {'duration_ms': 1234}, 'pi')
        check_times('tool', {'durationMs': 0}, {'duration_ms': 0}, 'pi')
        check_times('tool', {}, {}, 'pi')
        for native, public in (({'durationMs': 1234}, {}),                    # dropped
                               ({'durationMs': 1234}, {'duration_ms': 1235}),  # changed
                               ({}, {'duration_ms': 5}),                       # invented
                               ({'durationMs': -1}, {'duration_ms': -1}),      # negative never published
                               ({'durationMs': 1.5}, {'duration_ms': 1.5}),    # non-integer never published
                               ({'durationMs': 7}, {'duration_ms': '7'}),
                               ({'durationMs': 7}, {'duration_ms': 7, 'started_ms': 1})):  # no invented start
            with self.subTest(native=native, public=public), self.assertRaises(CheckFailed):
                check_times('tool', native, public, 'pi')
        # lead-secondary's control on ec04745f: Codex reports no tool duration, so any duration_ms there is invented.
        check_times('tool', {}, {}, 'codex')
        for value in (1234, -1, 'invented'):
            with self.subTest(codex=value), self.assertRaises(CheckFailed):
                check_times('tool', {}, {'duration_ms': value}, 'codex')

    def test_invalid_native_time_does_not_become_evidence(self):
        for value in (None, True, False, -1, 2.0, '1234', U64_MAX + 1):
            self.assertIsNone(native_ms(value))
            check_times('message', {'timestamp': value}, {}, 'pi')
            with self.assertRaises(CheckFailed):
                check_times('message', {'timestamp': value}, {'at_ms': 1234}, 'pi')

    def test_checked_second_conversion_overflow_remains_unknown(self):
        self.assertIsNone(native_ms(U64_MAX // 1000 + 1, 1000))
        check_times('run', {'startedAt': U64_MAX // 1000 + 1}, {}, 'codex')
        with self.assertRaises(CheckFailed):
            check_times('run', {'startedAt': U64_MAX // 1000 + 1}, {'started_ms': 0}, 'codex')

    def test_public_time_must_be_typed_and_exact_not_just_present(self):
        for actual in (None, True, 123.0, '123', 124):
            with self.assertRaises(CheckFailed):
                check_times('message', {'timestamp': 123}, {'at_ms': actual}, 'pi')
        with self.assertRaises(CheckFailed): check_times('message', {'timestamp': 123}, {}, 'pi')

    def test_missing_native_or_unknown_wire_is_gap_not_pass_or_na(self):
        for kind, native, wire in (('message', None, 'pi'), ('message', {}, 'acp'), ('unknown', {}, 'pi')):
            with self.assertRaises(HarnessGap): expected_times(kind, native, wire)

    def test_identity_marker_not_item_iteration_or_timestamp_sorting(self):
        entities = {('message', 'new'): {'value': {'content': [{'type': 'text', 'text': 'second'}], 'at_ms': 10}},
                    ('message', 'old'): {'value': {'content': [{'type': 'text', 'text': 'first'}], 'at_ms': 900}}}
        self.assertEqual(unique_item(entities, 'message', 'first')[0], 'old')
        self.assertEqual(unique_item(entities, 'message', 'second')[0], 'new')
        for marker in ('missing', ''):
            with self.assertRaises(CheckFailed): unique_item(entities, 'message', marker)
        entities[('message', 'duplicate')] = entities[('message', 'old')]
        with self.assertRaises(CheckFailed): unique_item(entities, 'message', 'first')

    def blocks(self):
        return [{'type': 'text', 'text': 'plain', 'format': 'plain'},
                {'type': 'text', 'text': 'default'},
                {'type': 'text', 'text': '**markdown**', 'format': 'markdown'},
                {'type': 'text', 'text': '**future**', 'format': 'future-format'}]

    def rows(self):
        return [{'type': 'text', 'text': 'plain'}, {'type': 'text', 'text': 'default'},
                {'type': 'text', 'text': '**markdown**', 'format': 'markdown'},
                {'type': 'text', 'text': '**future**'}]

    def test_per_block_plain_default_markdown_unknown(self):
        check_format_rows(self.blocks(), self.rows())

    def test_global_markdown_fallback_or_ignored_markdown_never_pass(self):
        for index in range(4):
            rows = self.rows()
            rows[index]['format'] = 'plain' if index == 2 else 'markdown'
            with self.assertRaises(CheckFailed): check_format_rows(self.blocks(), rows)
        with self.assertRaises(CheckFailed): check_format_rows(self.blocks(), self.rows()[:-1])
        rows = self.rows(); rows.reverse()
        with self.assertRaises(CheckFailed): check_format_rows(self.blocks(), rows)

    def test_actual_pinned_production_component_renders_per_block_and_restores_import(self):
        root = Path(__file__).resolve().parents[1] / 'extensions'
        hashes = {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                  for name in ('vqro_display.py', 'agent_transcript.py')}
        old = sys.modules.get('vqro_display')
        rows, identity = render_production_blocks(self.blocks(), root, 'f' * 40, hashes)
        self.assertEqual([row['text'] for row in rows], [block['text'] for block in self.blocks()])
        self.assertEqual(identity['component'], 'production agent_transcript.content_rows')
        self.assertIs(sys.modules.get('vqro_display'), old)

    def test_production_component_hash_mismatch_never_counts_as_render_proof(self):
        root = Path(__file__).resolve().parents[1] / 'extensions'
        with self.assertRaises(FixtureFailed):
            render_production_blocks(self.blocks(), root, 'f' * 40, {'vqro_display.py': '0' * 64})
        with self.assertRaises(FixtureFailed):
            render_production_blocks(self.blocks(), root, 'moving-main', {})

    def test_system_notice_is_not_coordination_or_input_origin(self):
        check_notice_role({'role': 'system'})
        for value in ({'role': 'coordination'}, {'role': 'assistant'}, {'role': 'system', 'origin': {'kind': 'human'}},
                      {'role': 'system', 'origin': None}):
            with self.assertRaises(CheckFailed): check_notice_role(value)


class MetadataContext:
    """Local control/ledger/public-state fake, never a qualification witness."""
    def __init__(self, wire, *, lose_ledger=False, corrupt_public=False, ack_only=False, lose_written=False,
                 final_transition=None):
        self.manifest = {'wire': wire}; self.evidence = []
        reducer = ItemReducer()
        reducer.accept({'op': 'snapshot', 'phase': 'begin', 'epoch': 'e1', 'coverage': 'tail'})
        reducer.accept({'op': 'snapshot', 'phase': 'end', 'seq': 0})
        self.view = SimpleNamespace(reducer=reducer, initial='reset')
        self.native = self
        self.rows, self.serial, self.turn_id = [], 0, 0
        self.lose_ledger, self.corrupt_public, self.ack_only = lose_ledger, corrupt_public, ack_only
        self.lose_written = lose_written
        self.final_transition, self.pending_updates = final_transition, []
    def delivery(self): return 'unit-marker'
    def mark(self): return self.serial
    def records(self, kind, after=0):
        return [] if self.lose_ledger or self.lose_written and kind == 'native_event' else [r for r in self.rows if r['kind'] == kind and r['seq'] > after]
    async def attach(self, **unused): return self.view
    async def wait_view(self, view, predicate, **unused):
        if not predicate(view.reducer):
            for key, value in self.pending_updates: self.put(*key, value)
            self.pending_updates.clear()
            if not predicate(view.reducer): raise CheckFailed('public observation missing')
    def put(self, kind, ident, value):
        if not self.ack_only: self.view.reducer.entities[(kind, ident)] = {'value': value}
    async def call(self, verb, **params):
        self.serial += 1
        if verb == 'message':
            payload = {'role': params['role'], 'content': [{'type': 'text', 'text': params['text']}]}
            if 'timestamp' in params: payload['timestamp'] = params['timestamp']
            self.rows.append({'kind': 'native_message', 'seq': self.serial, 'message': deepcopy(payload)})
            value = {'role': payload['role'], 'content': deepcopy(payload['content']), 'final': True}
            if 'timestamp' in payload: value['at_ms'] = payload['timestamp'] + int(self.corrupt_public)
            key = ('message', 'm' + str(self.serial))
            if (self.final_transition == 'correct' or self.final_transition == 'supplied' and 'timestamp' in payload
                    or self.final_transition == 'missing' and 'timestamp' not in payload):
                final = deepcopy(value)
                if self.final_transition != 'correct': final['at_ms'] = payload.get('timestamp', 0) + 111
                value['final'] = False
                self.pending_updates.append((key, final))
            self.put(*key, value)
        elif verb == 'notice':
            event = {'type': params['type'], **{key: value for key, value in params.items() if key != 'type'}}
            self.rows.append({'kind': 'native_notice', 'seq': self.serial, 'event': event})
            self.put('message', 'notice', {'role': 'system', 'content': [{'type': 'text', 'text': params['errorMessage']}], 'final': True})
        elif verb == 'turn':
            if params['state'] == 'start': self.turn_id += 1
            run = 'turn-' + str(self.turn_id)
            turn = {'id': run, 'status': 'inProgress' if params['state'] == 'start' else 'completed'}
            for native, field in (('startedAt', 'started_at'), ('completedAt', 'completed_at')):
                if field in params: turn[native] = params[field]
            self.rows.append({'kind': 'turn', 'seq': self.serial, 'native_turn': turn})
            if self.manifest['wire'] == 'pi':
                self.rows.append({'kind': 'native_event', 'seq': self.serial, 'wire': 'pi',
                    'type': 'agent_start' if params['state'] == 'start' else 'agent_end',
                    'payload': {'type': 'agent_start' if params['state'] == 'start' else 'agent_end'}})
            else:
                self.rows.append({'kind': 'native_event', 'seq': self.serial, 'wire': 'codex',
                    'method': 'turn/started' if params['state'] == 'start' else 'turn/completed',
                    'payload': {'turn': deepcopy(turn)}})
            state = {'run': run if params['state'] == 'start' else None,
                     'completion': None if params['state'] == 'start' else {'id': run}}
            self.put('state', 'current', state)
            if 'startedAt' in turn or 'completedAt' in turn:
                value = {'id': run}
                for native, field in (('startedAt', 'started_ms'), ('completedAt', 'ended_ms')):
                    if native in turn: value[field] = turn[native] * 1000
                self.put('run', run, value)
        elif verb == 'tool_call':
            marker, phase = params['id_ref'], params['state']
            public = {'title': 'fixture', 'status': 'running' if phase == 'start' else 'completed'}
            if self.corrupt_public: public['started_ms'] = 1
            if self.manifest['wire'] == 'pi':
                kind = 'tool_execution_' + phase
                self.rows.append({'kind': 'native_event', 'seq': self.serial, 'wire': 'pi', 'type': kind,
                    'payload': {'type': kind, 'toolCallId': marker, 'toolName': 'fixture', 'args': {}, 'result': {}}})
            else:
                self.rows.append({'kind': 'native_event', 'seq': self.serial, 'wire': 'codex',
                    'method': 'item/started' if phase == 'start' else 'item/completed',
                    'payload': {'item': {'id': marker, 'type': 'commandExecution', 'command': 'fixture'}}})
            self.put('tool', marker, public)
        elif verb == 'retain':
            item = {'id': 'retained-' + str(self.serial), 'type': 'agentMessage', 'text': '0:' + params['text']}
            self.rows.append({'kind': 'native_event', 'seq': self.serial, 'wire': 'codex',
                'method': 'item/completed', 'payload': {'item': item}})
            value = {'role': 'assistant', 'content': [{'type': 'text', 'text': item['text']}], 'final': True}
            if self.final_transition == 'codex_missing':
                final = {**deepcopy(value), 'at_ms': 1700000000999}
                value['final'] = False
                self.pending_updates.append((('message', item['id']), final))
            self.put('message', item['id'], value)
        else:
            raise AssertionError('unagreed fixture verb: ' + verb)
        return {'seq': self.serial}  # ACK cannot be sufficient for a witness


class NativeMetadataPhaseTests(unittest.IsolatedAsyncioTestCase):
    async def test_pi_phase_binds_native_payload_and_public_values(self):
        ctx = MetadataContext('pi')
        _, evidence = await observe_native_metadata(ctx)
        self.assertEqual(len(evidence), 4)
        self.assertEqual([r['public_value'].get('at_ms') for r in evidence[:3]],
                         [1700000000900, 1700000000100, None])
        self.assertEqual(ctx.evidence, evidence)

    async def test_codex_native_seconds_and_fresh_omitted_run(self):
        ctx = MetadataContext('codex')
        _, evidence = await observe_native_metadata(ctx)
        self.assertEqual(evidence[0]['public_value']['started_ms'], 1700000001000)
        self.assertEqual(evidence[1]['public_value']['ended_ms'], 1700000003000)
        self.assertIsNone(evidence[2]['public_value'])
        self.assertIsNone(evidence[3]['public_value'])
        self.assertNotEqual(evidence[0]['entity'], evidence[2]['entity'])

    async def test_correct_partial_then_wrong_final_timestamp_never_passes(self):
        with self.assertRaises(CheckFailed):
            await observe_native_metadata(MetadataContext('pi', final_transition='supplied'))

    async def test_untimed_partial_then_invented_final_clock_never_passes(self):
        with self.assertRaises(CheckFailed):
            await observe_native_metadata(MetadataContext('pi', final_transition='missing'))
        ctx = MetadataContext('codex', final_transition='codex_missing')
        with self.assertRaises(CheckFailed): await observe_native_absence(ctx, ctx.view)

    async def test_correct_partial_is_not_graded_until_correct_final_is_installed(self):
        ctx = MetadataContext('pi', final_transition='correct')
        _, evidence = await observe_native_metadata(ctx)
        self.assertTrue(all(row['public_value']['final'] is True for row in evidence))

    async def test_public_value_or_ack_without_actual_native_ledger_is_not_pass(self):
        with self.assertRaises(HarnessGap):
            await observe_native_metadata(MetadataContext('pi', lose_ledger=True))
        with self.assertRaises(CheckFailed):
            await observe_native_metadata(MetadataContext('pi', ack_only=True))

    async def test_native_ack_and_ledger_cannot_hide_wrong_public_timestamp(self):
        with self.assertRaises(CheckFailed):
            await observe_native_metadata(MetadataContext('pi', corrupt_public=True))

    async def test_pi_and_codex_tool_run_omission_use_written_payloads_and_public_items(self):
        for wire in ('pi', 'codex'):
            ctx = MetadataContext(wire)
            view, _ = await observe_native_metadata(ctx)
            await observe_native_absence(ctx, view)
            self.assertEqual(len([e for e in ctx.evidence if e['kind'] == 'o13_native_tool_absence']), 2)
            self.assertTrue(any(e['kind'] == 'o13_native_run_end_absence' for e in ctx.evidence))

    async def test_missing_after_write_ledger_is_harness_gap_not_unknown_success(self):
        ctx = MetadataContext('pi', lose_written=True)
        with self.assertRaises(HarnessGap): await observe_native_absence(ctx, ctx.view)

    async def test_viewer_clock_tool_time_without_native_field_fails(self):
        ctx = MetadataContext('pi', corrupt_public=True)
        with self.assertRaises(CheckFailed): await observe_native_absence(ctx, ctx.view)

    async def test_complete_handler_does_not_pass_without_public_producer_consumer_descriptor(self):
        ctx = MetadataContext('pi')
        with self.assertRaises(HarnessGap): await optional_item_metadata(ctx)

    async def test_uninstalled_metadata_verb_is_gap_not_capability_omission(self):
        ctx = MetadataContext('pi')
        ctx.call = AsyncMock(side_effect=FixtureFailed('unimplemented fixture verb: message'))
        with self.assertRaises(HarnessGap): await optional_item_metadata(ctx)

    async def test_corrupt_fixture_does_not_get_relabelled_as_missing_capability(self):
        ctx = MetadataContext('pi')
        ctx.call = AsyncMock(side_effect=FixtureFailed('corrupt native fixture record'))
        with self.assertRaises(FixtureFailed): await optional_item_metadata(ctx)

    def test_o13_is_registered_only_through_owner_module_import(self):
        self.assertIs(agent_conformance.cases.HANDLERS['O13'], optional_item_metadata)


class PublicRendererPhaseTests(unittest.IsolatedAsyncioTestCase):
    async def exercise(self, *, wrong_content=False, wrong_role=False, missing_sent=False, upgrade=False,
                       refusal_data=None, late_write=False):
        with tempfile.TemporaryDirectory(prefix='o13-unit-') as directory:
            ctx = MetadataContext('pi')
            ctx.root = Path(directory); (ctx.root / 'work').mkdir()
            source = Path(__file__).resolve().parents[1] / 'extensions'
            ctx.manifest.update(viewer_extensions=str(source), viewer_revision='f' * 40,
                viewer_sha256={path.relative_to(source).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                               for path in source.glob('*.py')})
            ctx.backend = SimpleNamespace(env={'VQRO_SOCKET': '/private/unit-only'}, tracked=[])
            ctx.peers = []
            async def facts(): return {'epoch': 'e1', 'caps': {'rev': 1, 'agent.send': {'modes': {'start': {}}}}}
            async def send(*args, **kwargs): return {'outcome': 'observed', 'effect': 'valid-framed-recovery'}
            ctx.facts, ctx.send = facts, send
            ctx.agents = [{'id': 'r-native'}]
            ctx._notice = lambda *args: None
            state = {'entries': {'doc/principals/owner': {'version': 1, 'value': 'actual-owner'}}, 'txn': 0, 'fenced': False}
            ctx.writes = lambda **kw: [{'command': 'late-send'}] if late_write and state['fenced'] else []
            class Owner:
                async def call(self, method, params):
                    if method == 'workspace.get': return deepcopy(state['entries'].get(params['key']))
                    if method == 'workspace.apply':
                        for op in params['ops']:
                            state['entries'][op['key']] = {'version': 1, 'value': op['value']}
                        state['fenced'] = True; state['txn'] += 1
                        return {'txn': state['txn']}
                    raise AssertionError(method)
            ctx.peer = Owner()
            class Process:
                pid = 987654321
                terminated = False
                def poll(self): return 0 if self.terminated else None
                def terminate(self): self.terminated = True
                def kill(self): self.terminated = True
                def wait(self, timeout): self.terminated = True; return 0
            process = Process()
            def spawn(argv, **kwargs):
                spec = json.loads(Path(argv[3]).read_text())
                state['resource'] = {'id': 'r-unit', 'backend_ref': spec['name'], 'available': True}
                reducer = ItemReducer()
                reducer.accept({'op': 'snapshot', 'phase': 'begin', 'epoch': spec['epoch'], 'coverage': 'all_retained'})
                for row in spec['records']:
                    record = deepcopy(row)
                    if wrong_content and row['id'] == 'formats': record['value']['content'][0]['text'] = 'wrong'
                    if wrong_role and row['id'] == 'system': record['value']['role'] = 'coordination'
                    reducer.accept(record)
                reducer.accept({'op': 'snapshot', 'phase': 'end', 'seq': len(spec['records'])})
                state['view'] = SimpleNamespace(reducer=reducer, initial='reset')
                rows = [] if missing_sent else [{'kind': 'snapshot_sent', 'records': spec['records']}]
                Path(argv[4]).write_text(''.join(json.dumps(row)+'\n' for row in rows))
                return process
            async def resources(): return [state['resource']]
            async def attach(**kwargs): return state['view']
            ctx.resources, ctx.attach = resources, attach
            class Observer:
                closed = False
                async def call(self, method, params):
                    if method == 'hello': return {'actor': 'undeclared'}
                    if method == 'workspace.apply':
                        # Ordinary writes are VALID. Only principal configuration
                        # is protected; this intentionally rejects the old probe.
                        if params['ops'][0]['key'] != 'doc/principals/owner' or upgrade:
                            return {'txn': 1}
                        raise RpcError({'data': refusal_data if refusal_data is not None else
                                        {'kind': 'forbidden', 'outcome': 'rejected'}})
                    if method == 'resource.invoke':
                        # Ordinary framed recovery is VALID, representation is not.
                        if params.get('on_behalf_of') != 'actual-owner' or upgrade:
                            return {'outcome': 'observed', 'effect': 'raw-recovery'}
                        raise RpcError({'data': refusal_data if refusal_data is not None else
                                        {'kind': 'forbidden', 'outcome': 'rejected'}})
                    raise AssertionError(method)
                def on(self, *args): pass
                async def close(self): self.closed = True
            observer = Observer()
            async def connect(*args, **kwargs): return observer
            module = 'agent_conformance.item_metadata_cases'
            with patch(module+'.python', return_value=sys.executable), \
                 patch(module+'.subprocess.Popen', side_effect=spawn), patch(module+'.Peer.connect', side_effect=connect), \
                 patch(module+'.identity', side_effect=lambda pid: None if process.terminated else {'pid': pid, 'birth': 1}):
                try:
                    await observe_public_rendering(ctx)
                    return ctx.evidence
                finally:
                    self.assertTrue(process.terminated)
                    self.assertTrue(observer.closed)

    async def test_generic_public_phase_retains_actual_component_rows_and_source_scope(self):
        evidence = await self.exercise()
        proof = next(row for row in evidence if row['kind'] == 'o13_public_renderer')
        self.assertIn('NOT native adapter', proof['source'])
        self.assertEqual(len(proof['rendered_rows']), 4)
        self.assertEqual(proof['authority_before']['protected_owner'], 'actual-owner')
        self.assertEqual(proof['authority_after']['protected_owner'], 'actual-owner')
        self.assertTrue(all(row['error']['data']['outcome'] == 'rejected'
                            for row in proof['authority_after']['refusals']))
        self.assertTrue(evidence[-1]['reaped'])

    async def test_spec_dict_without_matching_public_observation_is_not_pass(self):
        with self.assertRaises(CheckFailed): await self.exercise(wrong_content=True)
        with self.assertRaises(CheckFailed): await self.exercise(wrong_role=True)

    async def test_open_or_ack_without_sent_payload_ledger_is_not_pass(self):
        with self.assertRaises(CheckFailed): await self.exercise(missing_sent=True)

    async def test_role_observation_cannot_hide_standing_upgrade(self):
        with self.assertRaises(CheckFailed): await self.exercise(upgrade=True)

    async def test_missing_indeterminate_or_effectful_forbidden_is_not_qualified_refusal(self):
        for data in ({'kind': 'forbidden'},
                     {'kind': 'forbidden', 'outcome': 'indeterminate', 'effect': 'fx-pending'},
                     {'kind': 'forbidden', 'outcome': 'rejected', 'effect': 'fx-admitted'}):
            with self.subTest(data=data):
                with self.assertRaises(CheckFailed): await self.exercise(refusal_data=data)

    async def test_late_native_write_after_public_ordering_fence_fails(self):
        with self.assertRaises(CheckFailed): await self.exercise(late_write=True)

    def test_qualified_core_rejection_rejects_unknown_or_admitted_outcomes(self):
        qualified_authority_refusal(RpcError({'data': {'kind': 'forbidden', 'outcome': 'rejected'}}))
        for data in ({'kind': 'forbidden'}, {'kind': 'forbidden', 'outcome': 'indeterminate'},
                     {'kind': 'forbidden', 'outcome': 'rejected', 'effect': None}):
            with self.assertRaises(CheckFailed): qualified_authority_refusal(RpcError({'data': data}))

    def test_embedded_producer_uses_real_sdk_and_compiles(self):
        compile(PUBLIC_PROVIDER, '<O13-public-provider>', 'exec')
        self.assertIn('from vqro_provider import Provider', PUBLIC_PROVIDER)
        self.assertNotIn('coordination_engine', PUBLIC_PROVIDER)

