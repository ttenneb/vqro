import test from 'node:test';
import assert from 'node:assert/strict';
import { createDecisionRegistry } from './index.mjs';
import { decisionInputDigest } from '../../contracts/work-v1/canonical.mjs';
import { evaluate as deterministic, POINTS } from '../../contracts/work-v1/decision-defaults.mjs';

const task = (id,parent_id=null,assigned_actor_id='agent-a',state='active',assignment_sequence='1') => ({id,parent_id,assigned_actor_id,state,assignment_sequence});
const source = () => ({entry_id:'entry-a',entry_version:'2',thread_id:'thread-a',sender_actor_id:'human-a',recipient_actor_id:'agent-a',run_id:'run-a'});
function request(point,input) {
  const r={contract:'vqro.decision.request.v1',request_id:'req-a',point,scope_id:'scope-a',input_revision:'9007199254740993',input_digest:'',policy_revision:'4',mode:'auto_apply',budget_ms:250,
    context:{actor_id:'agent-a',actor_class:'agent',actor_current:true,execution_current:true,human_explicit:false,human_route:null,assigned_roots:['root'],tasks:[task('root'),task('child','root','agent-a','active','9007199254740993'),task('other',null,'other-a')]},input};
  r.input_digest=decisionInputDigest(r);r.request_id=`req-${r.input_digest.slice(0,32)}`; return r;
}
function input(point) { return {
  'recipient.routing':{recipient_actor_id:'human-a',recipient_state:'offline'},
  'message.triage':{entry_id:'entry-a'},
  'task.next':{}, 'report.acceptance':{report_entry_id:'report-a'},
  'action.confirmation':{action:'work.entry.reply.v1',task_id:'child',parent_task_id:null,cross_subtree:false},
  'turn.output_route':{run_id:'run-a',outcome:'completed',source:source(),explicit_human_actor_id:null,explicit_human_available:false},
}[point]; }
function updated(r, mutate) { const next=structuredClone(r); mutate(next); next.input_digest=decisionInputDigest(next); next.request_id=`req-${next.input_digest.slice(0,32)}`; return next; }
class FakeBroker {
  constructor() { this.state={store_id:'store-a',store_generation:'1',head:0,actor_id:'agent-a',scope_id:'scope-a',actor_current:true,execution_current:true,source_current:true,input_revision:'9007199254740993',policy_revision:'4',input_digest:null}; this.audit=[];this.effects=[];this.receipts=new Map();this.grants=new Set();this.auditAvailable=true;this.state.provider_generation='1'; }
  async admit(input) {
    // Fake trust boundary only. Real Work builds context from host lease and canonical records.
    this.state.input_digest=input.input_digest;
    return {request:input,actor_id:this.state.actor_id,store_id:this.state.store_id,store_generation:this.state.store_generation};
  }
  async current() { return {...this.state}; }
  async authorizeProvider(_admission, provider) { return this.grants.has(provider.id); }
  async apply(resolution) {
    const {request,plan,provenance}=resolution;
    const key=`${provenance.store_id}:${provenance.actor_id}:${request.request_id}`;
    const prior=this.receipts.get(key);
    if (prior) return prior.digest===request.input_digest ? prior.kind : 'duplicate_conflict';
    if (plan.kind==='no_effect') {
      if (!this.auditAvailable) return 'audit_unavailable';
      this.audit.push({...provenance,head:++this.state.head});
      this.receipts.set(key,{digest:request.input_digest,kind:'no_effect'});
      return 'no_effect';
    }
    const s=this.state;
    if (!this.auditAvailable) return 'audit_unavailable';
    if (!(await resolution.revalidate())) return 'conflict';
    if (s.store_id!==provenance.store_id || s.store_generation!==provenance.store_generation || s.actor_id!==provenance.actor_id || !s.actor_current ||
      s.execution_current!==request.context.execution_current || !s.source_current || s.scope_id!==request.scope_id ||
      s.input_revision!==request.input_revision || s.input_digest!==request.input_digest || s.policy_revision!==request.policy_revision) return 'conflict';
    if (plan.kind==='commit_action') {
      // Fake Work independently checks hard authority at its own CAS boundary.
      if (request.point!=='action.confirmation' || request.mode!=='auto_apply' ||
        (await deterministic(request)).proposal.authorization!=='auto_apply') return 'hard_policy_denied';
    }
    // Fake CAS models an atomic journal record, not a real Work broker or durable transaction.
    this.audit.push({...provenance,head:++s.head});
    if (plan.kind==='commit_action') this.effects.push({action:request.input.action,actor_id:s.actor_id,head:s.head});
    this.receipts.set(key,{digest:request.input_digest,kind:plan.kind});
    return plan.kind;
  }
}
function setup() { const broker=new FakeBroker();const registry=createDecisionRegistry({broker,now:()=>0});return {broker,registry}; }
function provider(id='custom-a', evaluate=deterministic, generation='1') {
  return {id,version:'1.0.0',generation,points:POINTS,capability:'work.decision.evaluate.v1',backend:'component',
    evaluate:async (r,options) => ({...await evaluate(r,options),provider_id:id,provider_generation:generation})};
}

