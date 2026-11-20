#!/usr/bin/env python3
"""Public-RPC regressions for coordination review: managed extension, real core,
fake provider, no native runtime/model or live endpoint. Derived from the
independent review driver. Usage: --binary PATH [--extensions PATH] [case ...].
An explicit core binary with C1/C2 support is required; roots are private.
"""
import argparse, json, os, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, required=True, help='explicit disposable-test core binary')
parser.add_argument('--extensions', type=Path, default=Path(__file__).resolve().parents[1] / 'extensions')
parser.add_argument('cases', nargs='*')
ARGS = parser.parse_args()
W = Path.home() / '.local/share/herdr/scratch/vqro-coordination-review'
W.mkdir(parents=True, exist_ok=True)
BIN = ARGS.binary.resolve()
EXT = ARGS.extensions.resolve()
ROOT = Path(tempfile.mkdtemp(prefix='k2c-', dir=W))
for name in ('home', 'run', 'state', 'extensions'):
    (ROOT / name).mkdir(mode=0o700)
ENV = {'HOME': str(ROOT / 'home'), 'PATH': '/usr/bin:/bin', 'XDG_RUNTIME_DIR': str(ROOT / 'run'),
       'VQRO_STATE_DIR': str(ROOT / 'state'), 'VQRO_SOCKET': str(ROOT / 'p'),
       'VQRO_EXTENSIONS_DIR': str(ROOT / 'extensions'), 'VQRO_HERDR': '["/bin/false"]',
       'HC_SOCKET': str(ROOT / 'h'), 'HC_SESSION': 'k2-review'}


class RPCError(Exception):
    def __init__(self, error):
        super().__init__(error.get('message'))
        self.kind = (error.get('data') or {}).get('kind')


class Client:
    def __init__(self, identity=None, handler=None):
        self.handler = handler
        self.sock = socket.socket(socket.AF_UNIX); self.sock.connect(str(ROOT / 'p'))
        self.file = self.sock.makefile('rb'); self.lock = threading.Lock(); self.cv = threading.Condition()
        self.next, self.replies, self.closed = 0, {}, False
        threading.Thread(target=self.read, daemon=True).start()
        hello = {'client': 'k2-review'}
        if identity: hello['identity'] = identity
        self.call('hello', hello)

    def read(self):
        try:
            for line in self.file:
                msg = json.loads(line)
                if 'method' in msg and 'id' in msg:
                    if self.handler:
                        try:
                            result = self.handler(msg['method'], msg.get('params') or {})
                            self.send({'jsonrpc': '2.0', 'id': msg['id'], 'result': result})
                        except Exception as error:
                            self.send({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32000, 'message': str(error), 'data': {'kind': 'backend'}}})
                    else:
                        self.send({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32000, 'message': 'no', 'data': {'kind': 'unsupported'}}})
                elif 'method' not in msg:
                    with self.cv: self.replies[msg['id']] = msg; self.cv.notify_all()
        except (OSError, ValueError):
            pass
        finally:
            with self.cv: self.closed = True; self.cv.notify_all()

    def send(self, value):
        with self.lock: self.sock.sendall((json.dumps(value) + '\n').encode())

    def call(self, method, params=None, timeout=20):
        with self.cv: self.next += 1; ident = self.next
        self.send({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params or {}})
        with self.cv:
            if not self.cv.wait_for(lambda: ident in self.replies or self.closed, timeout): raise TimeoutError(method)
            if ident not in self.replies: raise RuntimeError('core closed during ' + method)
            reply = self.replies.pop(ident)
        if 'error' in reply: raise RPCError(reply['error'])
        return reply['result']


