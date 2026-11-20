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
    def __init__(self, identity=None):
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
    preview = cmd('scope', {'action': 'preview', 'scope': 'workspace'})
    return cmd('scope', {'op': op(action), 'action': action, 'scope': 'workspace', 'expect': preview['expect']})


def raw_set(key, value):
    cur = owner.call('workspace.get', {'key': key})
    return owner.call('workspace.apply', {'ops': [{'op': 'set', 'key': key, 'value': value, 'expect': cur['version'] if cur else 0}]})


PROVIDERS = []


def agent(ref, native, name=None, instance='stable', incarnation='i1'):
    name = name or ('k2-' + ref)
    """Register (once per name) a fake backend and publish one agent resource; return its rid."""
    peer = Client()
    reg = peer.call('provider.register', {'name': name, 'role': 'backend', 'instance': instance,
        'software': {'name': 'fake', 'version': '0'}, 'provider_version': '0', 'capabilities': {'agent': {}}})
    facts = {'ref': ref, 'incarnation': incarnation, 'kind': 'agent', 'state': 'live',
             'status': {'state': native.get('state', 'idle'), 'app': 'fake'}, 'data': {'agent': native}}
    peer.call('provider.sync', {'generation': reg['generation'], 'resources': [facts], 'final': True})
    PROVIDERS.append(peer)
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


CASES = {'brick': case_brick, 'idle': case_idle, 'membership': case_membership, 'identity': case_identity, 'reconcile': case_reconcile, 'names': case_names}
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