test('all six defaults async and deterministic; fake audit CAS is never a hidden action committer', async () => {
  const {broker,registry}=setup();let committed;
  for (const point of POINTS) {
    const r=request(point,input(point)), a=await registry.resolve(r), b=await registry.resolve(r);
    assert.deepEqual(a.result,b.result);assert.equal(a.result.point,point);assert.equal(a.result.provider_id,'example.work-defaults');
    assert.equal(a.provenance.input_digest,r.input_digest);
    assert.equal(await broker.apply(a),a.plan.kind);
    if (point==='action.confirmation') committed=a;
  }
  assert.equal(await broker.apply(committed),'commit_action','duplicate returns original fake receipt');
  assert.equal(broker.effects.length,1);
  assert.equal(broker.audit.length,6);
  assert.equal((await registry.resolve(request('turn.output_route',input('turn.output_route')))).plan.kind,'proposal_only','thread target never auto-posts');
});

test('suggest/manual and hard subtree/human consent cannot be expanded by provider', async () => {
  const {registry,broker}=setup();
  const r=request('action.confirmation',input('action.confirmation'));
  const custom=provider('custom-a',async req => ({...await deterministic(req),provider_id:'custom-a',proposal:{authorization:'auto_apply'}}));
  registry.register(custom);broker.grants.add('custom-a');
  for (const action of ['work.report.accept.v1','work.report.reject.v1','work.resume.v1','work.output.route.v1']) {
    const restricted=updated(r,next=>{next.input.action=action;});
    const result=await registry.resolve(restricted,{providerId:'custom-a'});
    assert.equal(result.provenance.fallback_cause,'invalid_proposal');assert.equal(result.plan.kind,'request_confirmation');
  }
  for (const mutation of [next=>{next.input.task_id='other';},next=>{next.input.task_id=null;},next=>{next.input.cross_subtree=true;}]) {
    const result=await registry.resolve(updated(r,mutation),{providerId:'custom-a'});
    assert.equal(result.plan.kind,'request_confirmation');
  }
  const accept=await registry.resolve(updated(r,next=>{next.input.action='work.report.accept.v1';}));
  assert.equal(await broker.apply({...accept,plan:{kind:'commit_action',proposal:{authorization:'auto_apply'}}}),'hard_policy_denied','fake Work checks independently');
  const suggest=await registry.resolve(updated(r,next=>{next.mode='suggest';}));
  assert.equal(suggest.plan.kind,'record_suggestion');await broker.apply(suggest);assert.equal(broker.effects.length,0);
  const human=updated(r,next=>{next.context={...next.context,actor_id:'human-a',actor_class:'human',human_explicit:true,assigned_roots:[],tasks:[],execution_current:false};});
  broker.state.actor_id='human-a';broker.state.execution_current=false;
  assert.equal((await registry.resolve(human)).plan.kind,'commit_action');
  assert.equal((await registry.resolve(updated(human,next=>{next.context.human_explicit=false;}))).plan.kind,'request_confirmation');
});

