import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createHash} from 'node:crypto';
const dir=new URL('./',import.meta.url);
const load=name=>JSON.parse(fs.readFileSync(new URL(name,dir),'utf8'));
const schema=load('proposal.schema.json');
const digest=name=>createHash('sha256').update(fs.readFileSync(new URL(name,dir))).digest('hex');

test('proposal preview uses a separately named closed contract, not frozen Work read extension',()=>{
  assert.equal(digest('work.schema.json'),'525e915cfba2c7f00a9d260dbf35a17f6e3a0975dcd28241654bedca4fb87b15');
  assert.equal(digest('api.schema.json'),'2be89b06eb7a0eae91418253da5b63450266cfa5b3daebaf9b4727df1f60564f');
  assert.equal(schema.$defs.request.properties.contract.const,'vqro.work.proposal.read.request.v1');
  assert.equal(schema.$defs.result.properties.contract.const,'vqro.work.proposal.read.result.v1');
  assert.deepEqual(schema.$defs.request.required,['contract','proposal_id','after_cursor','limit']);
  assert.equal(schema.$defs.request.additionalProperties,false);
  for (const key of ['actor_id','class','grant','operator','chosen_action']) assert(!schema.$defs.request.properties[key]);
  assert.equal(schema.$defs.request.allOf[0].then.properties.after_cursor.type,'null');
});

test('preview exposes immutable choice plus diagnostic preconditions and bounded generation-fenced paging',()=>{
  const proposal=schema.$defs.proposal,result=schema.$defs.result;
  for(const key of ['proposal_id','input_revision','proposer_actor_id','decision_id','chosen_action','current_preconditions']) assert(proposal.required.includes(key));
  assert.equal(proposal.additionalProperties,false);
  assert.equal(result.properties.proposals.maxItems,128);
  for(const key of ['store_id','store_generation','snapshot_revision','next_cursor']) assert(result.required.includes(key));
  assert.deepEqual(proposal.properties.chosen_action.allOf[1].properties.action.not.enum,['work.proposal.confirm.v1','work.decision.override.v1']);
  assert.match(schema.description,/operator\/read grant/);
  assert.match(schema.description,/64KiB/);
});

test('new references resolve by registered schema URI, not local filename aliases',()=>{
  const work=load('work.schema.json');
  const registry=new Map([[schema.$id,schema],[work.$id,work]]);
  assert.equal(work.$id,'urn:vqro:work-contract:v1');
  assert(!registry.has(new URL('work.schema.json',schema.$id).href));
  function walk(value) {
    if (!value || typeof value!=='object') return;
    if(value.$ref) {
      const absolute=new URL(value.$ref,schema.$id).href;
      const [resource,fragment]=absolute.split('#');
      let current=registry.get(resource);
      assert(current,`unregistered schema URI ${resource}`);
      for(const token of (fragment??'').split('/').filter(Boolean)) current=current?.[token.replaceAll('~1','/').replaceAll('~0','~')];
      assert(current,value.$ref);
    }
    for(const child of Object.values(value)) walk(child);
  }
  walk(schema);
});
// These are shape/reference checks, not a full Draft2020 validator or host-authority test.
