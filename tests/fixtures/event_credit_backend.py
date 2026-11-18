"""Private finite-window event CREDIT fixture. Never invokes hc."""
import json, struct, sys
from pathlib import Path
inp,out=sys.stdin.buffer,sys.stdout.buffer
root=Path(sys.argv[1])
def exact(n):
    data=b''
    while len(data)<n:
        part=inp.read(n-len(data))
        if not part: raise EOFError
        data+=part
    return data
def send(channel,kind,payload):
    out.write(struct.pack('<IBI',channel,kind,len(payload))+payload);out.flush()
def event(kind,data,seq):
    return json.dumps({'method':'event','params':{'terminal':'t1','type':kind,'data':data,'seq':seq}}).encode()+b'\n'
json.loads(inp.readline())
out.write(b'{"jsonrpc":"2.0","id":0,"result":{"framed":true,"core":{"id":"event-credit-fixture"},"version":"fixture"}}\n');out.flush()
channel,kind,size=struct.unpack('<IBI',exact(9));request=json.loads(exact(size))
assert channel==0 and request['method']=='events.subscribe'
send(0,0,json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'channel':1,'seq':100,'terminals':[{'address':'t1','created_ms':7,'state':'live','argv':['sh']}]}}).encode())
messages=[event('title',{'title':'one'},101),event('title',{'title':'two'},102),event('title',{'title':'tri'},103),event('finished',{'reason':'exited','exit_code':0},104)]
# hc's event channel is NDJSON bytes, not one JSON object per DATA frame. Exercise both
# coalescing and arbitrary splits, while requiring consumption credit to make progress.
a,b=len(messages[1])//2,len(messages[2])//2
records=[messages[0]+messages[1][:a],messages[1][a:]+messages[2][:b],messages[2][b:]+messages[3]]
missed=json.dumps({'method':'missed','params':{'seq':110,'terminals':[{'address':'t1','created_ms':7,'name':'snapshot','state':'finished','reason':'exited','exit_code':0}]}}).encode()+b'\n'
stale=event('created',{'address':'t1','created_ms':7,'state':'live','name':'stale'},109)
extra=missed+stale
window=max(map(len,records));records += [extra[i:i+window] for i in range(0,len(extra),window)]
credited=0;total=sum(map(len,records))
def credit(channel,kind,payload):
    global window,credited
    if channel==1 and kind==1:
        amount,=struct.unpack('<I',payload);window+=amount;credited+=amount
        with (root/'credits').open('a') as log:log.write(str(amount)+'\n')
        if credited==total:(root/'all-consumed').write_text('yes')
try:
    for i,record in enumerate(records):
        while window<len(record):
            channel,kind,size=struct.unpack('<IBI',exact(9));payload=exact(size)
            credit(channel,kind,payload)
        window-=len(record);send(1,0,record)
        if i==0:(root/'first-sent').write_text('yes')
    (root/'finished-sent').write_text('yes')
    while True:
        channel,kind,size=struct.unpack('<IBI',exact(9));credit(channel,kind,exact(size))
except EOFError:pass