def wait(fn, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            v = fn()
            if v: return v
        except (OSError, RPCError, KeyError):
            pass
        time.sleep(.05)
    raise TimeoutError('settle')


owner = None
OUT = {'root': str(ROOT), 'core_binary': str(BIN), 'extensions': str(EXT), 'cases': {}}


def cmd(name, args=None, client=None):
    return (client or owner).call('command.invoke', {'name': 'coord.' + name, 'args': args or {}})


def op(label):
    return cmd('inbox.window')['epoch'] + ':' + label + ':' + str(time.monotonic_ns())


def scope(action):
    # Provider facts may finish arriving between the two RPCs. A rejected stale
    # preview made no gate change; obtain a new cut and a new operation ID.
    for attempt in range(4):
        preview = cmd('scope', {'action': 'preview', 'scope': 'workspace'})
        try:
            return cmd('scope', {'op': op(action), 'action': action, 'scope': 'workspace', 'expect': preview['expect']})
        except RPCError as error:
            if error.kind != 'conflict' or 'scope preview is stale' not in str(error) or attempt == 3:
                raise


def raw_set(key, value):
    cur = owner.call('workspace.get', {'key': key})
    return owner.call('workspace.apply', {'ops': [{'op': 'set', 'key': key, 'value': value, 'expect': cur['version'] if cur else 0}]})


PROVIDERS = []
GEN = {}


def agent(ref, native, name=None, instance='stable', incarnation='i1', handler=None):
    name = name or ('k2-' + ref)
    """Register (once per name) a fake backend and publish one agent resource; return its rid."""
    peer = Client(handler=handler)
    reg = peer.call('provider.register', {'name': name, 'role': 'backend', 'instance': instance,
        'software': {'name': 'fake', 'version': '0'}, 'provider_version': '0',
        'capabilities': {'agent': {}, **{key: {} for key in native.get('caps', {}) if key.startswith('agent.')}}})
    facts = {'ref': ref, 'incarnation': incarnation, 'kind': 'agent', 'state': 'live',
             'status': {'state': native.get('state', 'idle'), 'app': 'fake'}, 'data': {'agent': native}}
    peer.call('provider.sync', {'generation': reg['generation'], 'resources': [facts], 'final': True})
    PROVIDERS.append(peer)
    GEN[ref] = (peer, reg['generation'], facts)
    return wait(lambda: next((r['id'] for r in owner.call('workspace.snapshot')['resources']
                              if r.get('provider') == name and r.get('backend_ref') == ref), None))


def native(state='idle', pull=None, principal=None):
    n = {'v': 0, 'state': state, 'lifecycle': 'live', 'session': {'id': 's1'}, 'epoch': 'e1',
         'host': {'opaque': 'fixture', 'owns_lifecycle': False}, 'caps': {'rev': 1}}
    if pull: n['pull'] = pull
    if principal: n['principal'] = principal
    return n


def case_idle():
    pull = {'points': {'idle': {'precision': 'approximate'}}, 'insert': 'context', 'evidence': 'none',
            'framing': {'idle': {'role': 'context', 'frame': 'system_reminder'}}}
    rid = agent('idle-agent', native('running', pull)); scope('resume')
    sent = cmd('inbox.send', {'op': (o := op('idle-send')), 'submission': o, 'to': rid, 'mode': 'start', 'render': ['context'],
                              'content': [{'type': 'text', 'text': 'start while busy'}]})
    cmd('inbox.reconcile', {'op': op('idle-rec'), 'agent': rid, 'consumer': 'c-idle', 'records': []})
    took = cmd('inbox.take', {'op': op('idle-take'), 'agent': rid, 'consumer': 'c-idle', 'at': 'idle', 'accept': ['text'], 'max_bytes': 5000, 'limit': 1})
    return {'agent_state': 'running', 'precision': 'approximate', 'mode': 'start',
            'claimed': [r['delivery'] for r in took.get('records', [])], 'held': took.get('held'),
            'violation': bool(took.get('records'))}


def case_membership(rounds=20):
    a = agent('m-a', native()); b = agent('m-b', native()); scope('resume')
    cycles, outcomes = 0, []
    for i in range(rounds):
        raw_set(f'doc/agents/members/{a}', {'project': 'p', 'reports_to': None})
        raw_set(f'doc/agents/members/{b}', {'project': 'p', 'reports_to': None})
        va = owner.call('workspace.get', {'key': f'doc/agents/members/{a}'})['version']
        vb = owner.call('workspace.get', {'key': f'doc/agents/members/{b}'})['version']
        c1, c2 = Client({'kind': 'owner'}), Client({'kind': 'owner'})
        res = {}
        def move(client, rid, parent, ver, tag):
            try: res[tag] = cmd('membership.set', {'op': op('mv' + tag + str(i)), 'agent': rid, 'project': 'p', 'reports_to': parent, 'expect': ver}, client)['state']
            except RPCError as e: res[tag] = 'err:' + str(e.kind)
            except Exception as e: res[tag] = 'exc:' + repr(e)[:80]
        t = [threading.Thread(target=move, args=(c1, a, b, va, 'a')), threading.Thread(target=move, args=(c2, b, a, vb, 'b'))]
        [x.start() for x in t]; [x.join(20) for x in t]
        ma = owner.call('workspace.get', {'key': f'doc/agents/members/{a}'})['value']
        mb = owner.call('workspace.get', {'key': f'doc/agents/members/{b}'})['value']
        cyc = ma.get('reports_to') == b and mb.get('reports_to') == a
        outcomes.append((res.get('a'), res.get('b'), cyc))
        if cyc:
            cycles += 1
            try: after = cmd('status')['state']
            except RPCError as e: after = 'err:' + str(e.kind) + ':' + str(e)
            try:
                o2 = op('after-cycle'); cmd('inbox.send', {'op': o2, 'submission': o2, 'to': a, 'mode': 'start', 'content': [{'type': 'text', 'text': 'x'}]}); send_after = 'accepted'
            except RPCError as e: send_after = 'err:' + str(e.kind) + ': ' + str(e)
            return {'rounds': i + 1, 'cycle_formed': True, 'outcomes': outcomes[-3:], 'status_after': after, 'send_after_cycle': send_after}
    return {'rounds': rounds, 'cycle_formed': False, 'outcomes': outcomes[-5:]}


ORDINARY = []
def case_identity():
    out = {}
    ORDINARY.append(agent('ordinary', native())); scope('resume')
    for tag, inst, ref in (('ascii', 'i' * 256, 'r' * 128), ('control', '\x01' * 256, 'c' * 128), ('nonascii', '\u00e9' * 128, 'e' * 128)):
        try:
            rid = agent(ref, native(), name=('p' * 120) + tag[:8], instance=inst, incarnation='n' * 128)
        except Exception as e:
            out[tag] = {'admitted': False, 'error': repr(e)[:200]}; continue
        try:
            scope('resume'); out[tag] = {'admitted': True, 'workspace_resume': 'ok'}
        except RPCError as e:
            out[tag] = {'admitted': True, 'workspace_resume': 'err:' + str(e.kind) + ': ' + str(e)}
            try: out[tag]['status_after'] = cmd('status')['state']
            except RPCError as e2: out[tag]['status_after'] = 'err:' + str(e2.kind)
            try:
                o = op('other'); cmd('inbox.send', {'op': o, 'submission': o, 'to': ORDINARY[0], 'mode': 'start', 'content': [{'type': 'text', 'text': 'y'}]})
                out[tag]['ordinary_agent_send'] = 'accepted'
                out[tag]['ordinary_agent_record'] = cmd('inbox.list', {'target': ORDINARY[0]})
            except RPCError as e3: out[tag]['ordinary_agent_send'] = 'err:' + str(e3.kind) + ': ' + str(e3)
            return out
        try:
            o = op('big-id-' + tag); r = cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'start', 'content': [{'type': 'text', 'text': 'x'}]})
            out[tag].update(send='accepted', state=r.get('state'))
        except RPCError as e:
            out[tag].update(send='refused', kind=e.kind, error=str(e))
    return out


