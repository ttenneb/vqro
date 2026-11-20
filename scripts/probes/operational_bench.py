#!/usr/bin/env python3
"""C1 single-core-process calibration:8MB layouts + declared8MiB operational domain, real writer."""
import argparse,asyncio,hashlib,json,math,os,subprocess,time
from pathlib import Path
from operational_oracle import Fixture
class AsyncPeer:
 @classmethod
 async def open(cls,path):
  s=cls();s.r,s.w=await asyncio.open_unix_connection(str(path),limit=64*1024*1024);s.n=0;s.pending={};s.reader=asyncio.create_task(s.read());await s.call('hello',{'client':'C1-benchmark'});await s.call('hello',{'extension':'owner','extension_token':(Path(path).parent/'owner.token').read_text()});return s
 async def read(s):
  try:
   while line:=await s.r.readline():
    reply=json.loads(line);f=s.pending.pop(reply.get('id'),None)
    if f is not None and not f.done():f.set_result(reply)
  finally:
   for f in s.pending.values():
    if not f.done():f.set_exception(EOFError('transport closed; no replay'))
 async def call(s,method,params):
  s.n+=1;mid=s.n;f=asyncio.get_running_loop().create_future();s.pending[mid]=f;s.w.write((json.dumps({'jsonrpc':'2.0','id':mid,'method':method,'params':params},separators=(',',':'))+'\n').encode());await s.w.drain()
  try:return await asyncio.wait_for(f,60)
  finally:s.pending.pop(mid,None)
 async def close(s):s.w.close();await s.w.wait_closed();s.reader.cancel()
def pct(values):
 v=sorted(values);return {'n':len(v),'p50_ms':v[math.ceil(.5*len(v))-1] if v else None,'p99_ms':v[math.ceil(.99*len(v))-1] if v else None,'max_ms':max(v) if v else None}
def process(pid):
 stat=Path(f'/proc/{pid}/stat').read_text().split(') ')[1].split();status=Path(f'/proc/{pid}/status').read_text().splitlines()
 return {'cpu_s':(int(stat[11])+int(stat[12]))/os.sysconf('SC_CLK_TCK'),**{line.split(':')[0]:line.split(':')[1].strip() for line in status if line.startswith(('VmRSS:','VmHWM:','Cpus_allowed_list:'))},'loadavg':Path('/proc/loadavg').read_text().strip()}
def seed_workspace(p):
 initial=p.call('workspace.snapshot')['entries'];version=max([e['version'] for e in initial.values()]+[0]);values={}
 for i in range(100):values[f'view/panel-{i:03}']={'kind':'panel','title':f'Agent workspace{i}','items':[{'type':'text','text':f'agent{i}/row{j}: '+('workspace status and research notes. '*18)} for j in range(128)]}
 layout={'tabs':[{'title':f'Agent{i}','node':{'view':f'panel-{i:03}'}} for i in range(100)],'active':0};values['layout/main']=layout;values['workspace/active']={'layout':'main'}
 def encoded():return len(json.dumps({**initial,**{key:{'version':version+i+1,'value':value} for i,(key,value) in enumerate(values.items())}},separators=(',',':')).encode())
 size=encoded()
 if size>8_000_000:
  remove=math.ceil((size-8_000_000)/12800)
  for key,value in values.items():
   if key.startswith('view/'):
    for row in value['items']:row['text']=row['text'][:-remove]
 padding=8_000_000-encoded();assert padding>=0;values['view/panel-000']['items'][0]['text']+='x'*padding
 items=list(values.items())
 for at in range(0,len(items),5):p.call('workspace.apply',{'note':'C1 realistic8MB seed','ops':[{'op':'set','key':key,'value':value} for key,value in items[at:at+5]]})
 return layout
