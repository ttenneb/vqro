// Inert semantic example. Arguments named verified* must come from host/canonical joins,
// not client or adapter flags. No effects, auth implementation, transport or persistence.
import assert from 'node:assert/strict';

export function authorizeRoute(params, verifiedCaller, current) {
  assert.equal(verifiedCaller.subject_kind,'operator','human operator consent required');
  assert(verifiedCaller.current && verifiedCaller.explicit_consent);
  assert.equal(params.expected_route_revision,current.route_revision,'route CAS conflict');
  assert(!current.reply_committed,'output already committed');
  for (const field of ['task_id','source_entry_id','source_entry_version','thread_id','execution_id','execution_epoch']) assert.equal(params[field],current[field],field);
  if (params.run_id === null) assert.equal(current.run_id,null,'choose existing exact run');
  else assert.equal(params.run_id,current.run_id,'wrong run');
  assert(['explicit_human','thread_reply','cancel'].includes(params.route));
  if (params.route === 'explicit_human') {
    assert.equal(params.recipient_actor_id,current.recipient_actor_id,'wrong verified recipient');
    assert.equal(current.recipient_subject_kind,'operator','not a verified human recipient');
    assert(['available','offline','unknown'].includes(current.recipient_availability),'retired/unregistered recipient');
  } else assert.equal(params.recipient_actor_id,null);
  return {...params,authorization_id:current.next_authorization_id,author_actor_id:verifiedCaller.actor_id,
    route_revision:String(BigInt(current.route_revision)+1n),state:params.route==='cancel'?'cancelled':'authorized'};
}

export function boundHumanRoute(grant, verifiedRun, currentRouteRevision) {
  if (grant.state!=='authorized' || grant.route!=='explicit_human' || grant.route_revision!==currentRouteRevision) return null;
  for (const field of ['task_id','source_entry_id','source_entry_version','thread_id','execution_id','execution_epoch']) if (grant[field]!==verifiedRun[field]) return null;
  // V1 host permits one logical run per exact delivery/version; failed start is never
  // permission for a new model attempt/run under this consent. Recovery needs new source.
  if (!verifiedRun.sole_run_for_source || verifiedRun.cancelled || verifiedRun.reply_committed) return null;
  if (grant.run_id!==null && grant.run_id!==verifiedRun.run_id) return null;
  return {authorization_id:grant.authorization_id,recipient_actor_id:grant.recipient_actor_id,run_id:verifiedRun.run_id};
}