test('public provider registration/grant, wrong identities, invalid typed result and provider isolation', async () => {
  const {registry,broker}=setup();const r=request('message.triage',input('message.triage'));
  registry.register(provider());
  const descriptors=registry.interfaces();
  assert.equal(descriptors.length,2);
  assert.equal(descriptors[1].points.length,6);
  assert.equal(descriptors[1].points[0].request_contract,'vqro.decision.request.v1');
  assert.throws(()=>{descriptors[1].points.push('forged');});
  assert.throws(()=>registry.register(provider()),/registered/);
  assert.equal((await registry.resolve(r,{providerId:'custom-a'})).provenance.fallback_cause,'provider_unavailable');
  broker.grants.add('custom-a');
  assert.equal((await registry.resolve(r,{providerId:'custom-a'})).result.provider_id,'custom-a');
  broker.state.provider_generation='2';
  assert.equal((await registry.resolve(r,{providerId:'custom-a'})).plan.kind,'no_effect','selected generation must stay current');
  broker.state.provider_generation='1';
  registry.revoke('custom-a');
  assert.throws(()=>registry.register(provider()),/generation must increase/);
  registry.register(provider('custom-a',async req => ({...await deterministic(req),provider_id:'custom-a',request_id:'forged'}),'2'));
  assert.equal((await registry.resolve(r,{providerId:'custom-a'})).provenance.fallback_cause,'invalid_proposal');
  registry.revoke('custom-a');
  registry.register(provider('custom-a',async req => ({...await deterministic(req),provider_id:'custom-a',proposal:{priority:'high',disposition:'interrupt'}}),'3'));
  const invalid=await registry.resolve(r,{providerId:'custom-a'});
  assert.equal(invalid.provenance.fallback_cause,'invalid_proposal');assert.deepEqual(invalid.result.proposal,{priority:'normal',disposition:'queue'});
  assert.equal((await registry.resolve(r,{providerId:'unregistered'})).result.provider_id,'example.work-defaults');
});

test('swappable provider may prioritize eligible task, never cross actor or subtree', async () => {
  const {broker,registry}=setup();broker.grants.add('pick-a');
  const picked=provider('pick-a',async req=>({...await deterministic(req),proposal:{task_id:'child'}}));
  registry.register(picked);
  const r=request('task.next',{});
  const valid=await registry.resolve(r,{providerId:'pick-a'});
  assert.equal(valid.result.provider_id,'pick-a');assert.equal(valid.result.proposal.task_id,'child');
  registry.revoke('pick-a');
  const wrong=provider('pick-a',async req=>({...await deterministic(req),proposal:{task_id:'other'}}),'2');
  registry.register(wrong);broker.state.provider_generation='2';
  const denied=await registry.resolve(r,{providerId:'pick-a'});
  assert.equal(denied.provenance.fallback_cause,'invalid_proposal');
});

test('foreign provider result cannot smuggle identity, version, mode or unbounded evidence', async () => {
  const mutations=[
    ['request_id','wrong'],['scope_id','wrong'],['input_revision','2'],['input_digest','f'.repeat(64)],
    ['policy_revision','2'],['mode','suggest'],['provider_id','other-a'],['provider_version','2'],
    ['provider_generation','2'],['outcome','unsupported'],['explanation','😀'.repeat(300)],
    ['evidence_ids',Array.from({length:17},(_,i)=>`evidence-${i}`)],
  ];
  for (const [key,value] of mutations) {
    const {broker,registry}=setup();broker.grants.add('bad-a');
    const bad=provider('bad-a');
    bad.evaluate=async req=>({...await deterministic(req),provider_id:'bad-a',[key]:value});
    registry.register(bad);
    const result=await registry.resolve(request('message.triage',input('message.triage')),{providerId:'bad-a'});
    assert.equal(result.provenance.fallback_cause,'invalid_proposal',key);
    assert.equal(result.result.provider_id,'example.work-defaults');
    assert.equal(await broker.apply(result),'proposal_only');
    assert.equal(broker.audit[0].fallback_cause,'invalid_proposal');
  }
});

