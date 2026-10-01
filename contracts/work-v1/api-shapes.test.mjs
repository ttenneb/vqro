import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {METHODS,apiSchema,definitions} from './api-shapes.mjs';
import {retryEligibleAbsent} from './lookup-reference.mjs';
const read=name=>JSON.parse(fs.readFileSync(new URL(name,import.meta.url),'utf8'));

test('every proposed neutral method has bounded request/result/error shape and capability',()=>{
  const doc=fs.readFileSync(new URL('../../docs/next/api/v1-neutral-mechanics.md',import.meta.url),'utf8').split('### Immutable caller context')[0];
  const documented=new Set([...doc.matchAll(/`((?:runtime|extension|namespace|mailbox)\.[a-z.]+\.v1)`/g)].map(match=>match[1]));
  assert.deepEqual([...documented].sort(),Object.keys(METHODS).sort());
  for (const [method,shape] of Object.entries(METHODS)) {
    assert(shape.capability,method);assert.equal(shape.params.additionalProperties,false,method);
    assert.equal(shape.result.additionalProperties,false,method);
    assert(shape.limits.request_bytes<=65536 && shape.limits.result_bytes<=524288,method);
    assert.equal(shape.errors.$ref,'#/$defs/error');
  }
});

test('generated API candidate deterministic, neutral and internal refs resolved',()=>{
  const schema=read('api.schema.json');assert.deepEqual(schema,apiSchema(read('broker.schema.json').$defs));
  function visit(node) {
    if (!node || typeof node!=='object') return;
    if (node.$ref) {assert(node.$ref.startsWith('#/$defs/'));assert(schema.$defs[node.$ref.slice(8)],node.$ref);}
    if (node.properties) for (const key of Object.keys(node.properties)) assert(!['task_id','thread_id','sidebar','work_state','acceptance'].includes(key),key);
    Object.values(node).forEach(visit);
  }
  visit(schema);
});

test('absent lookup uses only known key/domain, never fabricated sender/provider attribution',()=>{
  const domain={store_id:'receipts-one',store_generation:'1'};
  for (const [definition,key] of [['actionLookup',{namespace:'example.work',original_actor_id:'actor-a',operation_id:'op-one'}],['deliveryLookup',{namespace:'example.work',delivery_id:'delivery-one'}]]) {
    const missing=definitions[definition].oneOf[1].oneOf[0];
    const fixture={state:'absent',lookup_key:key,lookup_domain:domain,assurance:'authoritative_absent'};
    assert.deepEqual(Object.keys(fixture).sort(),missing.required.toSorted());
    assert(retryEligibleAbsent(fixture,key,domain));
    assert(!retryEligibleAbsent({...fixture,state:'unknown',assurance:'not_authoritative'},key,domain));
    assert(!retryEligibleAbsent({...fixture,state:'retired',assurance:'not_authoritative'},key,domain));
    assert(!retryEligibleAbsent(fixture,key,{...domain,store_generation:'2'}));
    assert(!retryEligibleAbsent({...fixture,sender_actor_id:'fabricated'},key,domain));
    assert(!Object.hasOwn(missing.properties,'input_digest'));
    assert(!Object.hasOwn(missing.properties,'sender_actor_id'));
  }
  const workMissing=read('work.schema.json').$defs.receiptMissing;
  const workFixture={state:'absent',lookup_key:{namespace:'example.work',original_actor_id:'actor-a',operation_id:'op-one'},lookup_domain:domain,assurance:'authoritative_absent'};
  assert.deepEqual(Object.keys(workFixture).sort(),workMissing.required.toSorted());
  assert(retryEligibleAbsent(workFixture,workFixture.lookup_key,domain));
  assert(!Object.hasOwn(workMissing.properties,'input_digest'));
  assert(!definitions.deliveryReceipt.properties.state.enum.includes('absent'));
  assert.equal(METHODS['extension.action.receipt.v1'].result.properties.receipt.$ref,'#/$defs/actionLookup');
});

test('claim/run bindings and ACK/lookup carry expected mailbox generation; old domain cannot pass absent fence',()=>{
  for (const method of ['mailbox.claim.v1','mailbox.claim.renew.v1','mailbox.ack.v1','mailbox.run.bind.v1','mailbox.run.start.ack.v1','mailbox.run.receipt.v1']) {
    assert(METHODS[method].params.required.includes('expected_lookup_domain'),method);
  }
  assert(definitions.runReceipt.required.includes('lookup_domain'));
  assert(read('broker.schema.json').$defs.claim.required.includes('lookup_domain'));
  const old={store_id:'mail-one',store_generation:'1'}, next={store_id:'mail-two',store_generation:'2'};
  const key={namespace:'example.work',run_id:'run-old'};
  assert(!retryEligibleAbsent({state:'retired',lookup_key:key,lookup_domain:old,assurance:'not_authoritative'},key,next));
});

test('actor-facing queries use separate bounded provider result and Work read/receipt contracts',()=>{
  const query=METHODS['extension.query.invoke.v1'];
  assert.equal(query.capability,'extension.query.invoke');
  assert(query.params.required.includes('expected_provider_generation'));
  assert(!query.params.required.includes('operation_id'));
  assert.deepEqual(query.result.required.toSorted(),['namespace','provider_generation','result','service_id']);
  const work=read('work.schema.json').$defs;
  assert(work.readResult.required.includes('route_authorizations'));
  assert.equal(work.readResult.properties.next_cursor.oneOf[1].$ref,'#/$defs/readCursor');
  assert(work.readCursor.required.includes('store_generation'));
  const page={contract:'vqro.work.read.result.v1',store_id:'store-one',store_generation:'1',snapshot_revision:'1',next_cursor:null,tasks:[],threads:[],entries:[],route_authorizations:[]};
  assert.deepEqual(Object.keys(page).sort(),work.readResult.required.toSorted());
});

test('raw transport invocation cannot assert caller/actor/class; receipt assures source',()=>{
  const params=METHODS['extension.action.invoke.v1'].params;
  for (const field of ['actor_id','caller','actor_class','human_explicit']) assert(!Object.hasOwn(params.properties,field));
  assert(!Object.hasOwn(METHODS['runtime.actor.ticket.v1'].result.properties,'ticket_secret'));
  assert.deepEqual(METHODS['runtime.actor.ticket.take.v1'].secrets,['result.ticket_secret']);
  assert(METHODS['mailbox.run.receipt.v1']);assert(METHODS['mailbox.intent.recover.v1']);
});