def case_reconcile():
    pull = {'points': {'tool': {'precision': 'exact'}}, 'insert': 'user', 'evidence': 'entry',
            'framing': {'tool': {'role': 'user', 'frame': 'sender_header'}}}
    rid = agent('rec-agent', native('idle', pull)); scope('resume')
    raw_set(f'doc/agents/grants/all-admit-{rid}', {'who': '*', 'on': f'agent:{rid}', 'can': ['send:admit']})
    cmd('inbox.reconcile', {'op': op('rec0'), 'agent': rid, 'consumer': 'c0', 'records': []})
    senders = {}
    rows, deliveries = [], []
    for batch in range(4):
        for n in range(16):
            p = 'p:s' + str(batch * 2 + n // 8)
            senders.setdefault(p, Client({'kind': 'principal', 'principal': p}))
            o = op(f's{batch}-{n}')
            cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'admit', 'send_new': True, 'content': [{'type': 'text', 'text': 'm'}]}, senders[p])
        took = cmd('inbox.take', {'op': op(f't{batch}'), 'agent': rid, 'consumer': f'c{batch}', 'at': 'tool', 'accept': ['text'], 'max_bytes': 5000, 'limit': 16})
        deliveries.extend(row['delivery'] for row in took.get('records', []))
        try:
            r = cmd('inbox.reconcile', {'op': op(f'r{batch}'), 'agent': rid, 'consumer': f'c{batch + 1}', 'records': []})
            pages, processed = 1, len(r.get('records', []))
            while r['state'] == 'reconciling' and pages < 32:
                held = cmd('inbox.take', {'op': op('mid-reconcile'), 'agent': rid, 'consumer': f'c{batch + 1}', 'at': 'tool'})
                assert held['records'] == [] and held['held'] == 'reconciling', held
                prior = r['remaining']
                r = cmd('inbox.reconcile', {'op': op('rec-next'), 'agent': rid, 'consumer': f'c{batch + 1}', 'records': []})
                assert r['remaining'] < prior, r
                processed += len(r['records']); pages += 1
            assert r['state'] == 'reconciled' and len(took['records']) == 16, (r, took)
            rows.append({'batch': batch, 'taken': len(took.get('records', [])), 'reconciled_unknown': processed, 'pages': pages, 'state': r['state']})
        except RPCError as e:
            rows.append({'batch': batch, 'taken': len(took.get('records', [])), 'reconcile': 'err:' + str(e.kind) + ': ' + str(e)})
            try:
                t2 = cmd('inbox.take', {'op': op('t-after'), 'agent': rid, 'consumer': f'c{batch}', 'at': 'tool', 'accept': ['text'], 'max_bytes': 5000, 'limit': 16})
                rows.append({'take_after_failed_reconcile': t2.get('held') or len(t2.get('records', []))})
            except RPCError as e2:
                rows.append({'take_after_failed_reconcile': 'err:' + str(e2.kind)})
            break
    assert len(deliveries) == 64, deliveries
    proofs = [{'delivery': delivery, 'entry': 'entry-' + delivery} for delivery in deliveries]
    positive = cmd('inbox.reconcile', {'op': op('positive'), 'agent': rid, 'consumer': 'positive', 'records': proofs})
    assert positive['state'] == 'reconciling' and positive['remaining'] == 8, positive
    held = cmd('inbox.take', {'op': op('positive-mid'), 'agent': rid, 'consumer': 'positive', 'at': 'tool'})
    assert held['records'] == [] and held['held'] == 'reconciling', held
    final = cmd('inbox.reconcile', {'op': op('positive-final'), 'agent': rid, 'consumer': 'positive', 'records': proofs})
    assert final['state'] == 'reconciled' and len(final['records']) == 8, final
    return {'rows': rows, 'positive_pages': 2, 'positive_remaining': final['remaining']}


def case_names():
    a = agent('n-a', native(principal='p:legit')); b = agent('n-b', native()); scope('resume')
    raw_set(f'doc/agents/members/{a}', {'project': 'p', 'reports_to': None})
    raw_set(f'doc/agents/grants/names-only-{a}', {'who': 'p:attacker', 'on': f'agent:{a}', 'can': ['names']})
    raw_set(f'doc/agents/grants/team-send-{b}', {'who': f'team:{a}', 'on': f'agent:{b}', 'can': ['send:start']})
    attacker = Client({'kind': 'principal', 'principal': 'p:attacker'})
    def try_send(tag):
        try:
            o = op(tag); cmd('inbox.send', {'op': o, 'submission': o, 'to': b, 'mode': 'start', 'content': [{'type': 'text', 'text': 'from attacker'}]}, attacker)
            return 'accepted'
        except RPCError as e:
            return 'err:' + str(e.kind)
    before = try_send('before')
    try:
        bound = cmd('names', {'op': op('bind'), 'handle': '@root', 'agent': a, 'expect': 0, 'endpoint_principal': 'p:attacker'}, attacker)
        bound = bound.get('outcome', bound)
    except RPCError as e:
        bound = 'err:' + str(e.kind) + ': ' + str(e)
    after = try_send('after')
    return {'send_before_names': before, 'names_bind': bound, 'send_after_names': after,
            'escalated': before.startswith('err') and after == 'accepted'}


def case_brick():
    good = agent('good', native()); scope('resume')
    big = agent('c' * 128, native(), name=('p' * 120) + 'ctl', instance='\x01' * 256, incarnation='n' * 128)
    out = {}
    for action in ('pause', 'resume'):
        try: scope(action); out[action] = 'ok'
        except RPCError as e: out[action] = 'err:' + str(e.kind) + ': ' + str(e)
    try:
        preview = cmd('scope', {'action': 'preview', 'scope': 'agent:' + good})
        cmd('scope', {'op': op('single'), 'action': 'resume', 'scope': 'agent:' + good, 'expect': preview['expect']}); out['resume_only_good_agent'] = 'ok'
    except RPCError as e: out['resume_only_good_agent'] = 'err:' + str(e.kind) + ': ' + str(e)
    return out


def case_rawcycle():
    pull = {'points': {'tool': {'precision': 'exact'}}, 'insert': 'user', 'evidence': 'entry'}
    a = agent('raw-a', native(principal='p:forest-native'))
    b = agent('raw-b', native(pull=pull))
    raw_set(f'doc/agents/members/{a}', {'project': 'broken', 'reports_to': b})
    raw_set(f'doc/agents/members/{b}', {'project': 'broken', 'reports_to': a})
    for tag, who, on in [('recovery', 'p:recovery', '*'), ('project-only', 'p:project-only', 'project:broken'),
                          ('exact-only', 'p:exact-only', f'agent:{b}'), ('team-only', 'p:team-only', f'team:{a}'),
                          ('team-subject', f'team:{a}', f'agent:{b}')]:
        raw_set(f'doc/agents/grants/{tag}', {'who': who, 'on': on,
                'can': ['pause', 'resume', 'send:start', 'receive']})
    status = wait(lambda: (s if not (s := cmd('status'))['configuration']['membership_valid'] else None))
    assert 'cycle' in json.dumps(status['configuration']) and a in json.dumps(status['configuration']), status
    sent = []
    clients = {}
    for principal in ('p:recovery', 'p:project-only', 'p:exact-only'):
        client = clients[principal] = Client({'kind': 'principal', 'principal': principal})
        o = op(principal)
        result = cmd('inbox.send', {'op': o, 'submission': o, 'to': b, 'mode': 'start', 'send_new': True,
                     'content': [{'type': 'text', 'text': principal}]}, client)
        assert result['state'] in ('recorded', 'held'), result
        sent.append(result['delivery'])
    selected = {}
    for target, principal in [('workspace', 'p:recovery'), ('project:broken', 'p:project-only'), (f'agent:{b}', 'p:exact-only')]:
        for action in ('pause', 'resume'):
            preview = cmd('scope', {'action': 'preview', 'scope': target})
            result = cmd('scope', {'op': op(target + action), 'action': action, 'scope': target,
                                  'expect': preview['expect']}, clients[principal])
            assert result['state'] == 'accepted', result
            selected[target] = result['selected']
    known = {row['id'] for row in owner.call('workspace.snapshot')['resources']
             if row.get('kind') == 'agent' or (row.get('status') or {}).get('app')}
    assert selected['workspace'] == len(known) and selected['project:broken'] == 2 and selected[f'agent:{b}'] == 1, selected
    try:
        cmd('scope', {'action': 'preview', 'scope': f'team:{a}'})
        raise AssertionError('invalid team scope was expanded')
    except RPCError as error:
        assert error.kind == 'membership_invalid' and 'cycle' in str(error), str(error)
    for principal in ('p:team-only', 'p:forest-native'):
        client = Client({'kind': 'principal', 'principal': principal})
        try:
            o = op('blocked-team')
            cmd('inbox.send', {'op': o, 'submission': o, 'to': b, 'mode': 'start', 'send_new': True,
                               'content': [{'type': 'text', 'text': 'must not expand team'}]}, client)
            raise AssertionError('invalid team grant authorized a send')
        except RPCError as error:
            assert error.kind == 'membership_invalid' and 'cycle' in str(error), str(error)
    cmd('inbox.reconcile', {'op': op('raw-reconcile'), 'agent': b, 'consumer': 'raw-consumer', 'records': []}, clients['p:recovery'])
    o = op('raw-pull-send')
    delivery = cmd('inbox.send', {'op': o, 'submission': o, 'to': b, 'mode': 'admit', 'send_new': True,
                                'content': [{'type': 'text', 'text': 'pull remains usable'}]})['delivery']
    took = cmd('inbox.take', {'op': op('raw-pull'), 'agent': b, 'consumer': 'raw-consumer', 'at': 'tool'}, clients['p:recovery'])
    assert [r['delivery'] for r in took['records']] == [delivery], took
    # The durable cycle remains exactly as written: no quiet repair.
    assert owner.call('workspace.get', {'key': f'doc/agents/members/{a}'})['value']['reports_to'] == b
    assert owner.call('workspace.get', {'key': f'doc/agents/members/{b}'})['value']['reports_to'] == a
    return {'selected': selected, 'direct_sends': len(sent), 'pull_claims': 1, 'team_denied': 2,
            'cycle_visible': True, 'raw_cycle_preserved': True}


