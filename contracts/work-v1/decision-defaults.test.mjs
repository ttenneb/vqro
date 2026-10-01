import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { evaluate as evaluateRaw, currentResult, validateRequest, applicationPlan, POINTS } from './decision-defaults.mjs';
import { decisionInputDigest, freezeAdmission, canonical } from './canonical.mjs';

// Fixture producer mirrors authoritative admission; this is not client authentication.
async function evaluate(req,options) { req.input_digest=decisionInputDigest(req);return evaluateRaw(freezeAdmission(req),options); }
function validateFixture(req) { req.input_digest=decisionInputDigest(req);return validateRequest(req); }

function task(id, parent_id=null, assigned_actor_id='actor-a', state='active', assignment_sequence='1') {
  return {id,parent_id,assigned_actor_id,state,assignment_sequence};
}
function request(point, input) {
  return { contract:'vqro.decision.request.v1', request_id:'req-one', point, scope_id:'scope-one',
    input_revision:'1', input_digest:'a'.repeat(64), policy_revision:'1', mode:'auto_apply', budget_ms:250,
    context:{actor_id:'actor-a',actor_class:'agent',actor_current:true,execution_current:true,human_explicit:false,human_route:null,
      assigned_roots:['task-root'],tasks:[task('task-root'),task('task-child','task-root'),task('task-other',null,'actor-other')]}, input };
}
function source() {
  return {entry_id:'entry-one',entry_version:'2',thread_id:'thread-one',sender_actor_id:'actor-sender',recipient_actor_id:'actor-a',run_id:'run-one'};
}
function confirm(action='work.entry.post.v1', task_id='task-child') {
  return request('action.confirmation',{action,task_id,parent_task_id:null,cross_subtree:false});
}
function current(req) { return {input_revision:'1',input_digest:req.input_digest,policy_revision:'1',provider_generation:'1'}; }

test('all candidate schemas parse and internal references resolve (not full JSON Schema validation)', () => {
  for (const file of ['work.schema.json','decision.schema.json','broker.schema.json']) {
    const schema = JSON.parse(fs.readFileSync(new URL(file,import.meta.url),'utf8'));
    assert.equal(schema.$schema,'https://json-schema.org/draft/2020-12/schema');
    function visit(node) {
      if (!node || typeof node !== 'object') return;
      if (node.$ref) { assert(node.$ref.startsWith('#/$defs/')); assert(schema.$defs[node.$ref.slice(8)],node.$ref); }
      for (const value of Object.values(node)) visit(value);
    }
    visit(schema);
  }
});

test('deterministic defaults for all six points, no calls/storage/model', async () => {
  const inputs = {
    'recipient.routing':{recipient_actor_id:'actor-b',recipient_state:'available'},
    'message.triage':{entry_id:'entry-one'}, 'task.next':{},
    'report.acceptance':{report_entry_id:'report-one'},
    'action.confirmation':confirm().input,
    'turn.output_route':{run_id:'run-one',outcome:'completed',source:source(),explicit_human_actor_id:null,explicit_human_available:false},
  };
  for (const point of POINTS) {
    const req=request(point,inputs[point]); req.input_digest=decisionInputDigest(req); const frozen=JSON.stringify(req);
    const first=await evaluate(req), second=await evaluate(req);
    assert.deepEqual(first,second); assert.equal(JSON.stringify(req),frozen);
    assert.equal(first.provider_id,'example.work-defaults'); assert(currentResult(req,first,current(req)));
  }
  assert.deepEqual((await evaluate(request('report.acceptance',inputs['report.acceptance']))).proposal,{recommendation:'review',report_entry_id:'report-one'});
});

