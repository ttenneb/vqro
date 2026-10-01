import test from 'node:test';
import assert from 'node:assert/strict';
import {authorizeRoute,boundHumanRoute} from './output-route-reference.mjs';
const source={task_id:'task-one',source_entry_id:'entry-one',source_entry_version:'1',thread_id:'thread-one',execution_id:'exec-one',execution_epoch:'1'};
const params=()=>({...source,task_id:'task-one',run_id:null,expected_route_revision:'0',route:'explicit_human',recipient_actor_id:'human-one',reason:'show me output'});
const caller=()=>({subject_kind:'operator',current:true,explicit_consent:true,actor_id:'human-one'});
const current=()=>({...source,route_revision:'0',reply_committed:false,run_id:null,recipient_subject_kind:'operator',recipient_actor_id:'human-one',recipient_availability:'available',next_authorization_id:'auth-one'});
const run=()=>({...source,run_id:'run-one',sole_run_for_source:true,cancelled:false,reply_committed:false});

test('public human route before run binds exact first run, not an adapter flag',()=>{
  const grant=authorizeRoute(params(),caller(),current());
  assert.equal(grant.run_id,null);
  assert.deepEqual(boundHumanRoute(grant,run(),'1'),{authorization_id:'auth-one',recipient_actor_id:'human-one',run_id:'run-one'});
  assert.throws(()=>authorizeRoute(params(),{...caller(),subject_kind:'execution'},current()));
  assert.throws(()=>authorizeRoute(params(),{...caller(),explicit_consent:false},current()));
});
test('route revision/cancel/replaced execution/wrong source cannot redirect settled output',()=>{
  const grant=authorizeRoute(params(),caller(),current());
  for (const change of [{execution_epoch:'2'},{execution_id:'exec-two'},{source_entry_version:'2'},{cancelled:true},{sole_run_for_source:false},{reply_committed:true}]) {
    assert.equal(boundHumanRoute(grant,{...run(),...change},'1'),null);
  }
  assert.equal(boundHumanRoute(grant,run(),'2'),null);
  const cancel=authorizeRoute({...params(),run_id:'run-one',route:'cancel',recipient_actor_id:null,expected_route_revision:'1'},caller(),{...current(),route_revision:'1',run_id:'run-one'});
  assert.equal(boundHumanRoute(cancel,run(),'2'),null);
  assert.throws(()=>authorizeRoute(params(),caller(),{...current(),reply_committed:true}));
  assert.throws(()=>authorizeRoute(params(),caller(),{...current(),route_revision:'1'}));
});
test('existing run requires exact ID; pre-run form cannot silently choose already running work',()=>{
  assert.throws(()=>authorizeRoute(params(),caller(),{...current(),run_id:'run-one'}));
  const grant=authorizeRoute({...params(),run_id:'run-one'},caller(),{...current(),run_id:'run-one'});
  assert.equal(boundHumanRoute(grant,{...run(),run_id:'different'},'1'),null);
  assert.throws(()=>authorizeRoute(params(),caller(),{...current(),recipient_subject_kind:'execution'}));
  assert.throws(()=>authorizeRoute({...params(),recipient_actor_id:'attacker-human'},caller(),current()));
  assert.throws(()=>authorizeRoute({...params(),task_id:'wrong-task'},caller(),current()));
  assert.throws(()=>authorizeRoute(params(),caller(),{...current(),recipient_availability:'retired'}));
  assert.throws(()=>authorizeRoute({...params(),route:'thread_reply'},caller(),current()));
});