def seed_operational(p,budget,fill):
 values={f'operational/inbox/bench/{i}':{'phase':'pending','content':'x'*384,'revision':0} for i in range(128)}
 ceiling=budget if fill=='full' else budget-math.ceil(budget/10)
 pads=max(1,(ceiling-65536-8192)//32768)
 for i in range(pads):values[f'operational/inbox/retained/{i}']={'phase':'settled','content':'p'*31000,'revision':1}
 target=ceiling-65536-8192
 def encoded():return len(json.dumps({key:{'version':i//4+1,'value':value} for i,(key,value) in enumerate(values.items())},separators=(',',':')).encode())
 padding=target-encoded();assert padding>=0
 for i in range(pads):
  more=min(padding,1768);values[f'operational/inbox/retained/{i}']['content']+='p'*more;padding-=more
 assert padding==0
 items=list(values.items())
 for at in range(0,len(items),4):p.call('operational.apply',{'domain':'inbox','control':True,'ops':[{'op':'set','key':key,'value':value,'expect':0} for key,value in items[at:at+4]]})
 metadata=p.call('operational.snapshot',{'domain':'inbox'});assert metadata['bytes_used']<=budget;return metadata
async def load(f,layout,rate,seconds,traffic):
 p=await AsyncPeer.open(f.socket);start=time.monotonic()+.05;rows=[];tasks=[];before=process(f.proc.pid)
 async def one(i,scheduled):
  kind='layout' if i%10==0 else 'operational';begun=time.monotonic();row={'i':i,'kind':kind,'scheduled_s':scheduled-start,'start_lag_ms':(begun-scheduled)*1000}
  if kind=='layout':method='workspace.apply';params={'note':f'C1 bench layout{i}','ops':[{'op':'set','key':'layout/main','value':{**layout,'active':i%100}}]}
  else:method='operational.apply';params={'domain':'inbox','control':traffic=='control' or (traffic=='mixed' and i%10==1),'note':f'C1 bench record{i}','ops':[{'op':'set','key':f'operational/inbox/bench/{i%128}','value':{'phase':['pending','claimed','settled'][i%3],'content':'x'*384,'revision':i}}]}
  row['control']=params.get('control',False)
  try:
   reply=await p.call(method,params);row['latency_ms']=(time.monotonic()-begun)*1000;row['offered_latency_ms']=(time.monotonic()-scheduled)*1000
   if 'error' in reply:row.update(status='rejected',error=reply['error'])
   else:row.update(status='ok',txn=reply['result']['txn'])
  except Exception as error:row.update(status='unknown',latency_ms=(time.monotonic()-begun)*1000,error=repr(error))
  rows.append(row)
 for i in range(round(rate*seconds)):
  scheduled=start+i/rate;await asyncio.sleep(max(0,scheduled-time.monotonic()));tasks.append(asyncio.create_task(one(i,scheduled)))
 await asyncio.gather(*tasks);after=process(f.proc.pid);elapsed=time.monotonic()-start;await p.close();return sorted(rows,key=lambda r:r['i']),{'elapsed_with_drain_s':elapsed,'cpu_s':after['cpu_s']-before['cpu_s'],'before':before,'after':after}
def run(binary,out,rate,seconds,repeats,budget,traffic,fill):
 out=Path(out);out.mkdir(parents=True,exist_ok=True);summary=[];revision=subprocess.check_output(['git','describe','--always','--dirty'],cwd=Path(__file__).resolve().parents[2],text=True).strip();sha=hashlib.sha256(Path(binary).read_bytes()).hexdigest()
 for repeat in range(repeats):
  root=out/f'r{repeat+1}'
  with Fixture(root,binary,domain_bytes=budget) as f:
   layout=seed_workspace(f.peer);op=seed_operational(f.peer,budget,fill);history=f.peer.call('workspace.history',{'limit':500})['txns'];rows,metrics=asyncio.run(load(f,layout,rate,seconds,traffic))
   final_history=f.peer.call('workspace.history',{'limit':500})['txns'];layouts=[t for t in final_history if (t.get('note') or '').startswith('C1 bench layout')];assert len(final_history)==len(history)+len(layouts);assert not any((t.get('note') or '').startswith('C1 bench record') for t in final_history)
   ok=[r for r in rows if r['status']=='ok'];errors=[r for r in rows if r['status']!='ok']
   result={'traffic_class':traffic,'fill':fill,'source_revision':revision,'binary_sha256':sha,'offered_rate':rate,'offer_seconds':seconds,'offered':len(rows),'ok':len(ok),'errors':len(errors),'unknown':sum(r['status']=='unknown' for r in rows),'commit':pct([r['latency_ms'] for r in ok]),'by_kind':{kind:pct([r['latency_ms'] for r in ok if r['kind']==kind]) for kind in ['layout','operational']},'by_class':{kind:pct([r['latency_ms'] for r in ok if r['kind']=='operational' and r['control']==control]) for kind,control in [('normal',False),('control',True)]},'start_lag':pct([r['start_lag_ms'] for r in rows]),'errors_by_kind':{kind:sum(r['kind']==kind for r in errors) for kind in ['layout','operational']},'seed_op_bytes_used':op['bytes_used'],'declared_op_bytes':budget,'workspace_file_bytes':(root/'state/workspace.json').stat().st_size,'operational_file_bytes':(root/'state/operational/inbox.json').stat().st_size,'history_records':len(final_history),**metrics}
   (root/'samples.json').write_text(json.dumps(rows,separators=(',',':'))+'\n');(root/'result.json').write_text(json.dumps(result,indent=2)+'\n');summary.append(result);print(json.dumps(result),flush=True)
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--core',required=True);p.add_argument('--out',required=True);p.add_argument('--rate',type=int,default=100);p.add_argument('--seconds',type=float,default=30);p.add_argument('--repetitions',type=int,default=3);p.add_argument('--domain-bytes',type=int,default=8388608);p.add_argument('--traffic',choices=['normal','control','mixed'],default='normal');p.add_argument('--fill',choices=['normal','full'],default='normal');a=p.parse_args();run(a.core,a.out,a.rate,a.seconds,a.repetitions,a.domain_bytes,a.traffic,a.fill)