test('precision, eligible subtree selection, no wrong recipient or free-thread output', async () => {
  const {registry}=setup();let r=request('task.next',{});
  r=updated(r,next=>{next.context.tasks[0].state='blocked';next.context.tasks.push(task('z','root','agent-a','active','9007199254740994'),task('a','root','agent-a','active','9007199254740993'));});
  assert.equal((await registry.resolve(r)).result.proposal.task_id,'a'); // uint64 precision; sequence tie uses task ID
  const route=request('turn.output_route',input('turn.output_route'));
  for (const change of [n=>{n.input.source=null;},n=>{n.input.outcome='error';},n=>{n.input.source.run_id='other-run';}])
    assert.equal((await registry.resolve(updated(route,change))).result.proposal.route,'unrouted');
  const flag=updated(route,n=>{n.input.explicit_human_actor_id='human-a';n.input.explicit_human_available=true;});
  assert.equal((await registry.resolve(flag)).result.proposal.route,'thread_reply');
  const granted=updated(flag,n=>{n.context.human_route={authorization_id:'grant-a',run_id:'run-a',recipient_actor_id:'human-a'};});
  const singleRoute=await registry.resolve(granted);
  assert.equal(singleRoute.result.proposal.route,'explicit_human');
  assert.equal(singleRoute.plan.kind,'proposal_only','route never posts or duplicates an already routed reply');
  assert.equal(singleRoute.result.proposal.thread_id,null);
  for (const change of [n=>{n.input.source=null;},n=>{n.input.source.run_id='other-run';},
    n=>{n.input.source.recipient_actor_id='other-a';},n=>{n.context.execution_current=false;},
    n=>{n.context.actor_current=false;}]) {
    const inconsistent=await registry.resolve(updated(granted,change));
    assert.equal(inconsistent.plan.kind,'no_effect','human route needs exact source/run/recipient proof');
    assert.equal(inconsistent.provenance.fallback_cause,'invalid_source');
  }
  const missing=request('recipient.routing',{recipient_actor_id:null,recipient_state:'unregistered'});
  assert.equal((await registry.resolve(missing)).result.proposal.recipient_actor_id,null);
});

test('stale input/digest/policy/actor/source/store prevent apply; audit failure prevents effect', async () => {
  for (const key of ['input_revision','input_digest','policy_revision','actor_id','scope_id','store_id','store_generation','actor_current','source_current','execution_current']) {
    const {broker,registry}=setup(),r=request('action.confirmation',input('action.confirmation'));
    const resolution=await registry.resolve(r);
    broker.state[key]=key.endsWith('current') ? false : 'different';
    assert.equal(await broker.apply(resolution),'conflict',key);assert.equal(broker.effects.length,0);
  }
  const {broker,registry}=setup();const r=request('action.confirmation',input('action.confirmation'));
  broker.auditAvailable=false;
  assert.equal(await broker.apply(await registry.resolve(r)),'audit_unavailable');assert.equal(broker.effects.length,0);
  const changed=structuredClone(r);changed.input.task_id='other'; // stale same-revision semantic tampering
  await assert.rejects(()=>registry.resolve(changed));
  const frozen=await registry.resolve(r);
  assert.throws(()=>{frozen.result.proposal.authorization='suggest_confirm';});
  assert.throws(()=>{frozen.provenance.evidence_ids.push('forged');});
});

test('deterministic fallback is audited in the same fake CAS or blocked without effect', async () => {
  const {broker,registry}=setup();broker.grants.add('broken-a');
  registry.register(provider('broken-a',()=>{throw new Error('unavailable');}));
  const r=request('action.confirmation',input('action.confirmation'));
  const fallback=await registry.resolve(r,{providerId:'broken-a'});
  assert.equal(fallback.plan.kind,'commit_action');
  assert.equal(fallback.result.outcome,'fallback');
  assert.equal(fallback.provenance.fallback_cause,'provider_error');
  broker.auditAvailable=false;
  assert.equal(await broker.apply(fallback),'audit_unavailable');assert.equal(broker.effects.length,0);
  broker.auditAvailable=true;
  assert.equal(await broker.apply(fallback),'commit_action');
  assert.equal(broker.audit[0].fallback_cause,'provider_error');assert.equal(broker.effects.length,1);
});

test('revoked grant before invocation never calls provider despite delayed authorization', async () => {
  const broker=new FakeBroker();let calls=0;
  const registry=createDecisionRegistry({broker});
  registry.register(provider('vanish-a',async req=>{calls++;return deterministic(req);}));
  broker.authorizeProvider=async()=>{registry.revoke('vanish-a');return true;};
  const result=await registry.resolve(request('message.triage',input('message.triage')),{providerId:'vanish-a'});
  assert.equal(calls,0);assert.equal(result.provenance.fallback_cause,'revoked_or_cancelled');
});