def case_invalid_declaration():
    rid = agent('bad-point-list', native(pull={'points': ['tool'], 'insert': 'user', 'evidence': 'entry'}))
    scope('resume')
    cmd('inbox.reconcile', {'op': op('list-reconcile'), 'agent': rid, 'consumer': 'list-consumer', 'records': []})
    try:
        cmd('inbox.take', {'op': op('list-take'), 'agent': rid, 'consumer': 'list-consumer', 'at': 'tool'})
        raise AssertionError('list points silently accepted')
    except RPCError as error:
        assert error.kind == 'invalid' and 'declaration_invalid' in str(error), str(error)
        return {'explicit_invalid_declaration': True}


def case_window():
    rid = agent('window', native(pull={'points': {'tool': {'precision': 'exact'}}, 'insert': 'user', 'evidence': 'entry'}))
    scope('resume')
    sent_op = op('window-send')
    sent = cmd('inbox.send', {'op': sent_op, 'submission': sent_op, 'to': rid, 'mode': 'admit',
                            'content': [{'type': 'text', 'text': 'lost take'}]})
    cmd('inbox.reconcile', {'op': op('window-rec'), 'agent': rid, 'consumer': 'window-c', 'records': []})
    take_op = op('window-take')
    take_args = {'op': take_op, 'agent': rid, 'consumer': 'window-c', 'at': 'tool'}
    took = cmd('inbox.take', take_args)
    assert len(took['records']) == 1, took
    original = took['records'][0]
    epoch = cmd('inbox.window')['epoch']
    count = 0
    while cmd('inbox.window')['epoch'] == epoch and count < 1300:
        try:
            cmd('inbox.report', {'op': op('rotate'), 'delivery': 'd:missing', 'claim': 'x', 'stage': 'read'})
        except RPCError:
            pass
        count += 1
    assert cmd('inbox.window')['epoch'] != epoch, count
    frozen = cmd('inbox.get', {'delivery': sent['delivery']})
    historical_replays = 0
    # Rollover expires admission, not necessarily a lazy-cached result. Observe
    # actual eviction instead of assuming64 unrelated hash-bucket writes did it.
    for compacted in range(cmd('inbox.window')['to'] + 1):
        before = cmd('inbox.window')
        try:
            replay = cmd('inbox.take', take_args)
        except RPCError as error:
            assert error.kind == 'expired_operation', str(error)
            assert cmd('inbox.window') == before, 'expired take mutated its operation window'
            break
        assert replay.get('historical') is True and replay['records'] == [], replay
        assert len(replay['claims']) == 1 and replay['claims'][0]['claim'] == original['claim'], replay
        assert replay['claims'][0]['delivery'] == sent['delivery'], replay
        assert cmd('inbox.get', {'delivery': sent['delivery']}) == frozen, 'historical take changed its claim'
        assert cmd('inbox.window') == before, 'historical take mutated its operation window'
        historical_replays += 1
        try:
            cmd('inbox.report', {'op': op('compact'), 'delivery': 'd:missing', 'claim': 'x', 'stage': 'read'})
        except RPCError:
            pass
    else:
        raise AssertionError('fixture could not observe take-result eviction in a bounded operation window')
    history = cmd('inbox.op', {'op': take_op})
    assert history['records'] == [] and len(history['claims']) == 1, history
    assert history['claims'][0]['delivery'] == sent['delivery'] and history['claims'][0]['claim'] == original['claim'], history
    assert cmd('inbox.get', {'delivery': sent['delivery']}) == frozen
    return {'refusals_to_rotate': count, 'compactions_to_expiry': compacted, 'expiry_observed': True,
            'historical_replays': historical_replays, 'history_claims': 1, 'content_replayed': False}


