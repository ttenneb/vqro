// Declarative proposed public JSON method shapes. Inert; no registration or effects.
// Core definitions are neutral: no task/thread/work decision ontology.
export const ref = name => ({$ref:`#/$defs/${name}`});
const ID=ref('id'), REV=ref('revision'), NS=ref('namespace'), DIG=ref('digest'), CUR=ref('cursor');
const nullable = schema => ({oneOf:[{type:'null'},schema]});
const enumeration = (...values) => ({enum:values});
const string = maxLength => ({type:'string',maxLength});
const integer = (minimum,maximum) => ({type:'integer',minimum,maximum});
const list = (items,maxItems=128) => ({type:'array',maxItems,items});
const closed = properties => ({type:'object',additionalProperties:false,required:Object.keys(properties),properties});
const opaque = {type:'object'};
const page = closed({after:nullable(REV),limit:integer(1,128)});
const journalKey = {namespace:NS,store_id:ID,store_generation:REV};
const journalCAS = {...journalKey,expected_sequence:REV,operation_id:ID};
const deliveryKey = {namespace:NS,delivery_id:ID};
const claimKey = {...deliveryKey,expected_lookup_domain:ref('lookupDomain'),claim_id:ID,expected_version:REV,operation_id:ID};
const namespaceScope = closed({namespace:NS,capabilities:list(string(128),32)});
const execution = closed({execution_id:ID,epoch:REV});
const savedExport = closed({export_id:ID,head:CUR,sha256:DIG,pins_digest:DIG,writer_fsync_hash_verified:{const:true}});
const lookupDomain=closed({store_id:ID,store_generation:REV});
function missingLookup(key) {
 return {oneOf:[
   closed({state:{const:'absent'},lookup_key:closed(key),lookup_domain:lookupDomain,assurance:{const:'authoritative_absent'}}),
   closed({state:enumeration('unknown','retired','repair_required'),lookup_key:closed(key),lookup_domain:lookupDomain,assurance:{const:'not_authoritative'}}),
 ]};
}

export const definitions = {
 executionRef:execution,
 lookupDomain,
 actionLookup:{oneOf:[ref('admission'),missingLookup({namespace:NS,original_actor_id:ID,operation_id:ID})]},
 deliveryLookup:{oneOf:[ref('deliveryReceipt'),missingLookup(deliveryKey)]},
 actorMetadata:closed({actor_id:ID,subject_kind:enumeration('operator','execution','service'),principal_id:ID,
   execution:nullable(execution),availability:enumeration('available','offline','unknown','retired'),
   assurance:enumeration('user_authorized_binding','service_principal'),consent_revision:REV}),
 admission:closed({lookup_domain:lookupDomain,actor_id:ID,operation_id:ID,input_digest:DIG,namespace:NS,service_id:ID,provider_generation:REV,
   state:enumeration('admitted','committed','denied','conflict','pending','unknown','retired','repair_required'),commit:nullable(CUR),
   result:nullable(opaque),result_assurance:enumeration('host_journal','provider_claim','unknown')}),
 invocation:closed({invocation_id:ID,namespace:NS,service_id:ID,provider_generation:REV,operation_id:ID,budget_ms:integer(1,60000)}),
 journalRecord:closed({cursor:CUR,actor_id:ID,principal_id:ID,operation_id:ID,input_digest:DIG,opaque:nullable(opaque),
   semantic_pin:{type:'boolean'},transport_pins:list(ID,16),deferred_deliveries:list(ref('deliveryIntent'),16)}),
 runReceipt:closed({lookup_domain:lookupDomain,run_id:ID,namespace:NS,delivery_id:ID,version:REV,claim_id:ID,actor_id:ID,execution:execution,
   revision:REV,state:enumeration('bound','start_reported','settled_reported','unknown','retired','repair_required'),
   start_assurance:enumeration('not_started','adapter_reported','unknown')}),
 deliveryReceipt:closed({lookup_domain:lookupDomain,namespace:NS,delivery_id:ID,version:REV,sender_actor_id:ID,recipient_actor_id:ID,
   state:enumeration('waiting','held_offline','held_unknown','claimed','consumed','cancelled','recipient_retired','unknown','retired','repair_required'),
   claim:nullable(ref('claim')),run:nullable(ref('runReceipt')),payload_digest:nullable(DIG)}),
 error:closed({code:enumeration('unsupported','invalid_input','permission_denied','actor_not_found','actor_retired','unverified_subject',
   'expired','replayed','stale_generation','stale_revision','conflict','quota_exceeded','immutable_intent','pinned','backup_required',
   'cancelled','timeout','unknown','retired','repair_required'),message:string(1024),operation_id:nullable(ID),receipt_lookup_required:{type:'boolean'}}),
};
const methods={};
function method(name,capability,params,result,{secrets=[]}={}) {
 methods[name]={capability,params:closed(params),result:closed(result),secrets,
   errors:ref('error'),limits:{request_bytes:65536,result_bytes:65536}};
}
method('runtime.execution.get.v1','runtime.read',{execution_id:nullable(ID),terminal_id:nullable(ID)},
 {server_id:ID,boot_id:ID,execution:nullable(execution),terminal_id:nullable(ID),availability:enumeration('available','offline','unknown','retired'),
   process_birth_verified:{type:'boolean'},session_trust:enumeration('managed','reported','unknown')});