test('revocation aborts a hung grant check before its deadline', async () => {
  const broker=new FakeBroker(),registry=createDecisionRegistry({broker});let calls=0;
  broker.authorizeProvider=()=>new Promise(()=>{});
  registry.register(provider('revoke-hang',async r=>{calls++;return deterministic(r);}));
  const r=request('message.triage',input('message.triage'));
  const pending=registry.resolve(r,{providerId:'revoke-hang'});
  await new Promise(resolve=>setImmediate(resolve));
  registry.revoke('revoke-hang');
  const result=await Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(new Error('revoke hung')),150))]);
  assert.equal(result.plan.kind,'no_effect');assert.equal(calls,0);
});

test('provider completion races Work policy/source changes and human override; only current audit/effect survives', async () => {
  const {broker,registry}=setup();broker.grants.add('race-a');
  const r=request('action.confirmation',input('action.confirmation'));
  registry.register(provider('race-a',async req=>{
    broker.state.policy_revision='5'; // human override or policy change while provider evaluates
    return deterministic(req);
  }));
  const raced=await registry.resolve(r,{providerId:'race-a'});
  assert.equal(raced.plan.kind,'no_effect');assert.equal(await broker.apply(raced),'no_effect');assert.equal(broker.effects.length,0);
  broker.state.policy_revision='4';
  const nextRequest={...r,request_id:'req-next-attempt'};
  const current=await registry.resolve(nextRequest);
  broker.state.source_current=false; // claim/run/source changed after proposal but before fake CAS
  assert.equal(await broker.apply(current),'conflict');assert.equal(broker.effects.length,0);
});

test('bounded provider concurrency falls back rather than queueing unbounded evaluations', async () => {
  const {broker,registry}=setup();broker.grants.add('busy-a');
  registry.register(provider('busy-a',()=>new Promise(()=>{})));
  const r=request('message.triage',input('message.triage'));
  const controllers=Array.from({length:4},()=>new AbortController());
  const active=controllers.map(controller=>registry.resolve(r,{providerId:'busy-a',signal:controller.signal}));
  await new Promise(resolve=>setImmediate(resolve));
  const overflow=await registry.resolve(r,{providerId:'busy-a'});
  assert.equal(overflow.provenance.fallback_cause,'provider_busy');
  assert.equal(overflow.plan.kind,'proposal_only');
  controllers.forEach(controller=>controller.abort());
  for (const result of await Promise.all(active)) assert.equal(result.plan.kind,'no_effect');
});

test('hung Work current snapshot cannot hold resolution past host budget', async () => {
  const broker=new FakeBroker();broker.current=()=>new Promise(()=>{});
  const registry=createDecisionRegistry({broker});
  const r=updated(request('task.next',{}),n=>{n.budget_ms=20;});
  const result=await Promise.race([
    registry.resolve(r),
    new Promise((_,reject)=>setTimeout(()=>reject(new Error('current hung past 150ms')),150)),
  ]);
  assert.equal(result.plan.kind,'no_effect');assert.equal(result.provenance.fallback_cause,'deadline');
});

test('hung provider grant and post-resolution revalidation obey cancellation and deadline', async () => {
  const broker=new FakeBroker();let calls=0;
  broker.authorizeProvider=()=>new Promise(()=>{});
  const registry=createDecisionRegistry({broker,now:()=>0});
  registry.register(provider('grant-hang',async r=>{calls++;return deterministic(r);}));
  const r=updated(request('message.triage',input('message.triage')),n=>{n.budget_ms=20;});
  const grant=await Promise.race([registry.resolve(r,{providerId:'grant-hang'}),
    new Promise((_,reject)=>setTimeout(()=>reject(new Error('grant hung past 150ms')),150))]);
  assert.equal(grant.plan.kind,'no_effect');assert.equal(grant.provenance.fallback_cause,'deadline');assert.equal(calls,0);
  const controller=new AbortController();
  const cancelled=registry.resolve({...r,request_id:'req-cancel-grant'},{providerId:'grant-hang',signal:controller.signal});
  queueMicrotask(()=>controller.abort());
  const cancelledResult=await cancelled;
  assert.equal(cancelledResult.plan.kind,'no_effect');assert.equal(cancelledResult.provenance.fallback_cause,'cancelled');
  const defaultResult=await registry.resolve({...r,request_id:'req-revalidate'});
  assert.equal(defaultResult.plan.kind,'proposal_only');
  broker.current=()=>new Promise(()=>{});
  assert.equal(await Promise.race([defaultResult.revalidate(),
    new Promise((_,reject)=>setTimeout(()=>reject(new Error('revalidate hung past 150ms')),150))]),false);
});

