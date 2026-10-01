// Inert Slice2A reference. No host broker, transport, storage, model or UI authority.
import assert from 'node:assert/strict';
import { decisionInputDigest } from './canonical.mjs';

export const PROVIDER = Object.freeze({ id: 'example.work-defaults', version: '1.0.0', generation: '1' });
export const POINTS = Object.freeze(['recipient.routing', 'message.triage', 'task.next', 'report.acceptance', 'action.confirmation', 'turn.output_route']);
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const REV = /^(0|[1-9][0-9]*)$/;
const MAX_REV = 18446744073709551615n;
const REV_KEYS = ['input_revision', 'policy_revision'];
const FIELD_NAMES = ['contract','request_id','point','scope_id','input_revision','input_digest','policy_revision','mode','budget_ms','context','input'];

function exact(object, fields) {
  assert(object && typeof object === 'object' && !Array.isArray(object));
  assert.deepEqual(Object.keys(object).sort(), [...fields].sort(), 'unexpected or missing field');
}
function id(value) { assert(typeof value === 'string' && ID.test(value), 'invalid opaque ID'); }
function revision(value) { assert(typeof value === 'string' && value.length <= 20 && REV.test(value) && BigInt(value) <= MAX_REV, 'invalid revision'); }

// This is a reference semantic check, not a general JSON Schema implementation.
export function validateRequest(request) {
  exact(request, FIELD_NAMES);
  assert.equal(request.contract, 'vqro.decision.request.v1');
  for (const key of ['request_id','scope_id']) id(request[key]);
  for (const key of REV_KEYS) revision(request[key]);
  assert.match(request.input_digest, /^[a-f0-9]{64}$/);
  assert.equal(request.input_digest,decisionInputDigest(request),'semantic input changed without matching digest');
  assert(POINTS.includes(request.point));
  assert(['suggest','auto_apply'].includes(request.mode));
  assert(Number.isInteger(request.budget_ms) && request.budget_ms > 0 && request.budget_ms <= 5000);
  assert(Buffer.byteLength(JSON.stringify(request)) <= 65536, 'request byte quota');
  const context = request.context;
  exact(context, ['actor_id','actor_class','execution_current','human_explicit','assigned_roots','tasks','actor_current','human_route']);
  id(context.actor_id);
  assert(['human','agent','service'].includes(context.actor_class));
  assert.equal(typeof context.execution_current, 'boolean');
  assert.equal(typeof context.actor_current, 'boolean');
  assert.equal(typeof context.human_explicit, 'boolean');
  if (context.human_route !== null) {
    exact(context.human_route,['authorization_id','recipient_actor_id','run_id']);
    Object.values(context.human_route).forEach(id);
  }
  assert(Array.isArray(context.tasks) && context.tasks.length <= 256);
  assert(Array.isArray(context.assigned_roots) && context.assigned_roots.length <= 256);
  const tasks = new Map();
  for (const task of context.tasks) {
    exact(task, ['id','parent_id','assigned_actor_id','state','assignment_sequence']);
    id(task.id); if (task.parent_id !== null) id(task.parent_id);
    if (task.assigned_actor_id !== null) id(task.assigned_actor_id);
    assert(['active','blocked','reported','accepted','rejected','archived'].includes(task.state));
    revision(task.assignment_sequence);
    assert(!tasks.has(task.id), 'duplicate task'); tasks.set(task.id, task);
  }
  // Bounded reference graph validation; not on any runtime render/delivery path.
  for (const task of tasks.values()) {
    const seen = new Set(); let current = task;
    while (current) {
      assert(!seen.has(current.id), 'task cycle'); seen.add(current.id);
      if (current.parent_id === null) break;
      assert(tasks.has(current.parent_id), 'missing task parent'); current = tasks.get(current.parent_id);
    }
  }
  const roots = new Set();
  for (const root of context.assigned_roots) {
    id(root); assert(!roots.has(root), 'duplicate assigned root'); roots.add(root);
    assert.equal(tasks.get(root)?.assigned_actor_id, context.actor_id, 'root not assigned to actor');
  }
  return tasks;
}

function ownSubtree(taskId, context, tasks) {
  if (!taskId || !context.actor_current || context.actor_class === 'service' || (context.actor_class === 'agent' && !context.execution_current)) return false;
  const roots = new Set(context.assigned_roots);
  let current = tasks.get(taskId);
  while (current) {
    if (roots.has(current.id)) return true;
    current = tasks.get(current.parent_id);
  }
  return false;
}