method('runtime.operator.begin.v1','operator.bootstrap',{requested_scopes:list(namespaceScope,32)},
 {challenge_id:ID,challenge_secret:string(512),expires_in_ms:{const:60000}},{secrets:['result.challenge_secret']});
method('runtime.operator.confirm.v1','operator.bootstrap',{challenge_id:ID,challenge_secret:string(512),explicit_consent:{const:true},operation_id:ID},
 {actor:ref('actorMetadata'),credential:string(512),lease_ms:{const:600000},consent_revision:REV},{secrets:['params.challenge_secret','result.credential']});
method('runtime.actor.ticket.v1','actor.authorize',{execution:execution,scopes:list(namespaceScope,32),expected_consent_revision:REV,operation_id:ID},
 {ticket_id:ID,execution:execution,consent_revision:REV,expires_in_ms:{const:60000}});
method('runtime.actor.ticket.take.v1','actor.self.register',{expected_execution:execution},
 {ticket_id:ID,ticket_secret:string(512),expires_in_ms:integer(1,60000)},{secrets:['result.ticket_secret']});
method('runtime.actor.bind.v1','actor.self.register',{ticket_id:ID,ticket_secret:string(512),expected_execution:execution,operation_id:ID},
 {actor:ref('actorMetadata'),credential:string(512),lease_ms:{const:600000}},{secrets:['params.ticket_secret','result.credential']});
method('runtime.actor.credential.recover.v1','actor.self.register',{expected_execution:execution,operation_id:ID},
 {actor:ref('actorMetadata'),credential:string(512),lease_ms:{const:600000}},{secrets:['result.credential']});
method('runtime.actor.renew.v1','actor.self.renew',{actor_id:ID,expected_consent_revision:REV},
 {actor:ref('actorMetadata'),credential:string(512),lease_ms:{const:600000}},{secrets:['result.credential']});
method('runtime.actor.get.v1','actor.read',{actor_id:ID},{actor:ref('actorMetadata')});
method('runtime.actor.checkpoint.get.v1','actor.checkpoint.read',{actor_id:ID,namespace:NS},
 {actor_id:ID,execution:nullable(execution),revision:REV,opaque:nullable(opaque)});
method('runtime.actor.checkpoint.put.v1','actor.checkpoint.write',{actor_id:ID,namespace:NS,expected_revision:REV,operation_id:ID,opaque},
 {actor_id:ID,execution:nullable(execution),revision:REV,input_digest:DIG});