def case_handover():
    received = []
    def handle(method, params):
        assert method == 'effect.invoke' and params['op'] == 'agent.send', (method, params)
        origin = params['args']['origin']
        assert origin['principal'] == params['principal'] and origin['actor'] == params['actor'], (origin, params)
        assert origin['sender']['principal'] == 'owner' and origin['sender']['provenance'] == 'inbox-record', origin
        received.append(params['args'])
        return {'outcome': 'accepted', 'data': {'receipt': {'stage': 'admitted'}}}
    dst_native = native()
    dst_native['session'] = {'id': 's2'}
    dst_native['caps']['agent.send'] = {'modes': {'start': {'busy': 'reject'}},
        'origin': [{'as': 'attributed_user', 'role': 'user', 'frame': 'sender_header'}]}
    dst = agent('successor', dst_native, handler=handle)
    def prepare(ref, stages=None):
        src = agent(ref, native())
        scope('pause')
        o = op(ref)
        sent = cmd('inbox.send', {'op': o, 'submission': o, 'to': src, 'mode': 'start',
                                'content': [{'type': 'text', 'text': ref}]})
        peer, generation, facts = GEN[ref]
        facts['data']['agent']['lifecycle'] = 'ended'
        peer.call('provider.sync', {'generation': generation, 'resources': [facts], 'final': True})
        wait(lambda: cmd('inbox.get', {'delivery': sent['delivery']})['reason'] == 'agent_ended')
        args = {'op': op('handover'), 'from': {'agent': src, 'incarnation': 'i1'},
                'to': {'agent': dst, 'incarnation': 'i1'}, 'deliveries': [sent['delivery']]}
        if stages:
            args['bind'] = stages
        return src, sent, cmd('handover', args)['plan']
    src, sent, plan = prepare('predecessor')
    completed = cmd('handover', {'op': op('complete'), 'action': 'complete', 'plan': plan})
    assert completed['state'] == 'completed', completed
    record = cmd('inbox.get', {'delivery': sent['delivery']})
    assert record['to']['session'] == 's2' and record['handed_over_from']['session'] == 's1', record
    assert not received, received
    scope('resume')
    wait(lambda: cmd('inbox.get', {'delivery': sent['delivery']})['state'] == 'accepted')
    assert len(received) == 1 and received[0]['guard']['session'] == 's2', received
    stages = [{'handle': '@accepted-before-abort', 'agent': dst, 'expect': 0},
              {'handle': '@bad-before-abort', 'agent': dst, 'expect': 99}]
    src2, second, plan2 = prepare('abort-predecessor', stages)
    cmd('handover', {'op': op('good-stage'), 'action': 'stage', 'plan': plan2, 'stage': 0})
    try:
        cmd('handover', {'op': op('bad-stage'), 'action': 'stage', 'plan': plan2, 'stage': 1})
        raise AssertionError('bad stage accepted')
    except RPCError as error:
        assert error.kind == 'conflict', str(error)
    aborted = cmd('handover', {'op': op('abort'), 'action': 'abort', 'plan': plan2})
    assert aborted['state'] == 'aborted' and aborted['configuration'] == 'not_reverted', aborted
    restored = cmd('inbox.get', {'delivery': second['delivery']})
    assert restored['to']['agent'] == src2 and restored['state'] == 'held' and not restored.get('handover_plan'), restored
    assert owner.call('workspace.get', {'key': 'doc/agents/names/@accepted-before-abort'})['value']['agent'] == dst
    assert len(received) == 1, received
    return {'successor_session': 's2', 'native_writes': 1, 'sender_provenance': 'inbox-record',
            'transport_matches_core': True, 'abort_restores_held': True, 'accepted_config_not_reverted': True}


def case_archive():
    rid = agent('archive', native())
    scope('pause')
    o = op('archive-send')
    sent = cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'start',
                            'content': [{'type': 'text', 'text': 'archive evidence'}]})
    destination = ROOT / 'archive'; destination.mkdir(mode=0o700)
    plan = cmd('inbox.archive', {'op': op('freeze'), 'agents': [rid], 'destination': str(destination)})['archive']
    cmd('inbox.archive', {'op': op('export'), 'action': 'export', 'archive': plan})
    freed = cmd('inbox.archive', {'op': op('free'), 'action': 'free', 'archive': plan})
    assert freed['state'] == 'completed' and len(freed['freed']) == 1, freed
    assert cmd('inbox.get', {'delivery': sent['delivery']})['state'] == 'archived'
    Path(freed['locator']).unlink()  # this test created the private artifact
    assert cmd('inbox.get', {'delivery': sent['delivery']})['state'] == 'unknown'
    return {'verified_free': True, 'missing_artifact_unknown': True}


def case_ordering_visibility():
    calls = []
    def handle(method, params):
        assert params['op'] == 'agent.send', params
        calls.append(params['args']['delivery'])
        return {'outcome': 'indeterminate' if len(calls) == 1 else 'accepted'}
    facts = native()
    facts['caps']['agent.send'] = {'modes': {'start': {'busy': 'reject'}},
        'origin': [{'as': 'attributed_user', 'role': 'user', 'frame': 'sender_header'}]}
    rid = agent('ordering-visibility', facts, handler=handle)
    def send(label):
        o = op(label)
        return cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'start',
            'send_new': True, 'content': [{'type': 'text', 'text': label}]})['delivery']
    scope('pause'); predecessor = send('unknown predecessor'); scope('resume')
    wait(lambda: cmd('inbox.get', {'delivery': predecessor})['state'] == 'unknown')
    successor = send('blocked successor')
    wait(lambda: cmd('inbox.get', {'delivery': successor}).get('reason') == 'ordering_unknown')
    for _ in range(3):
        got = cmd('inbox.get', {'delivery': successor})
        listed = next(row for row in cmd('inbox.list', {'target': rid, 'state': 'held'})['items'] if row['delivery'] == successor)
        for row in (got, listed):
            assert row['ordering_predecessor'] == predecessor, row
        attention = cmd('status')['attention']
        assert any(row.get('delivery') == successor and row.get('ordering_predecessor') == predecessor
                   for row in attention['items']), attention
    assert calls == [predecessor] and cmd('inbox.get', {'delivery': predecessor})['state'] == 'unknown'
    other = Client({'kind': 'principal', 'principal': 'p:i22-other'})
    try:
        cmd('inbox.resolve', {'op': op('unauthorized-resolution'), 'delivery': predecessor, 'as': 'not_delivered'}, other)
        raise AssertionError('ungranted principal resolved predecessor')
    except RPCError as error:
        assert error.kind == 'no_grant', str(error)
    scope('pause')
    cmd('inbox.resolve', {'op': op('authorized-resolution'), 'delivery': predecessor, 'as': 'not_delivered'})
    wait(lambda: cmd('inbox.get', {'delivery': successor}).get('reason') == 'paused')
    assert 'ordering_predecessor' not in cmd('inbox.get', {'delivery': successor})
    scope('resume')
    wait(lambda: cmd('inbox.get', {'delivery': successor})['state'] == 'accepted')
    assert calls == [predecessor, successor], calls
    return {'get_list_attention_name_predecessor': True, 'reads_do_not_resolve_or_replay': True,
            'authorized_resolution_rechecks_pause': True, 'native_writes': len(calls)}