test('agent own-subtree actions allowed; root/cross/accept/reject/resume require confirmation', async () => {
  for (const action of ['work.task.assign.v1','work.entry.post.v1','work.entry.reply.v1','work.report.propose.v1']) {
    assert.equal((await evaluate(confirm(action))).proposal.authorization,'auto_apply');
  }
  const childCreate=confirm('work.task.create.v1',null);childCreate.input.parent_task_id='task-root';
  assert.equal((await evaluate(childCreate)).proposal.authorization,'auto_apply');
  for (const action of ['work.report.accept.v1','work.report.reject.v1','work.resume.v1','work.proposal.confirm.v1','work.decision.override.v1']) {
    assert.equal((await evaluate(confirm(action))).proposal.authorization,'suggest_confirm');
  }
  for (const req of [confirm('work.task.create.v1',null),confirm('work.entry.post.v1','task-other'),confirm('work.entry.post.v1',null)]) {
    assert.equal((await evaluate(req)).proposal.authorization,'suggest_confirm');
  }
  const cross=confirm(); cross.input.cross_subtree=true;
  assert.equal((await evaluate(cross)).proposal.authorization,'suggest_confirm');
  const stale=confirm(); stale.context.actor_current=false;
  assert.equal((await evaluate(stale)).proposal.authorization,'suggest_confirm');
});

test('human explicit current consent differs from role/caller claim and stale binding', async () => {
  const req=confirm('work.report.accept.v1'); req.context.actor_class='human'; req.context.execution_current=false;
  assert.equal((await evaluate(req)).proposal.authorization,'suggest_confirm');
  req.context.human_explicit=true;
  assert.equal((await evaluate(req)).proposal.authorization,'auto_apply');
  req.context.actor_current=false;
  assert.equal((await evaluate(req)).proposal.authorization,'suggest_confirm');
  const forged=confirm(); forged.actor={class:'human'};
  assert.throws(()=>validateFixture(forged));
  const role=confirm(); role.context.role='human';
  assert.throws(()=>validateFixture(role));
});

test('oldest eligible task ordering preserves uint64 string precision and stable tie', async () => {
  const req=request('task.next',{});
  req.context.tasks[0].state='reported';
  req.context.tasks.push(task('task-z','task-root','actor-a','active','9007199254740993'));
  req.context.tasks.push(task('task-a','task-root','actor-a','active','9007199254740992'));
  req.context.tasks[1].state='blocked';
  assert.equal((await evaluate(req)).proposal.task_id,'task-a');
  req.context.execution_current=false;
  assert.equal((await evaluate(req)).proposal.task_id,null);
});

test('registered offline/unknown queues hold exact address; retired/absent never become a name guess', async () => {
  assert.equal((await evaluate(request('recipient.routing',{recipient_actor_id:null,recipient_state:'unregistered'}))).proposal.recipient_actor_id,null);
  for (const state of ['retired','unregistered']) assert.equal((await evaluate(request('recipient.routing',{recipient_actor_id:'actor-b',recipient_state:state}))).proposal.recipient_actor_id,null);
  for (const state of ['available','offline','unknown']) assert.equal((await evaluate(request('recipient.routing',{recipient_actor_id:'actor-b',recipient_state:state}))).proposal.recipient_actor_id,'actor-b');
  assert.deepEqual((await evaluate(request('message.triage',{entry_id:'entry-one'}))).proposal,{priority:'normal',disposition:'queue'});
});

test('thread-final route exact source/run, explicit human separate, abort/ambiguity no guess', async () => {
  const input={run_id:'run-one',outcome:'completed',source:source(),explicit_human_actor_id:null,explicit_human_available:false};
  const req=request('turn.output_route',input);
  const result=await evaluate(req); assert.equal(result.proposal.route,'thread_reply');
  assert.equal(result.proposal.source_entry_version,'2'); assert.equal(result.proposal.recipient_actor_id,'actor-sender');
  for (const mutation of ['run','recipient','none','abort']) {
    const changed=structuredClone(req);
    if (mutation==='run') changed.input.source.run_id='different';
    if (mutation==='recipient') changed.input.source.recipient_actor_id='different';
    if (mutation==='none') changed.input.source=null;
    if (mutation==='abort') changed.input.outcome='aborted';
    assert.equal((await evaluate(changed)).proposal.route,'unrouted');
  }
  req.input.explicit_human_actor_id='actor-human'; req.input.explicit_human_available=true;
  assert.equal((await evaluate(req)).proposal.route,'thread_reply','adapter flag is not human authorization');
  req.context.human_route={authorization_id:'auth-one',recipient_actor_id:'actor-human',run_id:'run-one'};
  const human=await evaluate(req); assert.equal(human.proposal.route,'explicit_human'); assert.equal(human.proposal.thread_id,null);
});