methods['runtime.actor.checkpoint.put.v1'].limits.request_bytes=32768;
method('extension.interfaces.list.v1','extension.interfaces.read',{namespace:nullable(NS)},
 {lookup_domains:list(closed({namespace:NS,action_receipts:lookupDomain,mailbox_receipts:lookupDomain}),128),interfaces:list(closed({namespace:NS,service_id:ID,provider_generation:REV,interface_kind:enumeration('action','query','decision','form','recovery'),
   interface_id:ID,contract_id:string(128),schema_digest:DIG,capability:string(128)}),128)});
method('extension.query.invoke.v1','extension.query.invoke',{namespace:NS,service_id:ID,query:string(128),expected_provider_generation:REV,input:opaque},
 {namespace:NS,service_id:ID,provider_generation:REV,result:opaque});
method('extension.action.invoke.v1','extension.action.invoke',{namespace:NS,service_id:ID,action:string(128),expected_provider_generation:REV,expected_lookup_domain:lookupDomain,operation_id:ID,input:opaque},
 {receipt:ref('admission')});
method('extension.action.receipt.v1','extension.action.receipt',{namespace:NS,original_actor_id:ID,operation_id:ID,expected_lookup_domain:lookupDomain},{receipt:ref('actionLookup')});
method('extension.recovery.invoke.v1','extension.recovery.invoke',{namespace:NS,service_id:ID,expected_provider_generation:REV,operation_id:ID,
   intent_cursor:CUR,manifest_index:integer(0,15)}, {invocation:ref('invocation')});
method('namespace.journal.read.v1','namespace.journal.read',{namespace:NS,store_id:nullable(ID),store_generation:nullable(REV),record_ids:nullable(list(ID)),page},
 {store_id:ID,store_generation:REV,head:REV,records:list(ref('journalRecord')),next:nullable(REV),checkpoint:nullable(closed({head:CUR,chunk_cursors:list(CUR,32),checkpoint_digest:DIG,byte_count:integer(0,1048576)}))});
methods['namespace.journal.read.v1'].limits.result_bytes=524288;
method('namespace.journal.append.v1','namespace.journal.write',{...journalCAS,opaque,deferred_deliveries:list(ref('deferred'),16)},
 {cursor:CUR,input_digest:DIG,actor_id:ID,principal_id:ID,deferred_deliveries:list(ref('deliveryIntent'),16)});
method('namespace.journal.checkpoint.v1','namespace.journal.write',{...journalCAS,chunk_cursors:list(CUR,32),checkpoint_digest:DIG},
 {cursor:CUR,checkpoint_digest:DIG});
// Snapshot chunks are <=32KiB opaque journal payloads; commit validates <=1MiB total/hash.
// No oversized executable-service IPC frame or fictional 1MiB inline opaque response.
method('namespace.journal.pin.v1','namespace.journal.pin',{...journalCAS,record_ids:list(ID),pinned:{type:'boolean'}},
 {head:CUR,pins_digest:DIG});
method('namespace.journal.archive.v1','namespace.journal.archive',{...journalCAS,record_ids:list(ID)},
 {head:CUR,archived_record_ids:list(ID),retained_receipts:{const:true}});
method('namespace.journal.export.v1','namespace.journal.export',{...journalKey,expected_sequence:REV,export_id:nullable(ID),page},
 {export_id:ID,head:CUR,records:list(ref('journalRecord')),next:nullable(REV),complete:{type:'boolean'},sha256:nullable(DIG),pins_digest:DIG,record_count:REV});
methods['namespace.journal.export.v1'].limits.result_bytes=524288;
method('namespace.journal.reset.v1','namespace.journal.reset',{...journalCAS,saved_export:savedExport,explicit_reset_consent:{const:true},
   pinned_disposition:enumeration('none','abandon_all_exported_pins'),reason:string(1024)},
 {old_store_id:ID,old_generation:REV,new_store_id:ID,new_generation:REV,retired_lookup:{const:true}});
