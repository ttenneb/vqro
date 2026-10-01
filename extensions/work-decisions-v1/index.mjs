// Public, no-model decision composition. This does not admit actors or commit Work actions.
// The injected Work broker is the sole source of trusted context and current CAS facts.
import assert from 'node:assert/strict';
import { performance } from 'node:perf_hooks';
import { freezeAdmission, decisionInputDigest, canonical } from '../../contracts/work-v1/canonical.mjs';
import { evaluate as deterministic, validateRequest, POINTS, PROVIDER } from '../../contracts/work-v1/decision-defaults.mjs';

const MAX_ACTIVE = 32;
const MAX_PER_PROVIDER = 4;
const MAX_PROVIDERS = 32;
const MAX_PROVIDER_IDENTITIES = 128;
const MAX_BROKER_OUTSTANDING = 32;
const STOPPED_BEFORE_EVALUATION = Symbol('stopped before evaluation');
const RESULT_FIELDS = ['contract','request_id','point','scope_id','input_revision','input_digest','policy_revision','provider_id','provider_version','provider_generation','mode','outcome','proposal','rationale_code','explanation','evidence_ids'];
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const REV = /^(0|[1-9][0-9]*)$/;
function exact(value, keys) {
  assert(value && typeof value === 'object' && !Array.isArray(value));
  assert.deepEqual(Object.keys(value).sort(), [...keys].sort());
}
function id(value) { assert(typeof value === 'string' && ID.test(value)); }
function revision(value) { assert(typeof value === 'string' && value.length <= 20 && REV.test(value) && BigInt(value) <= 18446744073709551615n); }
function nullable(value, check) { if (value !== null) check(value); }
function validateProposal(point, p) {
  switch (point) {
    case 'recipient.routing': exact(p,['recipient_actor_id','delivery']); nullable(p.recipient_actor_id,id); assert.equal(p.delivery,'queue'); break;
    case 'message.triage': exact(p,['priority','disposition']); assert(['low','normal','high'].includes(p.priority)); assert.equal(p.disposition,'queue'); break;
    case 'task.next': exact(p,['task_id']); nullable(p.task_id,id); break;
    case 'report.acceptance': exact(p,['recommendation','report_entry_id']); assert(['review','accept','reject'].includes(p.recommendation)); id(p.report_entry_id); break;
    case 'action.confirmation': exact(p,['authorization']); assert(['auto_apply','suggest_confirm'].includes(p.authorization)); break;
    case 'turn.output_route':
      exact(p,['route','recipient_actor_id','thread_id','source_entry_id','source_entry_version']);
      assert(['thread_reply','explicit_human','unrouted'].includes(p.route));
      nullable(p.recipient_actor_id,id); nullable(p.thread_id,id); nullable(p.source_entry_id,id); nullable(p.source_entry_version,revision);
      if (p.route === 'thread_reply') { id(p.recipient_actor_id); id(p.thread_id); id(p.source_entry_id); revision(p.source_entry_version); }
      if (p.route === 'explicit_human') { id(p.recipient_actor_id); assert.equal(p.thread_id,null); assert.equal(p.source_entry_id,null); assert.equal(p.source_entry_version,null); }
      if (p.route === 'unrouted') for (const field of ['recipient_actor_id','thread_id','source_entry_id','source_entry_version']) assert.equal(p[field],null);
      break;
    default: assert.fail('unsupported decision point');
  }
}
function immutableJson(value) {
  const copy = JSON.parse(canonical(value));
  function freeze(node) { if (node && typeof node === 'object') { Object.values(node).forEach(freeze); Object.freeze(node); } return node; }
  return freeze(copy);
}
function validateResult(request, result, provider) {
  exact(result,RESULT_FIELDS);
  assert.equal(result.contract,'vqro.decision.result.v1');
  assert.equal(result.outcome,'proposal');
  for (const field of ['request_id','point','scope_id','input_revision','input_digest','policy_revision','mode']) assert.equal(result[field],request[field]);
  assert.equal(result.provider_id,provider.id); assert.equal(result.provider_version,provider.version); assert.equal(result.provider_generation,provider.generation);
  validateProposal(request.point,result.proposal);
  assert(typeof result.rationale_code === 'string' && Buffer.byteLength(result.rationale_code) <= 128);
  assert(typeof result.explanation === 'string' && Buffer.byteLength(result.explanation) <= 1024);
  assert(Array.isArray(result.evidence_ids) && result.evidence_ids.length <= 16);
  for (const value of result.evidence_ids) id(value);
  assert.equal(new Set(result.evidence_ids).size,result.evidence_ids.length);
  assert(Buffer.byteLength(JSON.stringify(result)) <= 65536);
}
function validPolicy(request, result, baseline) {
  const p = result.proposal, b = baseline.proposal;
  switch (request.point) {
    case 'recipient.routing': return p.recipient_actor_id === b.recipient_actor_id;
    case 'message.triage': return p.disposition === 'queue';
    case 'task.next': return p.task_id === null || (request.context.actor_current && request.context.execution_current &&
      request.context.tasks.some(t => t.id === p.task_id && t.state === 'active' && t.assigned_actor_id === request.context.actor_id &&
        inAssignedSubtree(t.id,request.context)));
    case 'report.acceptance': return p.recommendation === 'review' && p.report_entry_id === request.input.report_entry_id;
    case 'action.confirmation': return p.authorization !== 'auto_apply' || b.authorization === 'auto_apply';
    case 'turn.output_route': return canonical(p) === canonical(b); // exact Work-verified source/run/human route, not adapter hints
    default: return false;
  }
}
function inAssignedSubtree(taskId, context) {
  const tasks = new Map(context.tasks.map(t => [t.id,t]));
  let current = tasks.get(taskId);
  while (current) { if (context.assigned_roots.includes(current.id)) return true; current = tasks.get(current.parent_id); }
  return false;
}
function consistentHumanRoute(request) {
  if (request.point !== 'turn.output_route' || request.context.human_route === null) return true;
  const {source,run_id,outcome} = request.input;
  return request.context.actor_current && request.context.execution_current && outcome === 'completed' &&
    source !== null && source.run_id === run_id && source.recipient_actor_id === request.context.actor_id &&
    request.context.human_route.run_id === run_id;
}
function choosePlan(request, result) {
  if (request.mode === 'suggest') return 'record_suggestion';
  if (request.point === 'report.acceptance') return 'record_suggestion';
  if (request.point === 'action.confirmation') return result.proposal.authorization === 'auto_apply' ? 'commit_action' : 'request_confirmation';
  // A route selects a target, never grants posting authority. Work must run its
  // independent action-confirmation/subtree check and exact source/run CAS.
  return 'proposal_only';
}
function currentFence(admission, current) {
  const req = admission.request;
  return current && current.store_id === admission.store_id && current.store_generation === admission.store_generation &&
    current.actor_id === admission.actor_id && current.scope_id === req.scope_id &&
    current.actor_current === true && current.execution_current === req.context.execution_current &&
    (req.context.actor_class !== 'agent' || current.execution_current === true) && current.source_current === true &&
    current.input_revision === req.input_revision && current.input_digest === req.input_digest &&
    current.policy_revision === req.policy_revision;
}
function metadata(provider) {
  exact(provider,['id','version','generation','points','capability','backend','evaluate']);
  id(provider.id); id(provider.capability); revision(provider.generation);
  assert(typeof provider.version === 'string' && provider.version.length > 0 && provider.version.length <= 64);
  assert(['component','process','deterministic'].includes(provider.backend));
  assert(Array.isArray(provider.points) && provider.points.length > 0 && new Set(provider.points).size === provider.points.length);
  assert(provider.points.every(point => POINTS.includes(point)));
  assert.equal(typeof provider.evaluate,'function');
  return Object.freeze({id:provider.id,version:provider.version,generation:provider.generation,points:Object.freeze([...provider.points]),capability:provider.capability,backend:provider.backend,evaluate:provider.evaluate});
}