test('uncooperative broker callbacks retain bounded slots after cancellation', async () => {
  const broker=new FakeBroker();let calls=0;
  broker.current=()=>{calls++;return new Promise(()=>{});};
  const registry=createDecisionRegistry({broker,now:()=>0});
  const r=request('task.next',{});
  const controllers=Array.from({length:32},()=>new AbortController());
  const pending=controllers.map((controller,i)=>registry.resolve({...r,request_id:`req-broker-${i}`},{signal:controller.signal}));
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(calls,32);
  controllers.forEach(controller=>controller.abort());
  for (const result of await Promise.all(pending)) assert.equal(result.plan.kind,'no_effect');
  const overflow=await registry.resolve({...r,request_id:'req-broker-overflow'});
  assert.equal(overflow.plan.kind,'no_effect');assert.equal(calls,32);
});

test('timed-out uncooperative provider retains bounded slots until actual settlement', async () => {
  const broker=new FakeBroker(),clock={tick:0};broker.grants.add('hung-a');let calls=0;
  const registry=createDecisionRegistry({broker,now:()=>clock.tick,
    schedule:(fn,ms,kind)=>kind==='broker' ? setTimeout(fn,ms) :
      setImmediate(()=>{clock.tick+=250;fn();})});
  registry.register(provider('hung-a',()=>{calls++;return new Promise(()=>{});}));
  const r=request('message.triage',input('message.triage'));
  for (let i=0;i<4;i++) {
    const timed=await registry.resolve({...r,request_id:`req-hung-${i}`},{providerId:'hung-a'});
    assert.equal(timed.plan.kind,'no_effect');assert.equal(timed.provenance.fallback_cause,'deadline');
  }
  const bounded=await registry.resolve({...r,request_id:'req-hung-overflow'},{providerId:'hung-a'});
  assert.equal(bounded.provenance.fallback_cause,'provider_busy');assert.equal(calls,4);
});

test('plan expiry after evaluation is rechecked immediately before fake Work CAS', async () => {
  const broker=new FakeBroker(),clock={tick:0};
  const registry=createDecisionRegistry({broker,now:()=>clock.tick});
  const plan=await registry.resolve(request('action.confirmation',input('action.confirmation')));
  assert.equal(plan.plan.kind,'commit_action');assert.equal(await plan.revalidate(),true);
  clock.tick=251;
  assert.equal(await plan.revalidate(),false);
  assert.equal(await broker.apply(plan),'conflict');assert.equal(broker.effects.length,0);
});

test('monotonic deadline, cancellation, revoke and late completion cannot commit', async () => {
  const {broker,registry}=setup();broker.grants.add('slow-a');
  let release;
  registry.register(provider('slow-a',()=>new Promise(resolve=>{release=resolve;})));
  const r=request('action.confirmation',input('action.confirmation'));
  const controller=new AbortController();const pending=registry.resolve(r,{providerId:'slow-a',signal:controller.signal});
  await new Promise(resolve=>setImmediate(resolve));controller.abort();
  assert.equal((await pending).plan.kind,'no_effect');
  release({...await deterministic(r),provider_id:'slow-a'});
  const cancelled=await registry.resolve(r,{signal:AbortSignal.abort()});assert.equal(cancelled.plan.kind,'no_effect');
  const second=registry.resolve(r,{providerId:'slow-a'});await new Promise(resolve=>setImmediate(resolve));registry.revoke('slow-a');
  assert.equal((await second).plan.kind,'no_effect');
  const fakeClock={tick:0};const other=new FakeBroker();
  const deadlines=createDecisionRegistry({broker:other,now:()=>fakeClock.tick,
    schedule:(fn,ms,kind)=>kind==='broker' ? setTimeout(fn,ms) :
      (queueMicrotask(()=>{fakeClock.tick=251;fn();}),1)});
  other.grants.add('slow-a');deadlines.register(provider('slow-a',()=>new Promise(()=>{})));
  const timed=await deadlines.resolve(r,{providerId:'slow-a'});
  assert.equal(timed.plan.kind,'no_effect');assert.equal(timed.provenance.fallback_cause,'deadline');
});