def case_recipe_history(principal=None):
    calls = []
    error_kind = 'busy' + 'k' * 124 if principal is not None else 'busy'
    def handle(method, params):
        operation = params['op']; calls.append(operation)
        if operation == 'agent.send':
            return {'outcome': 'rejected', 'error': {'kind': error_kind, 'message': 'competing start'},
                    'data': {'opaque': 'x' * 20000}}
        return {'outcome': 'observed', 'data': {'opaque': 'y' * 20000}}
    facts = native('running')
    facts['caps'].update({'agent.send': {'modes': {'start': {'busy': 'reject'}},
        'origin': [{'as': 'attributed_user', 'role': 'user', 'frame': 'sender_header'}]},
        'agent.queue': {'actions': ['clear']},
        'agent.interrupt': {'pending': 'unknown', 'scope': 'foreground', 'waits': 'idle'}})
    rid = agent('recipe-history-' + str(len(PROVIDERS)), facts, handler=handle)
    sender = owner
    if principal is not None:
        sender = Client({'kind': 'principal', 'principal': principal})
        raw_set('doc/agents/grants/history-' + rid, {'who': principal, 'on': 'agent:' + rid,
                'can': ['send:interrupt_then_start', 'queue', 'interrupt', 'send:start']})
    scope('pause')
    o = op('i11-recipe')
    args = {'op': o, 'submission': o.ljust(256, 's') if principal is not None else o,
            'to': rid, 'mode': 'preempt', 'strategy': 'interrupt_then_start',
            'content': [{'type': 'text', 'text': 'partial recipe content'}]}
    delivery = cmd('inbox.send', args, sender)['delivery']
    scope('resume')
    def settled():
        row = cmd('inbox.get', {'delivery': delivery})
        return row if row.get('state') == 'partial' and row.get('incomplete') else None
    record = wait(settled)
    expected = [('agent.queue', 'observed'), ('agent.interrupt', 'observed'), ('agent.send', 'rejected')]
    assert [(s['op'], s['outcome']) for s in record['attempt']['steps']] == expected, record
    if principal is None:
        assert record['result']['native']['error_kind'] == error_kind, record
    else:
        assert record['result']['incomplete'], record
    assert calls == [op for op, _ in expected], calls
    repeated = cmd('inbox.send', dict(args, op=op('i11-repeat')), sender)
    assert repeated['delivery'] == delivery and len(calls) == 3, (repeated, calls)
    marked = 'principal_hash' in record['from']
    if principal is not None:
        listed = cmd('inbox.list', {'sender': principal})['items']
        assert [r['delivery'] for r in listed] == [delivery], listed
        if marked:
            assert cmd('inbox.list', {'sender': record['from']['principal_hash']})['items'] == []
        assert cmd('status')['maintenance'] is None
    return {'all_step_outcomes': expected, 'compacted': True, 'native_writes': len(calls),
            'duplicate_recreated': False, 'principal_hash': marked}


def case_declarations():
    status = cmd('status')
    snapshot = owner.call('workspace.snapshot')
    stats, cache = snapshot['identity_reservations'], status['identity_cache']
    assert cache['capacity'] == stats['max_entries'] and cache['max_entry_bytes'] == stats['max_entry_bytes'], (cache, stats)
    assert cache['source_epoch'] == snapshot['epoch'] and not cache['incomplete'], cache
    assert cache['source'] == 'workspace.snapshot.identity_reservations' and 'count' not in cache, cache
    assert status['control_latency_ms'] == 3000, status
    conditions = status['control_latency_conditions']
    assert conditions['historical'] and not conditions['runtime_guarantee'] and not conditions['current_release_verified'], conditions
    assert conditions['measurement']['samples'] == {'interrupt': 3, 'respond': 3}, conditions
    return {'capacity': cache['capacity'], 'max_entry_bytes': cache['max_entry_bytes'],
            'control_latency_ms': 3000, 'historical_conditional_only': True, 'native_writes': 0}


def case_compaction_identities():
    result = {}
    for label, character in (('quotes', '"'), ('backslashes', '\\'), ('plain', 'x')):
        principal = 'p:' + character * 254
        assert len(principal.encode()) == 256
        result[label] = case_recipe_history(principal)
        if label != 'plain':
            assert result[label]['principal_hash'], result
    return {'identities': result, 'native_writes': 9, 'maintenance_error': None}


def case_pause_reporting():
    calls, entered, released = [], threading.Event(), threading.Event()
    def handle(method, params):
        assert params['op'] == 'agent.send', params
        calls.append(params['args']['delivery'])
        entered.set()
        if not released.wait(8):
            raise AssertionError('owned H4 native reply barrier was not released')
        return {'outcome': 'accepted'}
    facts = native()
    facts['caps']['agent.send'] = {'modes': {'start': {'busy': 'reject'}},
        'origin': [{'as': 'attributed_user', 'role': 'user', 'frame': 'sender_header'}]}
    rid = agent('h4-reporting', facts, handler=handle)
    scope('pause')
    o = op('h4-send')
    delivery = cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'start',
        'content': [{'type': 'text', 'text': 'H4 private insertion'}]})['delivery']
    assert not cmd('inbox.get', {'delivery': delivery}).get('attempt') and not calls
    scope('resume')
    assert entered.wait(4), 'native call did not start'
    try:
        claimed = cmd('inbox.get', {'delivery': delivery})
        assert claimed['state'] == 'claimed', claimed
        attempt = claimed['attempt']['id']
        paused = scope('pause')
        assert next(item for item in paused['outstanding'] if item['agent'] == rid)['attempts'] == [attempt], paused
        assert paused['outstanding_count'] >= 1 and not paused['outstanding_incomplete'], paused
        assert 'H4 private insertion' not in json.dumps(paused)
        history = cmd('scope', {'action': 'result', 'receipt': paused['receipt']})
        assert history['outstanding_count'] == paused['outstanding_count'] and history['outstanding_incomplete'], history
        assert history['outstanding_hash'] == paused['outstanding_hash'] and attempt not in json.dumps(history)
    finally:
        released.set()
    wait(lambda: cmd('inbox.get', {'delivery': delivery})['state'] == 'accepted')
    later_op = op('h4-later')
    later = cmd('inbox.send', {'op': later_op, 'submission': later_op, 'to': rid, 'mode': 'start',
        'content': [{'type': 'text', 'text': 'stays behind closed gate'}]})['delivery']
    wait(lambda: cmd('inbox.get', {'delivery': later})['reason'] == 'paused')
    assert not cmd('inbox.get', {'delivery': later}).get('attempt') and calls == [delivery], calls
    return {'pause_wins_no_claim': True, 'winning_attempt_reported': True,
            'historical_details_incomplete': True, 'native_writes': len(calls)}