/** broker.admit(input) must build immutable Work+host context, never trust client-supplied actor/source/route.
 * broker.current(admission) must resnapshot Work and host lease facts. The returned plan is not a commit receipt.
 * Work must repeat these fences at journal CAS, include provenance in that same CAS and audit even fallback.
 */
export function createDecisionRegistry({broker, now = () => performance.now(), schedule = (fn, ms) => setTimeout(fn,ms)}) {
  assert.equal(typeof broker.admit,'function'); assert.equal(typeof broker.current,'function');
  assert.equal(typeof broker.authorizeProvider,'function');
  const providers = new Map(), running = new Map(), outstanding = new Map(), generations = new Map();
  const brokerOutstanding = new Set();
  let active = 0;
  async function boundedBroker(call, remainingBudgetMs, signal) {
    if (signal?.aborted) return {status:'cancelled'};
    if (remainingBudgetMs() <= 0) return {status:'deadline'};
    if (brokerOutstanding.size >= MAX_BROKER_OUTSTANDING) return {status:'busy'};
    const controller = new AbortController();
    const onAbort = () => controller.abort();
    signal?.addEventListener('abort',onAbort,{once:true});
    if (signal?.aborted) controller.abort();
    let timer;
    try {
      const task = Promise.resolve().then(() => {
        if (controller.signal.aborted) throw new Error('cancelled before broker call');
        return call({signal:controller.signal,remainingBudgetMs});
      }).then(value => ({status:'ok',value}),() => ({status:controller.signal.aborted ? 'cancelled' : 'error'}));
      brokerOutstanding.add(task);
      // An uncooperative host adapter retains its bounded slot until it settles.
      void task.then(() => brokerOutstanding.delete(task));
      const stopped = new Promise(resolve => controller.signal.addEventListener('abort',
        () => resolve({status:signal?.aborted ? 'cancelled' : 'deadline'}),{once:true}));
      const expired = new Promise(resolve => {timer=schedule(() => {controller.abort();resolve({status:'deadline'});},
        remainingBudgetMs(),'broker');});
      const outcome = await Promise.race([task,stopped,expired]);
      if (signal?.aborted) return {status:'cancelled'};
      if (remainingBudgetMs() <= 0) return {status:'deadline'};
      return outcome;
    } finally {
      if (timer !== undefined) clearTimeout(timer);
      signal?.removeEventListener('abort',onAbort);
    }
  }
  function register(raw) {
    const provider = metadata(raw);
    assert(provider.id !== PROVIDER.id,'reserved deterministic provider');
    assert(!providers.has(provider.id),'provider already registered; revoke before replacing generation');
    assert(providers.size < MAX_PROVIDERS && (generations.has(provider.id) || generations.size < MAX_PROVIDER_IDENTITIES),
      'registry quota exceeded');
    assert(!generations.has(provider.id) || BigInt(provider.generation) > BigInt(generations.get(provider.id)),
      'generation must increase after revoke');
    generations.set(provider.id,provider.generation);
    providers.set(provider.id,provider);
    return provider;
  }
  function revoke(providerId) {
    providers.delete(providerId);
    for (const controller of running.get(providerId) ?? []) controller.abort();
  }
  // Work may publish these descriptors through the separately advertised public
  // extension.interfaces.list.v1 method, filtered by the caller's actual grants.
  // Discovery itself never grants the ability to invoke a provider.
  function interfaces() {
    const builtIn = {...PROVIDER,points:POINTS,capability:'work.decision.evaluate.v1',backend:'deterministic'};
    return immutableJson([builtIn,...providers.values()].map(provider => ({provider_id:provider.id,
      provider_version:provider.version,provider_generation:provider.generation,
      capability:provider.capability,backend:provider.backend,
      points:provider.points.map(point => ({point,request_contract:'vqro.decision.request.v1',
        result_contract:'vqro.decision.result.v1'}))})));
  }
  async function resolve(input, {providerId = null, signal} = {}) {
    // The broker, not the caller, constructs the request, admission generation and scope facts.
    if (providerId !== null) id(providerId);
    const admitted = await broker.admit(input);
    const started = now(); // Work/host admission occurred; validation consumes this same budget.
    const request = freezeAdmission(admitted.request);
    validateRequest(request);
    assert.equal(request.input_digest,decisionInputDigest(request));
    id(admitted.actor_id); id(admitted.store_id); revision(admitted.store_generation);
    const admission = Object.freeze({request,actor_id:admitted.actor_id,store_id:admitted.store_id,store_generation:admitted.store_generation});
    assert.equal(request.context.actor_id,admission.actor_id);
    const provider = providerId === null ? null : providers.get(providerId);
    const selected = provider && provider.points.includes(request.point) ? provider : null;
    const deadline = started + request.budget_ms;
    const controller = new AbortController();
    if (selected && !running.has(selected.id)) running.set(selected.id,new Set());
    if (selected && !outstanding.has(selected.id)) outstanding.set(selected.id,new Set());
    if (selected) running.get(selected.id).add(controller);
    const onAbort = () => controller.abort();
    signal?.addEventListener('abort',onAbort,{once:true});
    if (signal?.aborted) controller.abort();
    const remainingBudgetMs = () => Math.max(0,Math.min(request.budget_ms,deadline - now()));
    let timer, deadlineFired = false, cause = null, result, fenceFailure = null;
    try {
      if (controller.signal.aborted) cause = 'cancelled';
      else if (remainingBudgetMs() <= 0) cause = 'deadline';
      else if (!consistentHumanRoute(request)) cause = 'invalid_source';
      else if (providerId !== null && !selected) cause = 'provider_unavailable';
      else if (selected) {
        const grant = await boundedBroker(options => broker.authorizeProvider(admission,selected,options),remainingBudgetMs,controller.signal);
        if (grant.status !== 'ok') cause = grant.status === 'error' ? 'provider_unavailable' :
          grant.status === 'cancelled' && !signal?.aborted ? 'revoked_or_cancelled' : grant.status;
        else if (!grant.value || providers.get(selected.id) !== selected) cause = 'provider_unavailable';
      }
      if (!cause && selected && providers.get(selected.id) !== selected) cause = 'provider_unavailable';
      if (!cause && selected && (active >= MAX_ACTIVE || outstanding.get(selected.id).size >= MAX_PER_PROVIDER)) cause = 'provider_busy';
      if (!cause && selected) {
        active++;
        // Host timeout/revocation/cancellation settles even when provider ignores AbortSignal.
        const stopped = new Promise(resolve => controller.signal.addEventListener('abort',() => resolve({stopped:true}),{once:true}));
        const expired = new Promise(resolve => { timer = schedule(() => {deadlineFired=true;controller.abort(); resolve({timeout:true});},remainingBudgetMs()); });
        const invocation = Promise.resolve().then(() => {
          if (controller.signal.aborted) return STOPPED_BEFORE_EVALUATION;
          return selected.evaluate(request,{signal:controller.signal,remainingBudgetMs});
        }).then(value => value === STOPPED_BEFORE_EVALUATION ? {stopped:true} : {value},error => ({error}));
        outstanding.get(selected.id).add(invocation);
        // A provider that ignores abort retains its bounded slot until it
        // actually settles; a timeout cannot accumulate unbounded hung calls.
        void invocation.then(() => { outstanding.get(selected.id)?.delete(invocation); active--; });
        const candidate = await Promise.race([invocation,expired,stopped]);
        if (deadlineFired || remainingBudgetMs() <= 0 || candidate.timeout) cause = 'deadline';
        else if (controller.signal.aborted || candidate.stopped || providers.get(selected.id) !== selected) cause = 'revoked_or_cancelled';
        else if (candidate.error) cause = 'provider_error';
        else {
          try {
            const safe = immutableJson(candidate.value);
            validateResult(request,safe,selected);
            const baseline = await deterministic(request,{remainingBudgetMs:() => request.budget_ms});
            if (!validPolicy(request,safe,baseline)) throw new Error('hard policy');
            result = safe;
          } catch { cause = 'invalid_proposal'; }
        }
      }
      if (!result && cause !== 'cancelled' && cause !== 'revoked_or_cancelled' &&
          cause !== 'invalid_source' && !signal?.aborted) {
        // A timed-out invocation has no remaining authority: record the deterministic
        // fallback proposal for review but never apply it after its monotonic deadline.
        const fallback = await deterministic(request,{remainingBudgetMs:() => request.budget_ms});
        result = immutableJson({...fallback,outcome:cause ? 'fallback' : 'proposal'});
      }
      async function revalidate() {
        if (signal?.aborted || controller.signal.aborted || remainingBudgetMs() <= 0) return false;
        const snapshot = await boundedBroker(options => broker.current(admission,options),remainingBudgetMs,controller.signal);
        if (snapshot.status !== 'ok') { fenceFailure = snapshot.status; return false; }
        if (!currentFence(admission,snapshot.value)) { fenceFailure = 'stale_current'; return false; }
        if (selected && result?.provider_id === selected.id) {
          if (providers.get(selected.id) !== selected || snapshot.value.provider_generation !== selected.generation) {
            fenceFailure = 'provider_generation_changed'; return false;
          }
          const grant = await boundedBroker(options => broker.authorizeProvider(admission,selected,options),remainingBudgetMs,controller.signal);
          if (grant.status !== 'ok' || !grant.value) { fenceFailure = grant.status === 'ok' ? 'provider_revoked' : grant.status; return false; }
        }
        return !signal?.aborted && !controller.signal.aborted && remainingBudgetMs() > 0;
      }
      const fresh = await revalidate();
      const terminalCause = ['deadline','cancelled','busy'].includes(cause);
      const kind = fresh && result && !terminalCause ? choosePlan(request,result) : 'no_effect';
      const finalCause = cause ?? (kind === 'no_effect' ?
        fenceFailure ?? (signal?.aborted ? 'cancelled' : remainingBudgetMs() <= 0 ? 'deadline' : 'stale_current') : null);
      // Provenance MUST travel with any Work plan. Work persists it atomically with effect/outbox.
      const provenance = immutableJson({point:request.point,request_id:request.request_id,scope_id:request.scope_id,
        provider_id:result?.provider_id ?? selected?.id ?? providerId,
        provider_version:result?.provider_version ?? selected?.version ?? null,
        provider_generation:result?.provider_generation ?? selected?.generation ?? null,
        mode:request.mode,input_revision:request.input_revision,input_digest:request.input_digest,
        policy_revision:request.policy_revision,proposal:result?.proposal ?? null,
        rationale_code:result?.rationale_code ?? null,explanation:result?.explanation ?? '',
        evidence_ids:result?.evidence_ids ?? [],
        outcome:kind === 'no_effect' ? 'no_effect' : result.outcome, fallback_cause:finalCause,
        attempted_provider_id:selected?.id ?? providerId,attempted_provider_generation:selected?.generation ?? null,
        actor_id:admission.actor_id,store_id:admission.store_id,store_generation:admission.store_generation});
      return Object.freeze({request,result,
        plan:Object.freeze({kind,proposal:kind === 'no_effect' ? null : result.proposal}),provenance,
        // Work must call immediately before its CAS, then repeat state and host
        // invocation lease/deadline checks atomically inside that commit path.
        revalidate});
    } finally {
      if (timer !== undefined) clearTimeout(timer);
      signal?.removeEventListener('abort',onAbort);
      if (selected) running.get(selected.id)?.delete(controller);
    }
  }
  return Object.freeze({register,revoke,interfaces,resolve});
}
