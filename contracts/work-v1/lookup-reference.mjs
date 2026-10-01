// Inert lookup fence. No storage access; authoritative_absent requires production verification.
export function retryEligibleAbsent(receipt, expectedKey, expectedDomain) {
  if (!receipt || Object.keys(receipt).sort().join(',') !== 'assurance,lookup_domain,lookup_key,state') return false;
  if (receipt.state !== 'absent' || receipt.assurance !== 'authoritative_absent') return false;
  const same = (a,b) => a && b && Object.keys(a).sort().join(',') === Object.keys(b).sort().join(',') && Object.keys(a).every(key=>a[key]===b[key]);
  return same(receipt.lookup_key,expectedKey) && same(receipt.lookup_domain,expectedDomain);
}
