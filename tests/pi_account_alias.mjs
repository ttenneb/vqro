// Pure API fakes, no native runtime/library/login/model/network invocation.
import assert from 'node:assert/strict';
import extension, {accountMetadata} from '../extensions/pi_accounts/index.mjs';
const record = {id:'b',label:'B',provider:'fake-base',native_provider:'vqro-account-b'};
const session = '01234567-89ab-4cde-8fab-0123456789ab';
const physical = {id:'custom',provider:record.provider,api:'fake-api',headers:{custom:'retained'}};
const nativeOAuth = {login:()=>{throw Error('no login')},refresh:()=>{throw Error('no refresh')}};
function fixture({raw=JSON.stringify(record),login=false,configured=true,model=physical,notifyFails=false,restoreThinking=true}={}) {
  const handlers={},commands={},providers=new Map();const notices=[];let stops=0;let selections=0;let idle=true;let pending=false;
  let thinking='high';
  providers.set(record.provider,{id:record.provider,auth:{oauth:nativeOAuth},getModels:()=>[physical],
    stream:()=>{throw Error('no model calls')},streamSimple:()=>{throw Error('no model calls')}});
  const context={model,ui:{notify:(message)=>{if(notifyFails)throw Error('broken UI');notices.push(message)}},
    shutdown:()=>{stops++},isIdle:()=>idle,hasPendingMessages:()=>pending,
    sessionManager:{getSessionId:()=>session,getSessionFile:()=>'/synthetic/native-history.jsonl',buildSessionContext:()=>({model:{modelId:physical.id}})},
    modelRegistry:{getProvider:id=>providers.get(id),refresh:async options=>assert.equal(options.allowNetwork,false),
      getModelOfType:(_type,id,modelId)=>providers.get(id)?.getModels().find(row=>row.id===modelId)}};
  const pi={registerFlag:()=>{},getFlag:name=>name==='vqro-account'?(raw === null ? undefined : raw):login,
    getSettings:()=>({defaultModel:physical.id}),on:(event,callback)=>{handlers[event]=callback},
    registerCommand:(name,value)=>{commands[name]=value},registerProvider:value=>providers.set(value.id,value),
    getThinkingLevel:()=>thinking,setThinkingLevel:value=>{if(restoreThinking)thinking=value},
    // Pi1.5.2 applies the shared/per-model default during model selection,
    // before falling back to the old current thinking level.
    setModel:async model=>{if(!configured)return false;selections++;context.model=model;thinking='medium';return true}};
  extension(pi);
  return {context,handlers,commands,providers,notices,get thinking(){return thinking},get stops(){return stops},get selections(){return selections},setIdle:value=>{idle=value},setPending:value=>{pending=value}};
}
// An earlier configured extension may submit from its session_start before
// our initializer. A declared alias must fence input even before metadata parses.
const beforeStart=fixture();
assert.equal(beforeStart.handlers.input({},beforeStart.context).action,'handled');
assert.equal(beforeStart.context.model.provider,record.provider);
assert.equal(beforeStart.selections,0);
const beforeLogin=fixture({login:true});
assert.equal(beforeLogin.handlers.input({},beforeLogin.context).action,'handled');
const normalBeforeStart=fixture({raw:null});
assert.equal(normalBeforeStart.handlers.input({},normalBeforeStart.context).action,'continue');
const good=fixture();await good.handlers.session_start({},good.context);
assert.equal(good.thinking,'high');
assert.equal(good.context.model.provider,record.native_provider);assert.equal(good.selections,1);assert.equal(good.stops,0);
assert.equal(good.providers.get(record.native_provider).auth.oauth,nativeOAuth);
assert.equal(good.context.model.headers.custom,'retained');
assert.equal(good.handlers.input({},good.context).action,'continue');
const effortFailure=fixture({restoreThinking:false});await effortFailure.handlers.session_start({},effortFailure.context);
assert.equal(effortFailure.stops,1);assert.equal(effortFailure.handlers.input({},effortFailure.context).action,'handled');
const missing=fixture({configured:false});await missing.handlers.session_start({},missing.context);
assert.equal(missing.stops,1);assert.equal(missing.handlers.input({},missing.context).action,'handled');
const broken=fixture({configured:false,notifyFails:true});await broken.handlers.session_start({},broken.context);
assert.equal(broken.stops,1);assert.equal(broken.handlers.input({},broken.context).action,'handled');
const malformed=fixture({raw:'{"token":"not-a-real-secret"}'});await malformed.handlers.session_start({},malformed.context);
assert.equal(malformed.stops,1);assert.equal(malformed.handlers.input({},malformed.context).action,'handled');
const normal=fixture({raw:null});await normal.handlers.session_start({},normal.context);
assert.equal(normal.stops,0);assert.equal(normal.selections,0);assert.equal(normal.handlers.input({},normal.context).action,'continue');
const signin=fixture({login:true,configured:false});await signin.handlers.session_start({},signin.context);
assert.equal(signin.stops,0);assert.equal(signin.selections,0);
assert.ok(signin.notices.some(text=>text.includes('/login')));
const virtual=fixture({model:{...physical,api:'pi-virtual'}});await virtual.handlers.session_start({},virtual.context);
assert.equal(virtual.stops,1);assert.ok(virtual.notices.includes('account switching unavailable for this model'));
const restored=fixture({model:null});await restored.handlers.session_start({},restored.context);
assert.equal(restored.context.model.provider,record.native_provider);assert.equal(restored.stops,0);
const prior=good.providers.get(record.provider);
good.providers.set(record.provider,{...prior,getModels:()=>[{...physical,headers:{custom:'later override'}}]});
assert.equal(good.providers.get(record.native_provider).getModels()[0].headers.custom,'later override');
const collision=fixture();collision.providers.set(record.native_provider,{id:record.native_provider});
await collision.handlers.session_start({},collision.context);assert.equal(collision.stops,1);assert.equal(collision.selections,0);
good.context.model=physical;assert.equal(good.handlers.input({},good.context).action,'handled');
const command=good.commands['vqro-account-quiesce'];
good.handlers.message_end({message:{role:'assistant',stopReason:'error',timestamp:123456}},good.context);
const stopRequest={session,file:'/synthetic/native-history.jsonl',provider:physical.provider,model:physical.id,error_timestamp:123456};
await command.handler(JSON.stringify({...stopRequest,file:'/different/history.jsonl'}),good.context);assert.equal(good.stops,0);
await command.handler(JSON.stringify({...stopRequest,file:null}),good.context);assert.equal(good.stops,0);
await command.handler(JSON.stringify(stopRequest),good.context);assert.equal(good.stops,1);
good.setIdle(false);await command.handler(JSON.stringify(stopRequest),good.context);assert.equal(good.stops,1);
good.setIdle(true);await command.handler(JSON.stringify({...stopRequest,session:'different'}),good.context);assert.equal(good.stops,1);
await command.handler(JSON.stringify({...stopRequest,model:'changed-model'}),good.context);assert.equal(good.stops,1);
good.setPending(true);await command.handler(JSON.stringify(stopRequest),good.context);assert.equal(good.stops,1);
good.setPending(false);good.handlers.agent_start({},good.context);
await command.handler(JSON.stringify(stopRequest),good.context);assert.equal(good.stops,1);
good.context.model={...physical,api:'pi-virtual'};
await command.handler(JSON.stringify(stopRequest),good.context);assert.equal(good.stops,1);
assert.throws(()=>accountMetadata(JSON.stringify({...record,secret:'never allowed'})));
assert.throws(()=>accountMetadata(JSON.stringify({...record,label:'\ud800'})));
console.log('PASS native account extension API fakes: alias startup, no-auth failure, broken UI, login-only, virtual unavailable, restored metadata, input fence, quiescent exact-session shutdown, strict metadata');
