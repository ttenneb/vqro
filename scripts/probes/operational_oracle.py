#!/usr/bin/env python3
"""C1 regression oracle ported from P1: product owned-domain RPC, fake native effects only."""
import argparse, asyncio, collections, concurrent.futures, copy, hashlib, json, os, signal, socket
import subprocess, sys, tempfile, time
from pathlib import Path
INDEX = 'operational/inbox/index'
SELF = str(Path(__file__).resolve())
class Refusal(Exception):
    def __init__(self, error): self.error=error; super().__init__(str(error))
    @property
    def kind(self): return self.error.get('data',{}).get('kind')
class Peer:
    def __init__(self, path):
        self.s=socket.socket(socket.AF_UNIX); self.s.settimeout(60); self.s.connect(str(path)); self.f=self.s.makefile('rb'); self.n=0; self.events=collections.deque(maxlen=512)
        self.epoch=self.call('hello',{'client':'C1-private'})['epoch']
        token=Path(path).parent/'owner.token'
        if token.exists():self.call('hello',{'extension':'owner','extension_token':token.read_text()})
    def call(self, method, params=None):
        params=params or {};unwrap=False
        if method=='workspace.get' and params.get('key','').startswith('operational/inbox/'):
            method='operational.get';params={'domain':'inbox','key':params['key']};unwrap=True
        elif method=='workspace.apply' and params.get('history') is False:
            method='operational.apply';params={'domain':'inbox','control':True,'ops':params['ops'],'note':params.get('note')}
        self.n+=1; mid=self.n; self.s.sendall((json.dumps({'jsonrpc':'2.0','id':mid,'method':method,'params':params or {}},separators=(',',':'))+'\n').encode())
        while True:
            line=self.f.readline()
            if not line: raise EOFError('core transport closed; outcome unknown, never replay')
            reply=json.loads(line)
            if reply.get('id')!=mid: self.events.append(reply); continue
            if 'error' in reply: raise Refusal(reply['error'])
            return reply['result']['entry'] if unwrap else reply['result']
    def close(self):
        try: self.s.shutdown(socket.SHUT_RDWR)
        except OSError: pass
        self.f.close(); self.s.close()