export async function evaluate(request, { signal, remainingBudgetMs = () => request.budget_ms } = {}) {
  const tasks = validateRequest(request);
  assert.equal(typeof remainingBudgetMs,'function');
  const remaining = remainingBudgetMs();
  assert(Number.isFinite(remaining) && remaining >= 0 && remaining <= request.budget_ms, 'invalid host remaining budget');
  const context = request.context, input = request.input;
  let proposal, rationale_code;
  switch (request.point) {
    case 'recipient.routing':
      exact(input, ['recipient_actor_id','recipient_state']);
      if (input.recipient_actor_id !== null) id(input.recipient_actor_id);
      assert(['available','offline','unknown','retired','unregistered'].includes(input.recipient_state));
      proposal = { recipient_actor_id: ['available','offline','unknown'].includes(input.recipient_state) ? input.recipient_actor_id : null, delivery: 'queue' };
      rationale_code = proposal.recipient_actor_id ? 'explicit_recipient' : 'recipient_required'; break;
    case 'message.triage':
      exact(input, ['entry_id']); id(input.entry_id);
      proposal = { priority: 'normal', disposition: 'queue' }; rationale_code = 'queue_default'; break;
    case 'task.next': {
      exact(input, []);
      const candidates = [...tasks.values()].filter(task => task.state === 'active' && task.assigned_actor_id === context.actor_id && ownSubtree(task.id, context, tasks));
      candidates.sort((a,b) => BigInt(a.assignment_sequence) < BigInt(b.assignment_sequence) ? -1 : BigInt(a.assignment_sequence) > BigInt(b.assignment_sequence) ? 1 : a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
      proposal = { task_id: candidates[0]?.id ?? null }; rationale_code = proposal.task_id ? 'oldest_eligible' : 'no_eligible_task'; break;
    }
    case 'report.acceptance':
      exact(input, ['report_entry_id']); id(input.report_entry_id);
      proposal = { recommendation: 'review', report_entry_id: input.report_entry_id }; rationale_code = 'human_review_required'; break;
    case 'action.confirmation': {
      exact(input, ['action','task_id','parent_task_id','cross_subtree']);
      if (input.task_id !== null) id(input.task_id); if (input.parent_task_id !== null) id(input.parent_task_id);
      assert.equal(typeof input.cross_subtree, 'boolean');
      const actionNames = ['work.task.create.v1','work.task.assign.v1','work.entry.post.v1','work.entry.reply.v1','work.report.propose.v1','work.report.accept.v1','work.report.reject.v1','work.resume.v1','work.output.route.v1','work.proposal.confirm.v1','work.decision.override.v1'];
      assert(actionNames.includes(input.action));
      const agentActions = actionNames.slice(0,5);
      const allowedAgent = context.actor_class === 'agent' && agentActions.includes(input.action) && !input.cross_subtree && ownSubtree(input.action === 'work.task.create.v1' ? input.parent_task_id : input.task_id, context, tasks);
      const allowedHuman = context.actor_class === 'human' && context.actor_current && context.human_explicit;
      proposal = { authorization: allowedAgent || allowedHuman ? 'auto_apply' : 'suggest_confirm' };
      rationale_code = allowedHuman ? 'human_explicit' : allowedAgent ? 'agent_own_subtree' : 'confirmation_required'; break;
    }
    case 'turn.output_route': {
      exact(input, ['run_id','outcome','source','explicit_human_actor_id','explicit_human_available']);
      id(input.run_id); assert(['completed','aborted','error'].includes(input.outcome));
      if (input.explicit_human_actor_id !== null) id(input.explicit_human_actor_id);
      assert.equal(typeof input.explicit_human_available, 'boolean');
      const empty = { route:'unrouted', recipient_actor_id:null, thread_id:null, source_entry_id:null, source_entry_version:null };
      proposal = empty; rationale_code = 'unrouted_output';
      if (!context.actor_current || !context.execution_current || input.outcome !== 'completed') break;
      if (input.explicit_human_actor_id && input.explicit_human_available && context.human_route?.recipient_actor_id === input.explicit_human_actor_id && context.human_route?.run_id === input.run_id) {
        proposal = { ...empty, route:'explicit_human', recipient_actor_id:input.explicit_human_actor_id }; rationale_code = 'explicit_human_route'; break;
      }
      if (input.source !== null) {
        const source = input.source;
        exact(source, ['entry_id','entry_version','thread_id','sender_actor_id','recipient_actor_id','run_id']);
        for (const key of ['entry_id','thread_id','sender_actor_id','recipient_actor_id','run_id']) id(source[key]); revision(source.entry_version);
        if (source.recipient_actor_id === context.actor_id && source.run_id === input.run_id) {
          proposal = { route:'thread_reply', recipient_actor_id:source.sender_actor_id, thread_id:source.thread_id, source_entry_id:source.entry_id, source_entry_version:source.entry_version };
          rationale_code = 'source_thread_reply';
        }
      }
      break;
    }
  }
  const cancelled = signal?.aborted || remainingBudgetMs() <= 0;
  return { contract:'vqro.decision.result.v1', request_id:request.request_id, point:request.point,
    scope_id:request.scope_id, input_revision:request.input_revision, input_digest:request.input_digest,
    policy_revision:request.policy_revision, provider_id:PROVIDER.id, provider_version:PROVIDER.version,
    provider_generation:PROVIDER.generation, mode:request.mode, outcome:cancelled ? 'cancelled' : 'proposal',
    proposal, rationale_code:cancelled ? (signal?.aborted ? 'cancelled' : 'budget_expired') : rationale_code, explanation:'', evidence_ids:[] };
}

function validProposal(point, proposal) {
  switch (point) {
    case 'recipient.routing': exact(proposal,['recipient_actor_id','delivery']); if (proposal.recipient_actor_id !== null) id(proposal.recipient_actor_id); assert.equal(proposal.delivery,'queue'); break;
    case 'message.triage': exact(proposal,['priority','disposition']); assert(['low','normal','high'].includes(proposal.priority)); assert.equal(proposal.disposition,'queue'); break;
    case 'task.next': exact(proposal,['task_id']); if (proposal.task_id !== null) id(proposal.task_id); break;
    case 'report.acceptance': exact(proposal,['recommendation','report_entry_id']); assert(['review','accept','reject'].includes(proposal.recommendation)); id(proposal.report_entry_id); break;
    case 'action.confirmation': exact(proposal,['authorization']); assert(['auto_apply','suggest_confirm'].includes(proposal.authorization)); break;
    case 'turn.output_route':
      exact(proposal,['route','recipient_actor_id','thread_id','source_entry_id','source_entry_version']);
      assert(['thread_reply','explicit_human','unrouted'].includes(proposal.route));
      for (const key of ['recipient_actor_id','thread_id','source_entry_id']) if (proposal[key] !== null) id(proposal[key]);
      if (proposal.source_entry_version !== null) revision(proposal.source_entry_version);
      if (proposal.route === 'thread_reply') { for (const key of ['recipient_actor_id','thread_id','source_entry_id']) id(proposal[key]); revision(proposal.source_entry_version); }
      if (proposal.route === 'explicit_human') { id(proposal.recipient_actor_id); for (const key of ['thread_id','source_entry_id','source_entry_version']) assert.equal(proposal[key],null); }
      if (proposal.route === 'unrouted') for (const key of ['recipient_actor_id','thread_id','source_entry_id','source_entry_version']) assert.equal(proposal[key],null);
      break;
    default: assert.fail('unsupported point');
  }
}

export async function applicationPlan(request, result, current, options = {}) {
  if (!currentResult(request,result,current,options)) return {kind:'ignore_stale'};
  // A validated response never bypasses hard V1 authority or the caller's suggest mode.
  if (request.mode === 'suggest' || request.point === 'report.acceptance') return {kind:'record_suggestion'};
  if (request.point === 'action.confirmation') {
    const hardDefault = await evaluate(request,options);
    if (hardDefault.outcome !== 'proposal' || !currentResult(request,result,current,options)) return {kind:'ignore_stale'};
    return {kind:hardDefault.proposal.authorization === 'auto_apply' && result.proposal.authorization === 'auto_apply' ? 'commit_action' : 'request_confirmation'};
  }
  return {kind:'proposal_only'};
}

// Pure result fence, NOT an authority/action applier. Production must independently
// enforce approved policy, exact source/target/run/claim, actor lease and journal CAS.
export function currentResult(request, result, current, { signal, remainingBudgetMs } = {}) {
  try {
    validateRequest(request);
    exact(result,['contract','request_id','point','scope_id','input_revision','input_digest','policy_revision','provider_id','provider_version','provider_generation','mode','outcome','proposal','rationale_code','explanation','evidence_ids']);
    assert.equal(result.contract,'vqro.decision.result.v1'); validProposal(result.point,result.proposal);
    assert(typeof result.rationale_code === 'string' && result.rationale_code.length <= 128);
    assert(typeof result.explanation === 'string' && Buffer.byteLength(result.explanation) <= 1024);
    assert(Array.isArray(result.evidence_ids) && result.evidence_ids.length <= 16 && new Set(result.evidence_ids).size === result.evidence_ids.length);
    result.evidence_ids.forEach(id);
  } catch { return false; }
  return !signal?.aborted && (!remainingBudgetMs || remainingBudgetMs() > 0) && result.outcome === 'proposal' && current.input_revision === request.input_revision
    && current.input_digest === request.input_digest && current.policy_revision === request.policy_revision && current.provider_generation === result.provider_generation
    && ['request_id','point','scope_id','input_revision','input_digest','policy_revision','mode'].every(key => result[key] === request[key])
    && result.provider_id === PROVIDER.id && result.provider_version === PROVIDER.version;
}