def case_queue_epoch():
    calls, runtime = [], {'running': True}
    def handle(method, params):
        operation, args = params['op'], params['args']
        calls.append({'operation': operation, 'epoch': args.get('epoch')})
        if operation == 'agent.queue':
            if args.get('epoch') != 'e1':
                return {'outcome': 'rejected', 'error': {'kind': 'stale', 'message': 'queue epoch mismatch'}}
        elif operation == 'agent.interrupt':
            runtime['running'] = False
        elif operation == 'agent.send':
            assert not runtime['running'], 'start before native interrupt observation'
        return {'outcome': 'accepted' if operation == 'agent.send' else 'observed'}
    facts = native('running')
    facts['caps'].update({'agent.send': {'modes': {'start': {'busy': 'reject'}},
        'origin': [{'as': 'attributed_user', 'role': 'user', 'frame': 'sender_header'}]},
        'agent.queue': {'actions': ['clear', 'list']},
        'agent.interrupt': {'pending': 'unknown', 'scope': 'foreground', 'waits': 'idle'}})
    rid = agent('queue-epoch', facts, handler=handle)
    scope('pause')
    o = op('recipe')
    delivery = cmd('inbox.send', {'op': o, 'submission': o, 'to': rid, 'mode': 'preempt',
        'strategy': 'interrupt_then_start', 'content': [{'type': 'text', 'text': 'recipe input'}]})['delivery']
    scope('resume')
    wait(lambda: cmd('inbox.get', {'delivery': delivery})['state'] == 'accepted')
    assert [row['operation'] for row in calls] == ['agent.queue', 'agent.interrupt', 'agent.send'], calls
    assert calls[0]['epoch'] == 'e1', calls
    default = cmd('control', {'op': op('queue-list'), 'target': rid, 'action': 'queue', 'args': {'action': 'list'}})
    assert default['state'] == 'observed' and calls[-1]['epoch'] == 'e1', (default, calls)
    stale = cmd('control', {'op': op('queue-stale'), 'target': rid, 'action': 'queue', 'args': {'action': 'list', 'epoch': 'old'}})
    assert stale['state'] == 'rejected' and calls[-1]['epoch'] == 'old', (stale, calls)
    return {'native_recipe_steps': 3, 'queue_epoch': 'e1', 'explicit_stale_preserved': True}


def case_handover_withdrawn():
    results = []
    for action, withdrawn_count in [('complete', 1), ('abort', 1), ('abort', 2), ('complete', 2)]:
        tag = action + str(withdrawn_count)
        src = agent('withdraw-src-' + tag, native())
        dst = agent('withdraw-dst-' + tag, dict(native(), session={'id': 's2'}))
        scope('pause')
        deliveries = []
        for n in range(2):
            o = op('withdraw-source')
            deliveries.append(cmd('inbox.send', {'op': o, 'submission': o, 'to': src, 'mode': 'start', 'send_new': True,
                                'content': [{'type': 'text', 'text': str(n)}]})['delivery'])
        peer, generation, facts = GEN['withdraw-src-' + tag]
        facts['data']['agent']['lifecycle'] = 'ended'
        peer.call('provider.sync', {'generation': generation, 'resources': [facts], 'final': True})
        wait(lambda: cmd('inbox.get', {'delivery': deliveries[0]})['reason'] == 'agent_ended')
        plan = cmd('handover', {'op': op('withdraw-plan'), 'from': {'agent': src, 'incarnation': 'i1'},
                   'to': {'agent': dst, 'incarnation': 'i1'}, 'deliveries': deliveries})['plan']
        for delivery in deliveries[:withdrawn_count]:
            record = cmd('inbox.get', {'delivery': delivery})
            cmd('inbox.withdraw', {'op': op('withdraw-mid-plan'), 'delivery': delivery, 'expect': record['revision']})
        result = cmd('handover', {'op': op('finalize-withdrawn'), 'action': action, 'plan': plan})
        assert result['state'] == ('completed' if action == 'complete' else 'aborted'), result
        for n, delivery in enumerate(deliveries):
            record = cmd('inbox.get', {'delivery': delivery})
            assert not record.get('handover_plan'), record
            if n < withdrawn_count:
                assert record['state'] == 'withdrawn', record
            else:
                assert record['state'] == 'held' and record['to']['agent'] == (dst if action == 'complete' else src), record
        results.append({'action': action, 'withdrawn': withdrawn_count, 'final': result['state']})
    return {'variants': results, 'nonpending_never_replayed': True}


def case_handover_archive():
    results = []
    for action in ('complete', 'abort'):
        ref = 'archive-handover-' + action
        src = agent(ref, native()); dst = agent(ref + '-dst', native())
        scope('pause')
        deliveries = []
        for n in range(2):
            o = op(ref)
            deliveries.append(cmd('inbox.send', {'op': o, 'submission': o, 'to': src, 'mode': 'start',
                'send_new': True, 'content': [{'type': 'text', 'text': str(n)}]})['delivery'])
        peer, generation, facts = GEN[ref]
        facts['data']['agent']['lifecycle'] = 'ended'
        peer.call('provider.sync', {'generation': generation, 'resources': [facts], 'final': True})
        wait(lambda: cmd('inbox.get', {'delivery': deliveries[0]})['reason'] == 'agent_ended')
        plan = cmd('handover', {'op': op(ref + '-plan'), 'from': {'agent': src, 'incarnation': 'i1'},
            'to': {'agent': dst, 'incarnation': 'i1'}, 'deliveries': deliveries})['plan']
        destination = ROOT / ref; destination.mkdir()
        try:
            cmd('inbox.archive', {'op': op('open-plan-freeze'), 'agents': [dst], 'destination': str(destination)})
            raise AssertionError('archive accepted open handover members')
        except RPCError as error:
            assert error.kind == 'conflict' and 'handover' in str(error), str(error)
        result = cmd('handover', {'op': op('finish-plan'), 'action': action, 'plan': plan})
        assert result['state'] == ('completed' if action == 'complete' else 'aborted'), result
        target = dst if action == 'complete' else src
        archive = cmd('inbox.archive', {'op': op('finished-plan-freeze'), 'agents': [target],
            'destination': str(destination)})['archive']
        assert cmd('inbox.archive', {'op': op('finished-plan-export'), 'action': 'export', 'archive': archive})['state'] == 'accepted'
        freed = cmd('inbox.archive', {'op': op('finished-plan-free'), 'action': 'free', 'archive': archive})
        assert freed['state'] == 'completed' and len(freed['freed']) == 2, freed
        results.append({'action': action, 'open_freeze_refused': True, 'freed_after_finish': 2})
    return {'variants': results}


