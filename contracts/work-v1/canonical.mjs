// Candidate integer-JSON canonical form, vqro-json-int-v1. Not a wire decoder.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';

export function canonical(value) {
  if (value === null || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number') {
    assert(Number.isSafeInteger(value) && !Object.is(value,-0),'integer JSON only; counters use strings');
    return String(value);
  }
  if (typeof value === 'string') {
    // Reject lone UTF-16 surrogates; valid surrogate pairs encode one Unicode scalar.
    assert.equal(value.toWellFormed(),value,'invalid Unicode scalar');
    return JSON.stringify(value);
  }
  assert(value && typeof value === 'object','unsupported JSON value');
  if (Array.isArray(value)) return '[' + value.map(canonical).join(',') + ']';
  assert(Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null,'plain JSON objects only');
  const keys = Object.keys(value).sort((a,b)=>Buffer.compare(Buffer.from(a,'utf8'),Buffer.from(b,'utf8')));
  return '{' + keys.map(key=>canonical(key)+':'+canonical(value[key])).join(',') + '}';
}
export function decisionInputDigest(request) {
  const selected = {};
  for (const field of ['contract','point','scope_id','input_revision','policy_revision','mode','budget_ms','context','input']) selected[field]=request[field];
  return createHash('sha256').update('vqro.decision.input.v1\0','utf8').update(canonical(selected),'utf8').digest('hex');
}
export function freezeAdmission(request) {
  const admitted=JSON.parse(canonical(request));
  assert.equal(admitted.input_digest,decisionInputDigest(admitted),'input digest does not match admitted semantic data');
  function freeze(value) { if (value && typeof value === 'object') {Object.values(value).forEach(freeze);Object.freeze(value);} return value; }
  return freeze(admitted);
}
