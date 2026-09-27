# Extension Health R1 product API

Status: implemented host and CLI seam; independently published product packages and production pins remain unavailable.

The accepted product architecture is
[`extension-health-r1-product.md`](../architecture/extension-health-r1-product.md). The host exposes
one additive generation-1 method:

```json
{"id":"doctor-1","method":"extension.doctor","params":{}}
```

The method accepts no caller-selected package, provider, binding, method, or payload. In one bounded
App turn it resolves only package `vqro.extension-doctor`, service `doctor`, import `health-policy`,
and that import's exact ready R1 binding. It derives a fixed-size
`vqro.extension-health-observation.v1` from authoritative package, admitted-service, and binding
state. Service totals come from the validated manifest of every retained active artifact in the
package registry, independent of candidate admission: disabled, failed, activating, and unavailable
packages still contribute their declared component services. A declared service is ready only when
the exact active artifact slot and its service runtime generation exist; otherwise it is unavailable.
A record with no active artifact contributes zero service declarations. If any retained active
artifact is missing, corrupt, or inconsistent, capture fails closed instead of omitting it. The
observation includes both package and manifest SHA-256 values for the consumer and provider; it
contains no credentials, extension payloads, repository or host paths, traps, logs, or raw errors.

The host then uses the existing deferred R1 relay with consumer method `doctor`. Its internal lease
adds a product-only request constraint containing provider method `evaluate` and the SHA-256 of the
host-canonical observation JSON. The direct relay canonicalizes the consumer-supplied payload and
requires both values to match before provider enqueue. A mismatch is
`service_request_invalid`, consumes the one-call lease, and never runs Health; a valid Doctor result
is accepted only after exactly one matching call. Generic R1 invocations have no request constraint.
The App actor does not wait for either component, the provider receives no host-call router, and the
existing revocation, deadline, generation, and stale-completion fences remain authoritative.

Only a validated `vqro.extension-doctor-diagnostic.v1` returns the new success result:

```json
{
  "id": "doctor-1",
  "result": {
    "type": "extension_doctor_diagnostic",
    "diagnostic": {
      "contract": "vqro.extension-doctor-diagnostic.v1",
      "status": "healthy",
      "findings": []
    }
  }
}
```

Every missing, suspended, revoked, busy, timed-out, canceled, stale, failed, or invalid composition
returns the ordinary generation-1 error envelope with a stable R1 code. There is no product-specific
success-shaped unavailable value. Clients must branch on `error.code`.

The command is:

```text
vqro extension doctor
vqro extension doctor --json
```

Without `--json`, the CLI alone renders the stable human diagnostic or unavailable message. With
`--json`, success prints the inner diagnostic and failure prints the ordinary API error envelope.
Exit status is 0 for healthy, 1 for a valid degraded or unhealthy diagnostic, and 2 when no valid
diagnostic is available.

The checked-in DTOs, exact package/service/import constants, and canonical
[`vqro-extension-health-contract-v1.json`](vqro-extension-health-contract-v1.json) descriptor are
local package integration seams. A digest test canonicalizes that descriptor to the accepted
`23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0` identity. These seams do not
install, approve, select, or publish a provider. No production pin is
present. Creation/publication of the two external repositories, writing production pins, and exact-
Git installation remain blocked on separate accountable-user authorization.
