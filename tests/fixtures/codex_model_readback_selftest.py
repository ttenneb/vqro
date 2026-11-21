#!/usr/bin/env python3
"""Private stdlib-only native model readback barrier/ledger test, no real runtime/model."""
import argparse
import asyncio
import json
from pathlib import Path
from codex_control_selftest import Client

async def checks(args):
    args.root.mkdir(parents=True,exist_ok=True)
    async with Client(args) as native:
        await native.rpc('initialize',{'clientInfo':{'name':'model-fixture-check','version':'0'}})
        session=await native.new()
        await native.control('pause_before',phase='model_readback')
        started=await native.rpc('turn/start',{'threadId':session,'model':'fixture-large','input':[{'type':'text','text':'fake-only','text_elements':[]}]})
        assert started['result']['turn']['id']
        await native.until(lambda:any(row['command']=='turn/start' for row in native.records('native_response')))
        reading=asyncio.create_task(native.rpc('thread/read',{'threadId':session,'includeTurns':False}))
        await native.until(lambda:any(row.get('phase')=='model_readback' for row in native.records('paused')))
        assert not reading.done()
        assert not any(row['command']=='thread/read' for row in native.records('native_response'))
        assert (await native.control('state',session=session))['model']=='fixture-large'
        await native.control('release',phase='model_readback')
        response=await reading
        assert response['result']['thread']['id']==session and response['result']['thread']['model']=='fixture-large'
        await native.until(lambda:any(row['command']=='thread/read' for row in native.records('native_response')))
        recorded=[row for row in native.records('native_response') if row['command']=='thread/read'][-1]
        assert recorded['value']==response and recorded['request']==response['id'] and recorded['success']
        await native.control('model_readback',session=session,model='fixture-small')
        assert (await native.rpc('thread/read',{'threadId':session,'includeTurns':False}))['result']['thread']['model']=='fixture-small'
        await native.control('model_readback',session=session,omit=True)
        assert (await native.rpc('thread/read',{'threadId':session,'includeTurns':False}))['result']['thread']['model'] is None
        await native.control('model_readback',session=session,reject=True)
        assert 'error' in await native.rpc('thread/read',{'threadId':session,'includeTurns':False})
    print(json.dumps({'result':'pass','wire':'codex','checks':8,'real_native_model_calls':0}))

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--harness-root',type=Path,required=True);parser.add_argument('--root',type=Path,required=True)
    asyncio.run(checks(parser.parse_args()))
