#!/usr/bin/env python3
"""Private public-wire30/200 panel A/B using actual bridge renderers, not a native-process soak.

Control-style20attempts/s, prompt:abort:steer=2:1:1; rejected busy prompts do not publish. Native
fixtures are modeled deterministically; no model, HC or inherited endpoint. All transcripts pre-warm128rows.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'extensions'))
from e2e_sidebar_scale import Client, wait
from pi_bridge.client import PiRpcClient, Limits
from pi_bridge.display import Transcript


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(args, count, before):
    root = Path(tempfile.mkdtemp(prefix='bw-', dir=args.scratch))
    for name in ('ext','state','run','home'):
        (root/name).mkdir()
    for path in (ROOT/'extensions').glob('*.py'):
        shutil.copy2(path, root/'ext'/path.name)
    if before:
        shutil.copy2(args.before_agents, root/'ext/agents.py')
    (root/'ext/defaults.json').write_text(json.dumps({'version':1,'extensions':[{'name':'agents','argv':['/usr/bin/python3',str(root/'ext/agents.py')]}]}))
    env = {'HOME':str(root/'home'),'PATH':'/usr/bin:/bin','LANG':'C.UTF-8','PRIM_SOCKET':str(root/'p.sock'),
           'PRIM_STATE_DIR':str(root/'state'),'PRIM_EXTENSIONS_DIR':str(root/'ext'),'PRIM_HERDR':'["/bin/false"]',
           'XDG_RUNTIME_DIR':str(root/'run'),'HC_SOCKET':str(root/'h.sock'),'HC_SESSION':'panel-bandwidth'}
    program = load('pi_bridge.before_program' if before else 'pi_bridge.current_program', args.before_program if before else ROOT/'extensions/pi_bridge/program.py')
    agents_module = load('before_agents' if before else 'current_agents', args.before_agents if before else ROOT/'extensions/agents.py')
    clients = []
    with (root/'serve.log').open('w') as log:
        core = subprocess.Popen([str(args.prim),'serve'],env=env,stdout=log,stderr=log)
    try:
        wait(lambda:(root/'p.sock').exists())
        for _ in range(3):
            clients.append(Client(str(root/'p.sock')))
        control, provider, observer = clients
        observer.call('events.subscribe',{'types':['view.content']})
        gen = provider.call('provider.register',{'name':'bandwidth','instance':'private','role':'backend',
             'software':{'name':'panel-fixture','version':'1'},'provider_version':'0.1.0','capabilities':{},
             'limits':{'max_streams':1,'stream_window_bytes':65536}})['generation']
        facts = [{'ref':str(i),'kind':'agent','incarnation':'one','state':'live','name':'Pi','data':{}} for i in range(count)]
        provider.call('provider.sync',{'generation':gen,'resources':facts,'final':True})
        resources = control.call('resource.list')['resources']
        sources, native, current = [], {}, {}
        rng = random.Random(7)
        for resource in resources:
            source = program.Bridge.__new__(program.Bridge)
            source.pi = PiRpcClient(Limits(),set())
            active = rng.random() < 2/3
            source.pi._facts.update(status='working' if active else 'done',run={'active':active,'settled':not active,'aborted':None})
            source.transcript = Transcript()
            for i in range(128):
                source.transcript.append('tool fixture: started' if i%2 else 'user> soak')
            source.transcript.clipped = True
            source.rid, source.cwd, source.view_problem = resource['id'],root,None
            source.view, source.status_view = 'agent-'+source.rid,'agent-status-'+source.rid
            source.ref = resource['backend_ref']
            source.cached = {}
            sources.append(source)
        shell = agents_module.Agents.__new__(agents_module.Agents)
        shell.ext = SimpleNamespace(live=native,resources=current,entries={})
        def publish(source):
            fact = source.facts(); fact.update(ref=source.ref,incarnation='one')
            current[source.rid] = dict(fact,id=source.rid,available=True,provider='bandwidth')
            provider.notify('resource.upsert',{'generation':gen,'facts':fact})
            parts = [(source.view,None)] if before else [(source.view,'output'),(source.status_view,'status')]
            for view, part in parts:
                body = source.content() if before else source.content(part)
                native[view] = body
                if source.cached.get(view) != body:
                    provider.call('view.publish',{'generation':gen,'view':view,'content':body})
                    source.cached[view] = body
        def complete():
            with observer.cv:
                return all(observer.last.get(view) == body for view,body in native.items()) and all(
                    observer.last.get('agents-'+rid) == shell.panel(resource) for rid,resource in current.items())
        for source in sources:
            publish(source)
        wait(complete)
        baseline = observer.stats()
        accepted = rejected = 0
        started = time.monotonic()
        for tick in range(int(args.seconds*20)):
            source = rng.choice(sources)
            op = rng.choice(('prompt','prompt','abort','steer'))
            state = source.pi._facts
            if op == 'prompt' and state['run']['active']:
                rejected += 1
            else:
                accepted += 1
                if op == 'prompt':
                    state.update(status='working',run={'active':True,'settled':False,'aborted':None})
                    source.transcript.append('tool fixture: started')
                elif op == 'steer':
                    state['queues']['steering'].append('soak')
                else:
                    for _ in state['queues']['steering']:
                        source.transcript.append('user> soak')
                    state['queues']['steering'].clear()
                    state.update(status='done',run={'active':False,'settled':True,'aborted':True})
                publish(source)
            time.sleep(max(0,started+(tick+1)/20-time.monotonic()))
        wait(complete)
        elapsed = time.monotonic()-started
        after = observer.stats()
        totals = {prefix:sum(size-baseline[1].get(name,0) for name,size in after[1].items() if name.startswith(prefix))
                  for prefix in ('agent-r','agent-status-','agents-')}
        snapshot = control.call('workspace.snapshot')
        assert snapshot['extensions']['agents']['display_status'] == 'running'
        result = {'agents':count,'before':before,'seconds':elapsed,'accepted':accepted,'rejected':rejected,
                  'bytes':sum(totals.values()),'bytes_per_minute':sum(totals.values())*60/elapsed,'by_source':totals,
                  'profile':'20attempts/s,2prompt:1abort:1steer,128warmrows,actual renderers; synthetic native state',
                  'public_observer_fence':True,'root':str(root)}
        (root/'receipt.json').write_text(json.dumps(result,indent=2)+'\n')
        return result
    finally:
        if clients:
            try:
                clients[0].call('core.shutdown')
            except Exception:
                core.terminate()
        core.wait(timeout=10)
        for client in clients:
            client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('prim',type=Path)
    parser.add_argument('--before-agents',type=Path,required=True)
    parser.add_argument('--before-program',type=Path,required=True)
    parser.add_argument('--scratch',type=Path,required=True)
    parser.add_argument('--seconds',type=int,default=5)
    parser.add_argument('--report',type=Path,required=True)
    args = parser.parse_args()
    args.scratch.mkdir(parents=True,exist_ok=True)
    results = [run(args,count,before) for count in (30,200) for before in (True,False)]
    args.report.write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results,indent=2))


if __name__ == '__main__':
    main()