def case_archive_abort():
    src = agent('archive-abort-source', native())
    dst = agent('archive-abort-destination', native())
    scope('pause')
    deliveries = []
    for n in range(6):
        o = op('archive-abort-send')
        deliveries.append(cmd('inbox.send', {'op': o, 'submission': o, 'to': src, 'mode': 'start',
                          'send_new': True, 'content': [{'type': 'text', 'text': str(n)}]})['delivery'])
    peer, generation, facts = GEN['archive-abort-source']
    facts['data']['agent']['lifecycle'] = 'ended'
    peer.call('provider.sync', {'generation': generation, 'resources': [facts], 'final': True})
    wait(lambda: cmd('inbox.get', {'delivery': deliveries[0]})['reason'] == 'agent_ended')
    destination = ROOT / 'archive-abort'; destination.mkdir()
    archive = cmd('inbox.archive', {'op': op('freeze-abort'), 'agents': [src], 'destination': str(destination)})['archive']
    for _ in range(3):
        cmd('inbox.archive', {'op': op('empty-plan'), 'agents': [src], 'deliveries': [], 'destination': str(destination)})
    frozen = cmd('inbox.get', {'delivery': deliveries[0]})
    try:
        cmd('handover', {'op': op('frozen-handover'), 'from': {'agent': src, 'incarnation': 'i1'},
                        'to': {'agent': dst, 'incarnation': 'i1'}, 'deliveries': [deliveries[0]]})
        raise AssertionError('handover changed frozen evidence')
    except RPCError as error:
        assert error.kind == 'conflict' and 'frozen' in str(error), str(error)
    assert cmd('inbox.get', {'delivery': deliveries[0]}) == frozen
    raw_set('doc/agents/grants/archive-abort-recover', {'who': 'p:archive-recover', 'on': 'agent:' + src, 'can': ['recover']})
    recoverer = Client({'kind': 'principal', 'principal': 'p:archive-recover'})
    try:
        cmd('inbox.archive', {'op': op('nonowner-abort'), 'action': 'abort', 'archive': archive}, recoverer)
        raise AssertionError('recover right conferred archive-abort ownership')
    except RPCError as error:
        assert error.kind == 'no_grant', str(error)
    aborted = cmd('inbox.archive', {'op': op('owner-abort'), 'action': 'abort', 'archive': archive})
    assert aborted['state'] == 'aborted' and aborted['released'] == 6, aborted
    audit = cmd('inbox.archive', {'action': 'inspect', 'archive': archive})
    assert audit['abort']['principal'] == frozen['from']['principal'], audit
    replanned = cmd('inbox.archive', {'op': op('replan'), 'agents': [src], 'destination': str(destination)})['archive']
    assert cmd('inbox.archive', {'op': op('re-export'), 'action': 'export', 'archive': replanned})['state'] == 'accepted'
    freed = cmd('inbox.archive', {'op': op('re-free'), 'action': 'free', 'archive': replanned})
    assert freed['state'] == 'completed' and len(freed['freed']) == 6 and freed['changed'] == [], freed
    return {'owner_only': True, 'handover_frozen_refused': True, 'released_slot_reused': True, 'replanned_freed': 6}


# Witness the bounded attention view before bulk uncertain-history fixtures fill it.
CASES = {'ordering_visibility': case_ordering_visibility, 'brick': case_brick, 'idle': case_idle, 'membership': case_membership, 'identity': case_identity, 'reconcile': case_reconcile, 'names': case_names,
         'rawcycle': case_rawcycle, 'invalid_declaration': case_invalid_declaration,
         'window': case_window, 'handover': case_handover, 'archive': case_archive, 'queue_epoch': case_queue_epoch, 'handover_withdrawn': case_handover_withdrawn, 'archive_abort': case_archive_abort, 'handover_archive': case_handover_archive, 'pause_reporting': case_pause_reporting, 'recipe_history': case_recipe_history, 'compaction_identities': case_compaction_identities, 'declarations': case_declarations}
core = None
try:
    core = subprocess.Popen([str(BIN), 'serve', '--fake'], env=ENV, cwd=ROOT, stdout=open(ROOT / 'core.log', 'wb'), stderr=subprocess.STDOUT)
    def connect():
        global owner
        owner = Client({'kind': 'owner'}); return owner
    wait(connect)
    snap = owner.call('workspace.snapshot')
    ops = [{'op': 'set', 'key': k, 'expect': r['version'], 'value': dict(r['value'], enabled=False)}
           for k, r in snap['entries'].items() if k.startswith('extension/') and isinstance(r['value'], dict) and r['value'].get('enabled')]
    if ops: owner.call('workspace.apply', {'ops': ops})
    decl = {'enabled': True, 'argv': ['/usr/bin/python3', str(EXT / 'coordination.py')], 'cwd': str(EXT),
            'manifest': json.loads((EXT / 'manifests/coord.json').read_text())}
    owner.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'extension/coord', 'expect': 0, 'value': decl}]})
    wait(lambda: cmd('status')['state'] == 'ready', 30)
    scope('resume')
    for name in (ARGS.cases or list(CASES)):
        try:
            result = CASES[name]()
            if name == 'idle':
                assert not result['violation'], result
            elif name == 'membership':
                assert not result['cycle_formed'], result
            elif name == 'identity':
                assert all(row.get('admitted') and row.get('workspace_resume') == 'ok' and row.get('send') == 'accepted' for row in result.values()), result
            elif name == 'reconcile':
                assert len(result['rows']) == 4 and all(row.get('state') == 'reconciled' for row in result['rows']), result
            elif name == 'names':
                assert result['send_before_names'] == result['send_after_names'] == 'err:no_grant' and not result['escalated'], result
            elif name == 'brick':
                assert result['pause'] == result['resume'] == result['resume_only_good_agent'] == 'ok', result
            OUT['cases'][name] = dict(result, passed=True)
        except Exception as e:
            OUT['cases'][name] = {'harness_error': repr(e)[:300]}
finally:
    try:
        if owner: owner.call('core.shutdown')
    except Exception:
        pass
    if core:
        try: core.wait(10)
        except subprocess.TimeoutExpired: core.kill(); core.wait()
    (ROOT / 'report.json').write_text(json.dumps(OUT, indent=2) + '\n')
    OUT['result'] = 'pass' if OUT['cases'] and all(row.get('passed') for row in OUT['cases'].values()) else 'fail'
    (ROOT / 'report.json').write_text(json.dumps(OUT, indent=2) + '\n')
    print(json.dumps(OUT, indent=2))
raise SystemExit(0 if OUT['result'] == 'pass' else 1)
