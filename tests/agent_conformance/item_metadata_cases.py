# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""O13: supplied native metadata -> observed public items, never a viewer clock.

Pure checker results alone are not a conformance PASS. The handler must bind
these inputs to actual native emitted-payload ledger and public item witnesses.
"""
import asyncio
import hashlib
import importlib.util
import json
import subprocess
from pathlib import Path
import re
import sys

from .cases import case, fresh_view
from .results import check, HarnessGap, FixtureFailed
from .wire import Peer, RpcError
from pi_provider_e2e import identity

U64_MAX = (1 << 64) - 1

# Same public SDK mechanism as stream_bound_cases.start_item_provider, embedded
# here under K1's ownership decision: no cross-owned module import/bootstrap.
PUBLIC_PROVIDER = r'''
import asyncio, json, sys
sys.path.insert(0, sys.argv[1])
from vqro_provider import Provider, Limits, ProviderError, integer
from vqro_provider_stream import Stream
with open(sys.argv[2]) as f: spec = json.load(f)
def record(**row):
    with open(sys.argv[3], 'a') as f: f.write(json.dumps(row) + '\n')
def line(row): return json.dumps(row, separators=(',', ':')).encode() + b'\n'
class P(Provider):
    async def _open(self, params):
        if params.get('channel') != 'items': return await super()._open(params)
        stream = Stream(self, params)
        self.streams[integer(params.get('stream'))] = stream
        record(kind='open', ref=params.get('ref'), after=params.get('after'))
        self._work(self.serve(stream))
        return stream, {'initial': 'reset', 'epoch': spec['epoch']}
    async def serve(self, stream):
        try:
            await stream.send(line({'op': 'snapshot', 'phase': 'begin', 'epoch': spec['epoch'], 'coverage': 'all_retained'}))
            for row in spec['records']: await stream.send(line(row))
            await stream.send(line({'op': 'snapshot', 'phase': 'end', 'seq': len(spec['records'])}))
            record(kind='snapshot_sent', records=spec['records'])
            while not stream.closed: await asyncio.sleep(.1)
        except ProviderError: pass
async def main():
    items = {'retain_bytes': 131072, 'entities': 64, 'entity_bytes': 65536}
    p = P(spec['name'], spec['name']+'-instance', {'agent': {'v': 0, 'modules': {}}, 'items.stream': items},
          limits=Limits(pending=64, max_streams=8), software={'name': 'O13-public-producer', 'version': '0'})
    await p.connect()
    await p.full_sync([{'ref': spec['name'], 'incarnation': 'i1', 'kind': 'agent', 'name': spec['name'], 'state': 'live',
        'data': {'agent': {'v': 0, 'epoch': 'e1', 'state': 'idle', 'lifecycle': 'live', 'session': {'id': 's1'},
                           'run': None, 'caps': {'rev': 1}, 'incomplete': []}, 'items': items}}])
    record(kind='ready')
    await p.wait_closed()
asyncio.run(main())
'''


def native_ms(value, scale=1):
    """Same typed, checked native conversion as current Pi/Codex decoders."""
    if type(value) is not int or not 0 <= value <= U64_MAX // scale:
        return None
    return value * scale


# The native field carrying a runtime's own tool duration in ms, per wire. Pi 1.5.2: tool_execution_end.durationMs
# (pi-agent-core types.d.ts:498-500). A wire not listed has none.
NATIVE_TOOL_DURATION = {'pi': 'durationMs'}


def expected_times(kind, native, wire):
    """native is the ACTUAL message/tool/turn payload, not an observation stamp.

    Pi state.rs message_value maps message.timestamp (ms); its tool/run event
    paths publish no optional times. Codex.rs run timestamps map Turn seconds.
    Unsupported/omitted native facts stay unknown, not a field capability skip.
    """
    if not isinstance(native, dict):
        raise HarnessGap('O13 needs the actual emitted native payload')
    if wire not in ('pi', 'codex'):
        raise HarnessGap('O13 has no source-validated metadata mapping for this native wire')
    if kind == 'message':
        value = native_ms(native.get('timestamp')) if wire == 'pi' else None
        # Pi state.rs count() rejects unsafe JS integers, even though they fit u64.
        return {'at_ms': value if value is not None and value < (1 << 53) else None}
    if kind not in ('tool', 'run'):
        raise HarnessGap('O13 timestamp witness has an unknown entity kind')
    if kind == 'run' and wire == 'codex':
        return {'started_ms': native_ms(native.get('startedAt'), 1000),
                'ended_ms': native_ms(native.get('completedAt'), 1000)}
    if kind == 'tool':
        # Generic for every runtime: duration_ms is the runtime's own tool duration, mapped exactly (a safe non-negative
        # integer), or absent. A wire with no native tool duration (Codex today) must publish none.
        field = NATIVE_TOOL_DURATION.get(wire)
        duration = native_ms(native.get(field)) if field else None
        return {'started_ms': None, 'ended_ms': None,
                'duration_ms': duration if duration is not None and duration < (1 << 53) else None}
    return {'started_ms': None, 'ended_ms': None}


def check_times(kind, native, public, wire):
    check(isinstance(public, dict), 'O13 public entity value is an object')
    expected = expected_times(kind, native, wire)
    for field, value in expected.items():
        if value is None:
            check(field not in public, 'O13 missing/invalid native time remains absent: ' + field)
        else:
            check(type(public.get(field)) is int and public[field] == value,
                  'O13 public time equals the exact supplied native value: ' + field)
    return expected


def unique_item(entities, kind, marker):
    """Match a unique semantic marker, never zip items by arrival or timestamp."""
    check(isinstance(marker, str) and bool(marker), 'O13 requires a nonempty actual native marker')
    matches = []
    for (item_kind, ident), row in entities.items():
        value = row.get('value', {})
        if item_kind == kind and any(block.get('type') == 'text' and marker in block.get('text', '')
                                     for block in value.get('content', []) if isinstance(block, dict)):
            matches.append((ident, value))
    check(len(matches) == 1, 'O13 marker identifies exactly one observed public entity')
    return matches[0]


def check_format_rows(content, rows):
    """Use distinct single-line native test markers and actual production rows.

    No renderer is implemented here. Only an explicit markdown block may yield
    markdown rows; omitted/plain/unknown formats remain plain independently.
    """
    check(isinstance(rows, list) and len(rows) == len(content), 'O13 renders every metadata test block exactly once')
    for block, row in zip(content, rows):
        check(row.get('type') == 'text' and row.get('text') == block['text'],
              'O13 rendered text preserves its exact native block')
        if block.get('format') == 'markdown':
            check(row.get('format') == 'markdown', 'O13 explicit markdown is rendered as markdown')
        else:
            check('format' not in row or row['format'] == 'plain',
                  'O13 plain/default/unknown format falls back to plain per block')


def render_production_blocks(content, directory, revision, hashes):
    """Run the pinned production component, not a locally reimplemented renderer.

    K1 supplies these from its immutable current-kit viewer descriptor. Caller
    must feed actual public item content; local unit inputs are not wire proof.
    """
    if not isinstance(revision, str) or not re.fullmatch('[0-9a-f]{40}', revision):
        raise FixtureFailed('O13 production viewer needs an exact source revision')
    root = Path(directory)
    if not root.is_absolute() or not root.is_dir():
        raise HarnessGap('O13 production viewer source is not staged')
    for name in ('vqro_display.py', 'agent_transcript.py'):
        path = root / name
        if not path.is_file():
            raise HarnessGap('O13 production viewer dependency absent: ' + name)
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != hashes.get(name):
            raise FixtureFailed('O13 production viewer source hash mismatch: ' + name)
    def load(name):
        spec = importlib.util.spec_from_file_location('_o13_' + name, root / (name + '.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    previous = sys.modules.get('vqro_display')
    try:
        sys.modules['vqro_display'] = load('vqro_display')
        transcript = load('agent_transcript')
        rows = transcript.content_rows(content)
    finally:
        if previous is None: sys.modules.pop('vqro_display', None)
        else: sys.modules['vqro_display'] = previous
    check_format_rows(content, rows)
    return rows, {'component': 'production agent_transcript.content_rows', 'revision': revision,
                  'path': str(root / 'agent_transcript.py'), 'sha256': dict(hashes),
                  'scope': 'component rendering, not full TUI paint'}


def check_notice_role(value):
    check(value.get('role') == 'system', 'O13 runtime notice is system, not coordination or input authority')
    check('origin' not in value, 'O13 runtime notice does not manufacture an input origin/authority')


def emitted_payload(ctx, kind, field, mark, predicate):
    rows = [row for row in ctx.native.records(kind, after=mark)
            if isinstance(row.get(field), dict) and predicate(row[field])]
    if not rows:
        raise HarnessGap('O13 actual emitted native payload witness is absent: ' + kind)
    check(len(rows) == 1, 'O13 native identity names exactly one emitted payload')
    return rows[0][field]


async def observe_native_metadata(ctx):
    """Existing native controls from fixture owner 322caabc, plus public items.

    This phase alone cannot pass O13: tool/message omission and generic public
    format/role producer witnesses are still required by the complete handler.
    """
    wire = ctx.manifest.get('wire', 'pi')
    view = await fresh_view(ctx)
    prefix = 'O13-' + ctx.delivery()
    evidence = []
    def retain(row):
        evidence.append(row)
        ctx.evidence.append(row)
    if wire == 'pi':
        # Runtime observation order deliberately disagrees with supplied time.
        for suffix, timestamp in (('later', 1700000000900), ('earlier', 1700000000100), ('missing', None)):
            marker = prefix + '-' + suffix
            mark = ctx.native.mark()
            params = {'role': 'assistant', 'text': marker}
            if timestamp is not None: params['timestamp'] = timestamp
            await ctx.native.call('message', **params)
            payload = emitted_payload(ctx, 'native_message', 'message', mark,
                lambda message: message.get('content') == [{'type': 'text', 'text': marker}])
            check(payload.get('role') == 'assistant', 'O13 native message has the requested source role')
            check(('timestamp' not in payload) if timestamp is None else
                  type(payload.get('timestamp')) is int and payload['timestamp'] == timestamp,
                  'O13 supplied/omitted native timestamp scenario was actually emitted', component='fixture')
            await ctx.wait_view(view, lambda reducer: any(
                kind == 'message' and row['value'].get('content') == payload['content'] and row['value'].get('final') is True
                for (kind, _), row in reducer.entities.items()))
            ident, public = unique_item(view.reducer.entities, 'message', marker)
            expected = check_times('message', payload, public, wire)
            retain({'kind': 'o13_native_time', 'wire': wire, 'entity': ['message', ident],
                             'native_payload': payload, 'public_value': public, 'expected': expected})
        marker = prefix + '-runtime-notice'
        mark = ctx.native.mark()
        await ctx.native.call('notice', type='auto_retry_start', attempt=1, delayMs=10, errorMessage=marker)
        notice = emitted_payload(ctx, 'native_notice', 'event', mark, lambda event: event.get('errorMessage') == marker)
        check(notice.get('type') == 'auto_retry_start', 'O13 notice source is an actual decoded runtime event', component='fixture')
        await ctx.wait_view(view, lambda reducer: any(
            kind == 'message' and marker in str(row['value'].get('content')) and row['value'].get('final') is True
            for (kind, _), row in reducer.entities.items()))
        ident, public = unique_item(view.reducer.entities, 'message', marker)
        check_notice_role(public)
        check_times('message', {}, public, wire)  # this actual notice event has no timestamp field
        retain({'kind': 'o13_native_notice', 'wire': wire, 'entity': ['message', ident],
                         'native_payload': notice, 'public_value': public})
    elif wire == 'codex':
        for supplied in (True, False):
            mark = ctx.native.mark()
            params = {'state': 'start'}
            if supplied: params['started_at'] = 1700000001
            await ctx.native.call('turn', **params)
            native = emitted_payload(ctx, 'turn', 'native_turn', mark, lambda turn: turn.get('status') == 'inProgress')
            run = native.get('id')
            check(isinstance(run, str) and bool(run), 'O13 native Turn supplies its actual run identity', component='fixture')
            if supplied:
                check(native.get('startedAt') == 1700000001, 'O13 supplied native seconds were emitted', component='fixture')
                await ctx.wait_view(view, lambda reducer: ('run', run) in reducer.entities)
                public = view.reducer.entities[('run', run)]['value']
                check_times('run', native, public, wire)
            else:
                check('startedAt' not in native and 'completedAt' not in native,
                      'O13 fresh native run omits time evidence', component='fixture')
                await ctx.wait_view(view, lambda reducer: reducer.entities.get(('state', 'current'), {}).get('value', {}).get('run') == run)
                public = view.reducer.entities.get(('run', run))
                if public is not None: check_times('run', native, public['value'], wire)
            retain({'kind': 'o13_native_time', 'wire': wire, 'entity': ['run', run],
                             'native_payload': native, 'public_value': public})
            mark = ctx.native.mark()
            params = {'state': 'settled'}
            if supplied: params.update(started_at=1700000001, completed_at=1700000003)
            await ctx.native.call('turn', **params)
            ended = emitted_payload(ctx, 'turn', 'native_turn', mark, lambda turn: turn.get('id') == run)
            if supplied:
                await ctx.wait_view(view, lambda reducer: reducer.entities.get(('run', run), {}).get('value', {}).get('ended_ms') == 1700000003000)
                public = view.reducer.entities[('run', run)]['value']
                check_times('run', ended, public, wire)
            else:
                check('startedAt' not in ended and 'completedAt' not in ended,
                      'O13 completed fresh native run still omits time evidence', component='fixture')
                await ctx.wait_view(view, lambda reducer: (reducer.entities.get(('state', 'current'), {}).get('value', {}).get('completion') or {}).get('id') == run)
                public = view.reducer.entities.get(('run', run))
                if public is not None: check_times('run', ended, public['value'], wire)
            retain({'kind': 'o13_native_time_end', 'wire': wire, 'entity': ['run', run],
                    'native_payload': ended, 'public_value': public})
    else:
        raise HarnessGap('O13 source-validated native metadata fixture is unavailable for this wire')
    return view, evidence


def written_event(ctx, wire, mark, predicate):
    rows = [row for row in ctx.native.records('native_event', after=mark)
            if row.get('wire') == wire and predicate(row)]
    if not rows:
        raise HarnessGap('O13 AFTER-write native_event ledger is missing for the omission scenario')
    check(len(rows) == 1, 'O13 omission event has exactly one native identity', component='fixture')
    payload = rows[0].get('payload')
    if not isinstance(payload, dict) or payload.get('omitted'):
        raise HarnessGap('O13 bounded metadata scenario needs its complete written native payload')
    return payload


def check_omitted_native_times(payload):
    # Do not silently treat a future/source-supported native time as unknown.
    check(not {'timestamp', 'at_ms', 'started_ms', 'ended_ms', 'startedAt', 'completedAt', 'createdAt'} & payload.keys(),
          'O13 omission scenario actually wrote no native timestamp fields', component='fixture')


async def observe_native_absence(ctx, view):
    """Exact AFTER-write event ledger (31893300), then actual public absence."""
    wire = ctx.manifest.get('wire', 'pi')
    marker = 'O13-tool-' + ctx.delivery()
    mark = ctx.native.mark()
    await ctx.native.call('turn', state='start')
    if wire == 'pi':
        native = written_event(ctx, wire, mark, lambda row: row.get('type') == 'agent_start')
        check_omitted_native_times(native)
        await ctx.wait_view(view, lambda reducer: bool(reducer.entities.get(('state', 'current'), {}).get('value', {}).get('run')))
        run = view.reducer.entities[('state', 'current')]['value']['run']
    elif wire == 'codex':
        payload = written_event(ctx, wire, mark, lambda row: row.get('method') == 'turn/started')
        native = payload.get('turn', {})
        check_omitted_native_times(native)
        run = native.get('id')
        check(isinstance(run, str) and bool(run), 'O13 written native turn has its actual id', component='fixture')
        await ctx.wait_view(view, lambda reducer: reducer.entities.get(('state', 'current'), {}).get('value', {}).get('run') == run)
    else:
        raise HarnessGap('O13 native omission scenario is unavailable for this wire')
    public_run = view.reducer.entities.get(('run', run))
    if public_run is not None: check_times('run', native, public_run['value'], wire)
    ctx.evidence.append({'kind': 'o13_native_run_absence', 'wire': wire, 'native_payload': native,
                         'public_run_id': run, 'public_entity': public_run})
    before = {key for key in view.reducer.entities if key[0] == 'tool'}
    tool_id = None
    for phase in ('start', 'end'):
        mark = ctx.native.mark()
        await ctx.native.call('tool_call', id_ref=marker, state=phase)
        if wire == 'pi':
            native = written_event(ctx, wire, mark, lambda row: row.get('type') == 'tool_execution_' + phase)
            check(native.get('toolCallId') == marker, 'O13 native tool id correlates with this controlled call', component='fixture')
        else:
            payload = written_event(ctx, wire, mark, lambda row: row.get('method') == ('item/started' if phase == 'start' else 'item/completed'))
            native = payload.get('item', {})
            check(native.get('id') == marker and native.get('type') == 'commandExecution',
                  'O13 written native tool is the controlled command item', component='fixture')
        check_omitted_native_times(native)
        if phase == 'start':
            await ctx.wait_view(view, lambda reducer: bool({key for key in reducer.entities if key[0] == 'tool'} - before))
            added = {key for key in view.reducer.entities if key[0] == 'tool'} - before
            check(len(added) == 1, 'O13 one native tool creates exactly one new public tool identity')
            tool_id = added.pop()
        else:
            await ctx.wait_view(view, lambda reducer: reducer.entities[tool_id]['value'].get('status') == 'completed')
        public = view.reducer.entities[tool_id]['value']
        check_times('tool', native, public, wire)
        ctx.evidence.append({'kind': 'o13_native_tool_absence', 'wire': wire, 'native_payload': native,
                             'public_entity': tool_id, 'public_value': public})
    if wire == 'codex':
        text = 'O13-message-missing-' + ctx.delivery()
        mark = ctx.native.mark()
        await ctx.native.call('retain', count=1, text=text)
        payload = written_event(ctx, wire, mark, lambda row: row.get('method') == 'item/completed')
        native = payload.get('item', {})
        check(native.get('type') == 'agentMessage' and text in native.get('text', ''),
              'O13 native untimed message was actually written', component='fixture')
        check_omitted_native_times(native)
        await ctx.wait_view(view, lambda reducer: reducer.entities.get(('message', native['id']), {}).get('value', {}).get('final') is True)
        public = view.reducer.entities[('message', native['id'])]['value']
        check_times('message', native, public, wire)
        ctx.evidence.append({'kind': 'o13_native_message_absence', 'wire': wire,
                             'native_payload': native, 'public_value': public})
    mark = ctx.native.mark()
    await ctx.native.call('turn', state='settled')
    finished = written_event(ctx, wire, mark, lambda row: row.get('type') == 'agent_end' if wire == 'pi' else row.get('method') == 'turn/completed')
    end_native = finished if wire == 'pi' else finished.get('turn', {})
    check_omitted_native_times(end_native)
    await ctx.wait_view(view, lambda reducer: not reducer.entities.get(('state', 'current'), {}).get('value', {}).get('run'))
    public_run = view.reducer.entities.get(('run', run))
    if public_run is not None: check_times('run', end_native, public_run['value'], wire)
    ctx.evidence.append({'kind': 'o13_native_run_end_absence', 'wire': wire,
                         'native_payload': end_native, 'public_run_id': run, 'public_entity': public_run})


def qualified_authority_refusal(error):
    """Known pre-effect core authorization refusal, never a provider admission."""
    check(error.kind == 'forbidden' and error.data.get('outcome') == 'rejected',
          'O13 authority probe requires an explicitly rejected core refusal', component='core')
    check('effect' not in error.data and 'effect' not in error.error,
          'O13 pre-effect authority refusal cannot carry an admitted effect', component='core')
    return error.error


async def authority_probe(ctx, observer, name, stage):
    """Protected principal config + unauthorized represented input, not raw recovery.

    Ordinary writes and framed undeclared sends may be valid. This observer may
    not mint standing authority or represent the actual workspace owner.
    """
    owner_key = 'doc/principals/owner'
    owner_entry = await ctx.peer.call('workspace.get', {'key': owner_key})
    owner = owner_entry.get('value') if isinstance(owner_entry, dict) else None
    check(isinstance(owner, str) and bool(owner), 'O13 reads the actual protected workspace owner', component='core')
    facts = await ctx.facts()
    send = {'resource': ctx.agents[0]['id'], 'op': 'agent.send', 'on_behalf_of': owner,
        'deadline_ms': 1000, 'args': {'v': 0, 'epoch': facts.get('epoch'),
            'caps_rev': facts.get('caps', {}).get('rev'), 'mode': 'start',
            'delivery': name + '-' + stage, 'content': [{'type': 'text', 'text': name}],
            'origin': {'kind': 'human'}, 'render': ['attributed_user']}}
    mark = ctx.native.mark()
    refused = []
    for method, params in (
        ('workspace.apply', {'ops': [{'op': 'set', 'key': owner_key, 'value': owner}]}),
        ('resource.invoke', send),
    ):
        try:
            reply = await observer.call(method, params)
        except RpcError as error:
            refused.append({'method': method, 'error': qualified_authority_refusal(error)})
        else:
            check(False, 'O13 observation must not authorize protected or represented work: ' + str(reply), component='core')
    # This real owner commit fences the serialized core mutation queue after the
    # qualified result. No admitted provider effects are accepted by this probe.
    fence = 'doc/o13/fence/' + name + '-' + stage
    adopted = await ctx.peer.call('workspace.apply', {'ops': [{'op': 'set', 'key': fence, 'value': stage}]})
    check(type(adopted.get('txn')) is int and adopted['txn'] > 0, 'O13 public authority fence has a transaction receipt', component='core')
    fence_entry = await ctx.peer.call('workspace.get', {'key': fence})
    check(isinstance(fence_entry, dict) and fence_entry.get('value') == stage,
          'O13 public authority fence was actually adopted', component='core')
    current = await ctx.peer.call('workspace.get', {'key': owner_key})
    check(current == owner_entry, 'O13 protected principal entry remains unchanged after the ordering fence', component='core')
    check(not ctx.native.writes(after=mark), 'O13 qualified authority refusals have no native writes after the public fence', component='core')
    return {'protected_owner': owner, 'refusals': refused, 'fence': adopted, 'owner_entry': current}


async def observe_public_rendering(ctx):
    """Generic public SDK producer + actual pinned production renderer.

    This is deliberately NOT native Pi/Codex metadata support certification.
    """
    root = ctx.manifest.get('viewer_extensions')
    revision = ctx.manifest.get('viewer_revision')
    hashes = ctx.manifest.get('viewer_sha256')
    if not isinstance(root, str) or not isinstance(hashes, dict):
        raise HarnessGap('O13 current-kit viewer_extensions/viewer_revision/viewer_sha256 are not staged')
    sdk = Path(root)
    if not sdk.is_absolute() or not sdk.is_dir():
        raise HarnessGap('O13 immutable production viewer/SDK directory is absent')
    for filename in ('vqro_provider.py', 'vqro_provider_stream.py'):
        if not (sdk / filename).is_file():
            raise HarnessGap('O13 public producer SDK dependency absent: ' + filename)
    # viewer_sha256 declares the top-level viewer/SDK module closure (same
    # contract as O14), not unrelated client and pi_bridge packages.
    for path in sdk.glob('*.py'):
        relative = path.relative_to(sdk).as_posix()
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != hashes.get(relative):
            raise FixtureFailed('O13 production viewer/SDK source pin mismatch: ' + relative)
    name = 'o13-' + hashlib.sha256(ctx.delivery().encode()).hexdigest()[:16]
    content = [{'type': 'text', 'text': name + '-plain', 'format': 'plain'},
               {'type': 'text', 'text': name + '-default'},
               {'type': 'text', 'text': '**' + name + '-markdown**', 'format': 'markdown'},
               {'type': 'text', 'text': '**' + name + '-unknown**', 'format': 'future-format'}]
    records = [{'op': 'put', 'kind': 'message', 'id': 'formats', 'rev': 1,
                'value': {'position': 1, 'role': 'assistant', 'content': content, 'final': True}},
               {'op': 'put', 'kind': 'message', 'id': 'system', 'rev': 1,
                'value': {'position': 2, 'role': 'system', 'content': [{'type': 'text', 'text': name + '-system'}], 'final': True}},
               {'op': 'put', 'kind': 'message', 'id': 'coordination', 'rev': 1,
                'value': {'position': 3, 'role': 'coordination', 'content': [{'type': 'text', 'text': name + '-coordination'}], 'final': True}}]
    # Validate the consumer identity before creating any owned producer.
    _, renderer = render_production_blocks(content, sdk, revision, hashes)
    script, spec, log = (ctx.root / (name + suffix) for suffix in ('.py', '.json', '.jsonl'))
    script.write_text(PUBLIC_PROVIDER)
    spec.write_text(json.dumps({'name': name, 'epoch': name + '-e1', 'records': records}) + '\n')
    process, owned = None, None
    observer = None
    try:
        with (ctx.root / (name + '.log')).open('ab') as output:
            process = subprocess.Popen(['/usr/bin/python3', str(script), str(sdk), str(spec), str(log)],
                cwd=ctx.root / 'work', env=dict(ctx.backend.env), stdout=output, stderr=subprocess.STDOUT)
        owned = identity(process.pid)
        if owned: ctx.backend.tracked.append(owned)
        async def resource_ready():
            while True:
                if process.poll() is not None: raise FixtureFailed('O13 public producer exited before registration')
                rows = [row for row in await ctx.resources() if row.get('backend_ref') == name and row.get('available')]
                if len(rows) == 1: return rows[0]
                await asyncio.sleep(.01)
        resource = await asyncio.wait_for(resource_ready(), 5)
        observer = await Peer.connect(ctx.backend.env['VQRO_SOCKET'], max_record=8 << 20)
        ctx.peers.append(observer)
        hello = await observer.call('hello', {'client': 'O13-public-observer'})
        check(hello.get('actor') == 'undeclared', 'O13 observer starts without inherited input authority', component='core')
        for method in ('stream.data', 'stream.closed', 'stream.gap'):
            observer.on(method, lambda params, method=method: ctx._notice(observer, method, params))
        before = await authority_probe(ctx, observer, name, 'before')
        view = await ctx.attach(rid=resource['id'], peer=observer)
        await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0 and all(
            ('message', ident) in reducer.entities for ident in ('formats', 'system', 'coordination')))
        actual = view.reducer.entities[('message', 'formats')]['value']['content']
        check(actual == content, 'O13 generic public producer content reaches the observer unchanged')
        rows, renderer = render_production_blocks(actual, sdk, revision, hashes)
        system = view.reducer.entities[('message', 'system')]['value']
        coordination = view.reducer.entities[('message', 'coordination')]['value']
        check_notice_role(system)
        check(coordination.get('role') == 'coordination' and coordination.get('role') != system['role'],
              'O13 observed system and coordination roles remain distinct')
        after = await authority_probe(ctx, observer, name, 'after')
        emitted = [json.loads(line) for line in log.read_text().splitlines()]
        check(any(row.get('kind') == 'snapshot_sent' and row.get('records') == records for row in emitted),
              'O13 actual SDK producer sent exactly the observed metadata records', component='fixture')
        ctx.evidence.append({'kind': 'o13_public_renderer', 'component': renderer,
            'source': 'generic public SDK provider, NOT native adapter metadata',
            'producer_sha256': hashlib.sha256(PUBLIC_PROVIDER.encode()).hexdigest(),
            'public_resource': resource, 'actual_public_content': actual, 'rendered_rows': rows,
            'system': system, 'coordination': coordination, 'observer_hello': hello,
            'authority_before': before, 'authority_after': after, 'producer_log': str(log)})
    finally:
        if observer is not None: await observer.close()
        if process is not None:
            if process.poll() is None: process.terminate()
            try: await asyncio.to_thread(process.wait, 4)
            except subprocess.TimeoutExpired:
                process.kill(); await asyncio.to_thread(process.wait, 4)
            ctx.evidence.append({'kind': 'o13_public_producer_cleanup', 'reaped': process.poll() is not None,
                                 'identity': identity(process.pid), 'original_identity': owned})


@case('O13')
async def optional_item_metadata(ctx):
    # All clauses are mandatory to exercise; optional facts may be unknown, but
    # missing source/wire/consumer fixtures are honest applicable harness gaps.
    try:
        view, _ = await observe_native_metadata(ctx)
        await observe_native_absence(ctx, view)
        await observe_public_rendering(ctx)
    except FixtureFailed as error:
        if str(error).startswith(('unimplemented fixture verb:', 'unimplemented native Codex fixture verb:')):
            raise HarnessGap('O13 native metadata producer controls are not installed: ' + str(error)) from error
        raise