method('mailbox.enqueue.v1','mailbox.send',{namespace:NS,delivery_id:ID,recipient_actor_id:ID,expected_lookup_domain:lookupDomain,payload:opaque,operation_id:ID},
 {receipt:ref('deliveryReceipt')});
method('mailbox.intent.recover.v1','mailbox.recovery.send',{namespace:NS,intent_cursor:CUR,manifest_index:integer(0,15),operation_id:ID},
 {receipt:ref('deliveryReceipt')});
method('mailbox.intent.abandon.v1','mailbox.recovery.abandon',{namespace:NS,intent_cursor:CUR,manifest_index:integer(0,15),expected_delivery_version:REV,
   operation_id:ID,explicit_consent:{const:true},reason:string(1024)}, {receipt:ref('deliveryReceipt'),transport_pin_released:{type:'boolean'}});
method('mailbox.list.v1','mailbox.read',{namespace:NS,recipient_actor_id:ID,page},
 {envelopes:list(ref('mailboxEnvelope')),next:nullable(REV)});
methods['mailbox.list.v1'].limits.result_bytes=524288;
method('mailbox.read.v1','mailbox.read',{...deliveryKey,expected_version:REV},{envelope:ref('mailboxEnvelope')});
method('mailbox.edit.v1','mailbox.edit',{...deliveryKey,expected_version:REV,operation_id:ID,payload:opaque},{receipt:ref('deliveryReceipt')});
method('mailbox.claim.v1','mailbox.claim',{...deliveryKey,expected_lookup_domain:lookupDomain,expected_version:REV,operation_id:ID},{claim:ref('claim')});
method('mailbox.claim.renew.v1','mailbox.claim',{...claimKey},{claim:ref('claim')});
method('mailbox.ack.v1','mailbox.consume',{...claimKey,disposition:enumeration('consumed','released')},{receipt:ref('deliveryReceipt')});
method('mailbox.receipt.v1','mailbox.receipt',{...deliveryKey,expected_lookup_domain:lookupDomain},{receipt:ref('deliveryLookup')});
method('mailbox.run.bind.v1','mailbox.run.bind',{...claimKey},{run:ref('runReceipt')});
method('mailbox.run.start.ack.v1','mailbox.run.ack',{namespace:NS,run_id:ID,expected_lookup_domain:lookupDomain,expected_run_revision:REV,operation_id:ID}, {run:ref('runReceipt')});
method('mailbox.run.receipt.v1','mailbox.run.read',{namespace:NS,run_id:ID,expected_lookup_domain:lookupDomain},{run:{oneOf:[ref('runReceipt'),missingLookup({namespace:NS,run_id:ID})]}});

export const METHODS=Object.freeze(methods);
export function apiSchema(neutralDefinitions) {
 const defs={...neutralDefinitions,...definitions};
 const requestVariants=[],responseVariants=[];
 for (const [name,shape] of Object.entries(METHODS)) {
   const request=closed({id:ID,method:{const:name},params:shape.params});
   const response=closed({id:ID,method:{const:name},result:nullable(shape.result),error:nullable(ref('error'))});
   response.oneOf=[{properties:{result:shape.result,error:{type:'null'}}},{properties:{result:{type:'null'},error:ref('error')}}];
   const key=name.replaceAll('.','_');defs[`${key}_request`]=request;defs[`${key}_response`]=response;
   requestVariants.push(ref(`${key}_request`));responseVariants.push(ref(`${key}_response`));
 }
 return {$schema:'https://json-schema.org/draft/2020-12/schema',$id:'urn:vqro:neutral-method-shapes:v1',
   title:'Unavailable candidate new method requests/responses; not gen1 fixture replacement',
   description:'Actor/caller/invocation authorization lives in transport, never params. Errors feature-local. Byte/scalar/permission/graph/CAS constraints are normative semantic checks.',
   oneOf:[{oneOf:requestVariants},{oneOf:responseVariants}],$defs:defs};
}