test('stale revisions, policy, wrong provider/generation and cancellation cannot pass result fence', async () => {
  const req=confirm(), result=await evaluate(req);
  for (const key of ['request_id','scope_id','input_revision','input_digest','policy_revision','mode','provider_id','provider_version']) {
    const changed={...result,[key]:'wrong'}; assert(!currentResult(req,changed,current(req)),key);
  }
  assert(!currentResult(req,result,{...current(req),input_revision:'2'}));
  assert(!currentResult(req,result,{...current(req),policy_revision:'2'}));
  assert(!currentResult(req,result,{...current(req),provider_generation:'2'}));
  const signal=AbortSignal.abort();
  assert.equal((await evaluate(req,{signal})).outcome,'cancelled');
  assert(!currentResult(req,result,current(req),{signal}));
});

test('suggest and hard authority cannot be bypassed by a valid current provider answer', async () => {
  const own=confirm(), ownResult=await evaluate(own);
  assert.equal((await applicationPlan(own,ownResult,current(own))).kind,'commit_action');
  const suggest=confirm();suggest.mode='suggest';
  assert.equal((await applicationPlan(suggest,await evaluate(suggest),current(suggest))).kind,'record_suggestion');
  const accept=confirm('work.report.accept.v1');const malicious=await evaluate(accept);
  malicious.proposal.authorization='auto_apply';
  assert(currentResult(accept,malicious,current(accept)),'shape validity is not authority');
  assert.equal((await applicationPlan(accept,malicious,current(accept))).kind,'request_confirmation');
  const controller=new AbortController();
  const pending=applicationPlan(own,ownResult,current(own),{signal:controller.signal});controller.abort();
  assert.equal((await pending).kind,'ignore_stale');
  const malformed={...ownResult,proposal:{authorization:'auto_apply',extra:'forged'}};
  assert(!currentResult(own,malformed,current(own)));
});

test('immutable admitted request and current semantic digest fence same-revision mutation', async () => {
  const req=request('message.triage',{entry_id:'entry-one'});req.input_digest=decisionInputDigest(req);
  const admitted=freezeAdmission(req), result=await evaluateRaw(admitted);
  req.input.entry_id='entry-two';
  assert(!currentResult(req,result,current(req)));
  assert.throws(()=>{admitted.input.entry_id='entry-two';});
  const other=request('message.triage',{entry_id:'entry-two'});other.input_digest=decisionInputDigest(other);
  assert(!currentResult(admitted,result,current(other)));
  assert.equal(canonical({b:2,a:1}),canonical({a:1,b:2}));
});

test('host monotonic budget expiry is cancelled/fenced; provider cannot extend it', async () => {
  const req=confirm(), result=await evaluate(req);
  assert.equal((await evaluate(req,{remainingBudgetMs:()=>0})).outcome,'cancelled');
  assert(!currentResult(req,result,current(req),{remainingBudgetMs:()=>0}));
  await assert.rejects(()=>evaluate(req,{remainingBudgetMs:()=>251}));
});

test('invalid/missing graph, duplicate roots, oversized and numeric revisions reject', () => {
  const cycle=confirm(); cycle.context.tasks[0].parent_id='task-child'; assert.throws(()=>validateFixture(cycle));
  const missing=confirm(); missing.context.tasks[0].parent_id='missing'; assert.throws(()=>validateFixture(missing));
  const fakeRoot=confirm();fakeRoot.context.assigned_roots=['task-other'];assert.throws(()=>validateFixture(fakeRoot));
  const duplicate=confirm();duplicate.context.assigned_roots.push('task-root');assert.throws(()=>validateFixture(duplicate));
  for (const value of [1,'01','18446744073709551616']) { const req=confirm();req.input_revision=value;assert.throws(()=>validateFixture(req)); }
  const big=confirm();big.input.body='x'.repeat(65536);assert.throws(()=>validateFixture(big));
});