class Fixture:
    def __init__(self, root, binary, fault='none',domain_bytes=2097152):
        self.domain_bytes=domain_bytes
        self.root=Path(root); self.root.mkdir(parents=True,exist_ok=True); self.binary=str(binary)
        for name in ['state','home','run','ext']: (self.root/name).mkdir(exist_ok=True)
        self.sockroot=tempfile.TemporaryDirectory(prefix='vi',dir='/tmp'); self.socket=Path(self.sockroot.name)/'p'; self.proc=None; self.start(fault)
    def start(self, fault='none'):
        for name in ['ready','fault-boundary','release-fault','owner.token']: (self.root/'state'/name).unlink(missing_ok=True)
        env={'HOME':str(self.root/'home'),'PATH':'/usr/bin:/bin','VQRO_STATE_DIR':str(self.root/'state'),'VQRO_SOCKET':str(self.socket),'VQRO_EXTENSIONS_DIR':str(self.root/'ext'),'VQRO_HERDR':'["/bin/false"]','XDG_RUNTIME_DIR':str(self.root/'run'),'HC_SOCKET':str(self.root/'h'),'HC_SESSION':'p1-private'}
        with open(self.root/f'core-{time.time_ns()}.log','wb') as log:
            self.proc=subprocess.Popen([self.binary,str(self.root/'state'),str(self.socket),fault],env=env,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
        self.wait_file(self.root/'state/ready');self.wait_file(self.root/'state/owner.token')
        until=time.monotonic()+5
        while len((self.root/'state/owner.token').read_text())!=64:
            if time.monotonic()>until:raise TimeoutError('private owner token')
            time.sleep(.005)
        (Path(self.sockroot.name)/'owner.token').write_text((self.root/'state/owner.token').read_text())
        self.peer=Peer(self.socket);self.peer.call('operational.register',{'name':'inbox','bytes':self.domain_bytes})
    def wait_file(self, path):
        until=time.monotonic()+10
        while not path.exists():
            if self.proc.poll() is not None: raise AssertionError(f'private core exited: {self.root}')
            if time.monotonic()>until: raise TimeoutError(str(path))
            time.sleep(.005)
    def stop(self):
        if self.proc:
            try: self.peer.close()
            except (OSError,AttributeError): pass
            if self.proc.poll() is None: self.proc.kill()
            self.proc.wait(timeout=10); self.proc=None
    def close(self): self.stop(); self.sockroot.cleanup()
    def __enter__(self): return self
    def __exit__(self,*args): self.close()
def op(key, value, version): return {'op':'set','key':key,'value':value,'expect':version}
def key(mid): return f'operational/inbox/message/{mid}'
def digest(content): return hashlib.sha256(content.encode()).hexdigest()
class Inbox:
    def __init__(self, peer, ledger, retention=8): self.p=peer; self.ledger=Path(ledger); self.retention=retention
    def get(self,k): return self.p.call('workspace.get',{'key':k})
    def apply(self, ops,note=None): return self.p.call('workspace.apply',{'history':False,'ops':ops,'note':note})
    def put(self, content, mid=None):
        assert len(content.encode())<=32768
        entry=self.get(INDEX); index=copy.deepcopy(entry['value']) if entry else {'high':0,'live':[],'settled':[],'resumed_epoch':self.p.epoch}
        mid=index['high']+1 if mid is None else mid
        if mid<=index['high']:
            old=self.get(key(mid))
            if old is None: return mid,{'status':'unknown_pruned'}
            if old['value']['origin']!=digest(content): raise ValueError('submission conflict')
            return mid,old
        if mid!=index['high']+1: raise ValueError('invalid sequence; resolve once, do not recreate old ids')
        if len(index['live'])>=128: raise ValueError('prototype per-target admission full')
        index['high']=mid; index['live'].append(mid)
        record={'id':mid,'target':'agent:fixture','origin':digest(content),'content':content,'revision':1,'phase':'pending','epoch':self.p.epoch}
        self.apply([op(INDEX,index,entry['version'] if entry else 0),op(key(mid),record,0)])
        return mid,self.get(key(mid))
    def status(self,mid):
        row=self.get(key(mid)); index=self.get(INDEX)
        if row is None: return 'unknown_pruned' if index and mid<=index['value']['high'] else 'absent'
        value=row['value']
        if value['phase']=='pending' and index['value']['resumed_epoch']!=self.p.epoch: return 'held'
        if value['phase']=='claimed' and value['claim']['epoch']!=self.p.epoch: return 'unknown'
        return value['phase']
    def edit(self,mid,content=None,withdraw=False,cached=None):
        row=cached or self.get(key(mid)); value=copy.deepcopy(row['value'])
        if value['phase']!='pending': raise ValueError('too late')
        value['revision']+=1
        if withdraw: value['phase']='withdrawn'
        else: value['content']=content
        return self.apply([op(key(mid),value,row['version'])])
    def claim(self,mid,cached=None):
        row=cached or self.get(key(mid)); value=copy.deepcopy(row['value']); index=self.get(INDEX)['value']
        if index['resumed_epoch']!=self.p.epoch or value['phase']!='pending': raise ValueError('held or too late')
        value['phase']='claimed'; value['claim']={'attempt':f'{mid}:{value["revision"]}','epoch':self.p.epoch,'revision':value['revision']}
        # Only a confirmed-durable success authorizes one invoke. No error path calls native_write.
        self.apply([op(key(mid),value,row['version'])],note='claim'); return self.get(key(mid))
    def native(self,row):
        subprocess.run([sys.executable,SELF,'native',str(self.ledger),str(row['value']['id']),row['value']['content']],check=True,env={'PATH':'/usr/bin:/bin'})
    def result(self,mid):
        row=self.get(key(mid)); value=copy.deepcopy(row['value']); assert value['phase']=='claimed'
        value['phase']='settled'; value['result']={'native':'accepted'}
        entry=self.get(INDEX); index=copy.deepcopy(entry['value']); index['settled'].append(mid)
        ops=[op(key(mid),value,row['version'])]
        while len(index['settled'])>self.retention:
            old=index['settled'].pop(0); index['live'].remove(old); old_entry=self.get(key(old))
            ops.append({'op':'delete','key':key(old),'expect':old_entry['version']})
        ops.append(op(INDEX,index,entry['version'])); self.apply(ops)
    def dispatch(self,mid): row=self.claim(mid); self.native(row); self.result(mid)
def ledger_rows(path): return [json.loads(line) for line in Path(path).read_text().splitlines()] if Path(path).exists() else []
def native_write(path, mid, content):
    # This oracle records EVERY native invocation, not idempotent suppression that could hide replay.
    with open(path,'a') as out:
        out.write(json.dumps({'id':int(mid),'content':content})+'\n'); out.flush(); os.fsync(out.fileno())
def worker(socket_path, ledger, mid, stop_at, marker):
    p=Peer(socket_path); box=Inbox(p,ledger)
    try:
        row=box.claim(mid)
        if stop_at=='claim': Path(marker).write_text('confirmed-claim-before-invoke')
        else:
            box.native(row); Path(marker).write_text('native-applied-before-result')
        while True: time.sleep(1)
    finally: p.close()
def run_checks(binary,out):
    out=Path(out); out.mkdir(parents=True,exist_ok=True); evidence=[]
    for fault,expected in [('claim-before','held'),('claim-after','unknown')]:
        with Fixture(out/fault,binary,fault) as f:
            box=Inbox(f.peer,f.root/'native.jsonl'); mid,_=box.put('payload')
            with concurrent.futures.ThreadPoolExecutor(1) as pool:
                def claiming():
                    p=Peer(f.socket)
                    try: return Inbox(p,box.ledger).claim(mid)
                    finally: p.close()
                future=pool.submit(claiming); f.wait_file(f.root/'state/fault-boundary'); f.stop()
                try: future.result(timeout=5); raise AssertionError('dead core acknowledged a claim')
                except (EOFError,ConnectionError,OSError): pass
            f.start(); box=Inbox(f.peer,box.ledger); assert box.status(mid)==expected; assert not ledger_rows(box.ledger)
            evidence.append({'boundary':fault,'recovered':expected,'native_writes':0})
    for boundary in ['claim','invoke']:
        with Fixture(out/f'worker-{boundary}',binary) as f:
            box=Inbox(f.peer,f.root/'native.jsonl'); mid,_=box.put('payload'); marker=f.root/'worker-boundary'
            proc=subprocess.Popen([sys.executable,SELF,'worker',str(f.socket),str(box.ledger),str(mid),boundary,str(marker)],env={'PATH':'/usr/bin:/bin'})
            try: f.wait_file(marker)
            finally:
                if proc.poll() is None: proc.kill()
                proc.wait(timeout=5)
            f.stop(); f.start(); box=Inbox(f.peer,box.ledger); assert box.status(mid)=='unknown'
            assert len(ledger_rows(box.ledger))==(boundary=='invoke')
            try: box.dispatch(mid); raise AssertionError('unknown mail replayed')
            except ValueError: pass
            assert len(ledger_rows(box.ledger))==(boundary=='invoke'); evidence.append({'boundary':boundary,'recovered':'unknown','native_writes':len(ledger_rows(box.ledger))})
    for fault in ['claim-unconfirmed','claim-error']:
        with Fixture(out/fault,binary,fault) as f:
            box=Inbox(f.peer,f.root/'native.jsonl'); mid,_=box.put('do-not-invoke')
            f.peer.call('workspace.apply',{'ops':[{'op':'set','key':'doc/layout','value':1}]})
            f.peer.call('events.subscribe',{})
            try: box.dispatch(mid); raise AssertionError('fault was ignored')
            except Refusal as error:
                assert error.kind==('durability_unconfirmed' if fault=='claim-unconfirmed' else 'io')
                assert not ledger_rows(box.ledger)
                if fault=='claim-unconfirmed':
                    until=time.monotonic()+2
                    while True:
                        f.peer.call('hello',{'client':'event-barrier'})
                        events=[e['params'] for e in f.peer.events if e.get('method')=='event' and e.get('params',{}).get('type')=='operational']
                        if events or time.monotonic()>until: break
                        time.sleep(.01)
                    assert events and events[-1]['data']['txn']==error.error['data']['txn'],f.peer.events
                    assert events[-1]['data']['changes'][0]['key']==key(mid)
            evidence.append({'fault':fault,'native_writes':0})
    with Fixture(out/'retention',binary) as f:
        box=Inbox(f.peer,f.root/'native.jsonl',retention=8)
        initial=f.peer.call('workspace.apply',{'ops':[{'op':'set','key':'doc/layout','value':1}]})['txn']
        history=f.peer.call('workspace.history',{'limit':500}); history_bytes=json.dumps(history,sort_keys=True)
        for n in range(40): mid,_=box.put(f'payload-{n}'); box.dispatch(mid)
        assert json.dumps(f.peer.call('workspace.history',{'limit':500}),sort_keys=True)==history_bytes
        assert len(box.get(INDEX)['value']['live'])==8
        f.peer.call('workspace.revert',{'txn':initial}); f.stop(); f.start(); box=Inbox(f.peer,box.ledger)
        assert box.put('payload-0',1)[1]['status']=='unknown_pruned'; assert len(ledger_rows(box.ledger))==40
        mid,_=box.put('held-after-restart'); f.stop(); f.start(); box=Inbox(f.peer,box.ledger); assert box.status(mid)=='held'
        evidence.append({'settled':40,'retention':8,'after_restart':'old ids unknown, pending held','history_unchanged':True})
    with Fixture(out/'races',binary) as f:
        box=Inbox(f.peer,f.root/'native.jsonl'); cases=[]
        for withdraw in [False,True]:
            for winner in ['edit','claim','concurrent']:
                for repeat in range(10):
                    mid,row=box.put('old'); other=Peer(f.socket); contender=Inbox(other,box.ledger)
                    def change(): return contender.edit(mid,'new',withdraw=withdraw,cached=row)
                    def claim(): return box.claim(mid,cached=row)
                    try:
                        if winner=='edit':
                            change()
                            try: claim(); raise AssertionError('stale claim succeeded')
                            except (Refusal,ValueError): pass
                            if not withdraw: box.dispatch(mid)
                        elif winner=='claim':
                            claimed=claim()
                            try: change(); raise AssertionError('post-claim edit succeeded')
                            except (Refusal,ValueError): pass
                            box.native(claimed); box.result(mid)
                        else:
                            with concurrent.futures.ThreadPoolExecutor(2) as pool:
                                a=pool.submit(change); b=pool.submit(claim); results=[]
                                for future in [a,b]:
                                    try: results.append(('ok',future.result()))
                                    except (Refusal,ValueError): results.append(('conflict',None))
                            assert [r[0] for r in results].count('ok')==1,results
                            if results[1][0]=='ok': box.native(results[1][1]); box.result(mid)
                            elif not withdraw: box.dispatch(mid)
                        matching=[r for r in ledger_rows(box.ledger) if r['id']==mid]; assert len(matching)<=1
                        if matching:
                            edited=box.get(key(mid))
                            assert matching[0]['content']==edited['value']['content']
                        cases.append({'withdraw':withdraw,'order':winner,'writes':len(matching)})
                    finally: other.close()
        rows=ledger_rows(box.ledger); assert len({r['id'] for r in rows})==len(rows)
        evidence.append({'race_cases':len(cases),'no_duplicate_native_write':True,'cases':cases})
    (out/'oracle.json').write_text(json.dumps(evidence,indent=2)+'\n'); print(json.dumps(evidence,indent=2))

def main():
    mode=sys.argv[1]
    if mode=='native': native_write(*sys.argv[2:]); return
    if mode=='worker': worker(sys.argv[2],sys.argv[3],int(sys.argv[4]),sys.argv[5],sys.argv[6]); return
    parser=argparse.ArgumentParser(); parser.add_argument('mode',choices=['check']); parser.add_argument('--core',required=True); parser.add_argument('--out',required=True); args=parser.parse_args()
    run_checks(args.core,args.out)
if __name__=='__main__': main()
