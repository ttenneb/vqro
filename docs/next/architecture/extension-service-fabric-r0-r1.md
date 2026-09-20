# Extension Service Fabric R0/R1

Status: accepted implementation architecture

## 1. Purpose

The Extension Service Fabric lets one independently installed sandboxed package call one explicitly selected service exported by another independently installed sandboxed package. It is a neutral composition mechanism, not a coordination ontology and not a replacement for Herdr.

R0 establishes exact contract identity and durable user-approved bindings. R1 adds one bounded read-only request/response call. The design preserves package isolation, durable PTYs, generation-1 endpoint compatibility, and feature-specific Collections and Spaces authority.

The keywords MUST, MUST NOT, SHOULD, and MAY are normative.

## 2. Non-goals

R0/R1 do not provide:

- events, subscriptions, streams, fan-out, load balancing, or remote providers;
- nested calls, recursion, re-entrancy, provider host calls, or capability inheritance;
- package-defined widgets, document composition, or publication into host-owned namespaces;
- host-side semantic merging of package documents;
- automatic provider selection by install order, load order, package name, or contract similarity;
- compatibility ranges or structural subtyping;
- generalized effects or a stable public SDK;
- a Herdr package. Herdr remains a backend behind later neutral contracts.

Collections and Spaces retain their exact audited authority paths. A generic export MUST NOT replace either feature pin.

## 3. Terms

- **Contract descriptor**: bounded canonical JSON describing one exact contract, its methods, schemas, information categories, and byte limits.
- **Contract identity**: `(contract_id, contract_version, descriptor_sha256)`. Equality is exact equality of all three values.
- **Export**: a package service's named endpoint implementing one contract identity.
- **Import**: a package service's named slot requiring or optionally consuming one contract identity.
- **Service binding**: durable host-owned approval connecting one exact consumer artifact/import to one exact provider artifact/export.
- **Effective status**: a derived status such as `ready`, `suspended`, or `revoked`; it is not provider-authored health.
- **Invocation lease**: a short-lived host-minted authorization for one call, bound to the durable binding revision, exact artifacts, current package and worker generations, deadline, and cancellation flag.

Discovery and installation never grant authority. Only an effective durable binding can mint an invocation lease.

## 4. Manifest v3 and exact contracts

### 4.1 Versioning

Manifest v2 is frozen. Its `provides` strings retain their existing meaning and MUST NOT be interpreted as service-fabric exports. A v2 package has no fabric exports or imports.

Manifest v3 is selected only when `manifest_version = 3`. It uses a distinct DTO and schema. A v3 file MUST NOT be deserialized as v2 with optional fields. Unknown fields fail closed. An old Vqro rejects manifest v3 because the manifest version is unsupported.

The archive contract remains `vqro.package.v1`; changing the manifest language does not change the deterministic `.vqrox` container contract.

### 4.2 Declarations

Each v3 service has the existing service identity, runtime, and host-capability list plus:

```toml
[[services.exports]]
id = "echo"
descriptor = "contracts/example.echo.v1.json"
descriptor_sha256 = "<64 lowercase hex>"

[[services.imports]]
id = "echo-source"
descriptor = "contracts/example.echo.v1.json"
descriptor_sha256 = "<same digest>"
required = true
```

Export IDs and import IDs are package-service-local stable identifiers. They use the existing bounded package identifier grammar and are unique within the service. One import binds to zero or one export. One export may serve multiple bindings.

`required = true` means the importing service is unavailable for fabric-enabled host invocation while the slot is unbound or suspended. It does not prevent installation of the consumer before its provider and does not cause eager startup ordering.

Host capabilities remain ambient authority granted by package installation. Imports and exports MUST NOT infer, add, or transfer host capabilities.

### 4.3 Descriptor

The initial descriptor is `vqro.service-contract.v1` canonical JSON:

```json
{
  "contract":"vqro.service-contract.v1",
  "contract_id":"example.echo",
  "contract_version":"1.0.0",
  "methods":[{
    "id":"echo",
    "request_schema":{},
    "response_schema":{},
    "request_information":["user.supplied.text"],
    "response_information":["provider.generated.text"],
    "max_request_bytes":4096,
    "max_response_bytes":4096
  }]
}
```

Descriptors are data, not executable code. They are bounded by host constants for total bytes, nesting, method count, identifier length, information-category count, and request/response sizes.

The host parses the descriptor into a closed DTO, rejects duplicate or unsorted-equivalent entries, serializes it with the host canonical-JSON algorithm, and hashes those canonical bytes. Objects are ordered lexicographically by UTF-8 key bytes; arrays retain order; numbers use the accepted JSON integer representation; duplicate object keys, floats, and non-canonical values are rejected. The computed digest MUST equal the declaration's lowercase SHA-256.

The descriptor's contract ID and version are display and diagnostic fields. The descriptor digest is mandatory and prevents two packages from claiming compatibility using only a free-form name. Provider and consumer declarations match only on the exact contract identity.

R1 structurally validates envelopes, method membership, JSON encoding, and byte bounds. It does not claim universal semantic validation of domain payloads. The schemas and information categories are exact reviewed claims made by the two artifacts; future schema enforcement can tighten behavior without relaxing identity.

## 5. Information-flow authority

A binding approval shows, and durably records:

- exact consumer and provider package IDs, versions, package SHA-256 values, service IDs, and import/export IDs;
- exact contract identity;
- the allowed method subset;
- for each allowed method, request information categories flowing consumer → provider and response categories flowing provider → consumer;
- request and response byte limits;
- the complete consent-document SHA-256.

R1 approves the descriptor's exact method set; method subsets are reserved for a later additive binding version. Information categories are namespaced bounded identifiers. They are disclosure labels, not claims that the host can infer the semantic content of arbitrary JSON. Enforcement is achieved by exact artifacts, exact schemas, exact methods, direction, and byte bounds. Revocation prevents future disclosure but cannot erase information already delivered.

A provider receives only the request envelope and payload. It receives no consumer host capabilities, host namespace access, PTY access, state snapshot, credential, or nested service-call authority.

## 6. Durable ServiceBinding

### 6.1 State schema

`extensions.json` advances to state schema 2 and contains a service-binding registry alongside package state. State schema 1 migrates under the existing state lock with an empty binding registry. Migration MUST NOT fabricate bindings from v2 `provides` strings. An old binary encountering schema 2 fails closed and leaves the file untouched.

R1 approval admits only enabled exact-Git component packages. Curated discovery, developer links, file imports, legacy processes, and manifest-v2 `provides` grant no Service Fabric eligibility or authority.

A binding record contains:

- stable `binding_id` derived from the logical consumer package/service/import tuple;
- monotonically increasing binding revision and registry revision;
- exact consumer endpoint plus package and manifest SHA-256 values;
- exact provider endpoint plus package and manifest SHA-256 values;
- exact contract identity and the approved information-flow summary;
- consent digest;
- state `approved` or `revoked`.

Bindings are artifact-bound deliberately. Updating, rolling back, reinstalling with different bytes, disabling, removing, or failing to start either package makes the binding `suspended`; it never silently follows a compatible-looking logical endpoint. Replacing a consumer or provider requires a new explicit approval and revision. Restoring the identical previously approved artifact may make a non-revoked binding effective again.

Because package pointer changes do not rewrite bindings, package activation and binding authority cannot split: exact artifact mismatch fails closed before a lease is minted. The existing package activation journal remains authoritative for package replacement. A binding mutation is a single locked atomic replacement of schema-2 `extensions.json`. A future logical-binding design would require a graph-aware activation journal; R0 does not introduce one.

### 6.2 Status and lifecycle

Durable records store consent, not runtime health. Effective status is derived:

- `ready`: approved, both exact artifacts are active and enabled, declarations still match, and both current service workers are admitted;
- `suspended`: approval remains but any exact-artifact, declaration, enablement, or runtime condition is absent;
- `revoked`: the durable record was explicitly revoked.

No fallback provider is selected. Reorder does not affect a binding. Package failure affects only its bindings and package-owned semantics; generic workspace/tab/pane operation and PTY recovery remain available.

Approval, replacement, and revocation use host API methods added to endpoint generation 1. They are additive methods, not reinterpretations of existing methods. Every mutation carries an expected registry/binding revision and explicit confirmation. Inspection is read-only and computes the exact consent document before mutation.

## 7. R1 call protocol

### 7.1 Consumer request

A consumer may issue one `service.call` host request while handling a host-started outer invocation. Before starting that consumer invocation, the App actor resolves the exact approved edge and gives the consumer worker an immutable, revocable lease containing the already-running provider worker's bounded sender. The host call identifies only its declared import ID, method ID, and JSON payload. The runtime supplies and validates the consumer identity; package-supplied package IDs, provider IDs, artifact hashes, binding revisions, depths, or generations are not trusted.

The runtime router enforces:

- depth exactly 1;
- one outstanding host call per consumer invocation;
- one service call total per outer invocation;
- a declared import and exact approved binding;
- method membership and request byte bound;
- a live deadline and cancellation flag.

A provider invocation is recorded as the second hop in host tracing and runs with **no host-call router**. Any attempted provider host call is rejected. A consumer invoked as a provider also receives no router, which makes recursion and re-entrancy impossible in R1.

### 7.2 Off-App-actor execution

Runtime execution and waiting MUST NOT occur on the App actor.

1. In one nonblocking turn before the outer consumer invocation, the App actor validates the binding and generations, clones the provider worker's bounded endpoint, and mints an immutable revocable lease. It then releases all registry, catalog, candidate-set, and state-file locks.
2. The consumer worker reaches `service.call`. Its synchronous handler validates the lease and uses `try_send` to place one distinct service-read request on the provider's existing bounded worker with a one-shot response channel. A full or disconnected queue fails immediately.
3. The consumer worker waits only until the outer deadline, cancellation, retirement, or revocation. The App actor is not involved and remains free to process PTY and lifecycle events.
4. The provider worker revalidates its side of the lease and runs the provider with no host calls. It validates the bounded response and replies directly through the one-shot channel. Nested provider completion does not consume `AppEvent` capacity.
5. The consumer handler revalidates cancellation, deadline, revocation, and both generations after receipt before returning to the consumer component.

Wake-ups are proportional to actual calls and completions, never panes, views, frames, clients, or installed-but-idle packages. Nested consumer-to-provider relay traffic never enters the App event channel. A host-started outer API invocation uses one existing candidate-completion event (plus a timeout backstop) to settle its deferred client response, while the App actor performs no runtime wait.

### 7.3 Lease and stale completion fencing

The lease binds at least:

- binding ID and revision;
- binding-registry revision;
- exact consumer and provider package/manifest SHA-256 values;
- consumer and provider artifact generations;
- consumer and provider worker/runtime generations and retirement tokens;
- import ID, export ID, contract digest, and method;
- outer request identity, absolute deadline, and shared cancellation flag.

Before dispatch and before delivery, all fields that can change are revalidated. Package lifecycle revision numbers are not independently copied into the lease: exact artifact pointers plus per-worker retirement tokens are the stronger live fence, and every replacement, disable, removal, or worker restart retires those tokens before a newer worker is admitted. Revocation, timeout, or cancellation likewise marks the shared token. A stale completion is dropped and returns a closed error; it MUST NOT be delivered to a newer consumer.

### 7.4 Bounds and errors

The host applies fixed bounds to descriptor bytes, bindings, pending calls, queue depth, JSON request/response bytes, invocation wall time, component fuel, and component memory. Provider saturation fails busy; it does not create an unbounded queue. Timeout/cancellation marks the provider work stale even if Wasmtime completes later.

Stable R1 error classes are:

- `service_binding_unavailable`;
- `service_binding_revoked`;
- `service_method_denied`;
- `service_request_invalid`;
- `service_call_busy`;
- `service_call_timeout`;
- `service_call_canceled`;
- `stale_service_call_lease`;
- `service_provider_failed`;
- `service_response_invalid`.

Package diagnostics may be logged, but responses do not expose host paths, credentials, raw traps, or another package's private runtime state.

## 8. Recovery and compatibility

On startup the host:

1. recovers any existing package activation journal using existing exact package rules;
2. migrates state schema 1 to schema 2 under the state lock if needed;
3. validates the complete binding registry structurally and against installed artifacts;
4. starts package candidates without fabric authority;
5. derives effective binding status only after exact workers are admitted;
6. publishes a fresh binding generation in one App turn.

No invocation lease survives restart or live handoff. Durable approvals survive when exact artifacts do. Pending calls fail closed. Generic terminal identities and PTYs are not part of binding state and remain recoverable.

Generation-1 endpoint framing and existing methods remain unchanged. New clients can inspect fabric status; old clients continue to use all pre-fabric methods. `host.document.v2`, `HostDocumentV1`, Collections, Spaces, and private protocol 23 are unchanged.

## 9. Acceptance gates

Evidence may be compositional across persistence, registry, runtime, App/API, and existing PTY-recovery tests, but at least one integration test MUST drive the ordinary API through inspect → approve → deferred consumer → provider → completion with two real independently packaged components. Lower-level race tests remain required where deterministic orchestration is not possible through a socket test.

R0 is complete only when tests prove:

- strict independent parsing of manifests v2 and v3;
- deterministic descriptor identity and rejection of mismatches, duplicate JSON keys, malformed schemas, unknown fields, and bounds violations;
- state-1 → state-2 migration with no fabricated bindings and old-reader fail-closed behavior;
- explicit approve, inspect, replace, suspend, revoke, restart, and identical-artifact reactivation behavior;
- package update/reorder/disable/remove cannot silently rebind or preserve a stale lease;
- consent changes whenever artifacts, declarations, methods, information labels, schemas, bounds, or endpoints change.

R1 is complete only when two independently packaged component fixtures prove:

- one consumer calls one provider through one approved exact binding;
- no binding, wrong artifact, wrong descriptor, wrong method, or revoked approval fails closed;
- provider host calls, self-call, nesting, recursion, and re-entrancy are impossible;
- queue saturation, timeout, cancellation, trap, oversized request/response, and malformed response are bounded;
- provider replacement, consumer replacement, revocation, and worker restart fence late completions;
- the App actor remains responsive while the provider is slow;
- unrelated packages and existing PTYs remain usable;
- restart preserves approval but creates fresh runtime leases.

Full lint and test suites remain release gates. The platform status document MUST describe the fabric as experimental until the production path, not merely DTOs or a test harness, passes these gates.

## 10. Later milestones

R2 may add events with independent sequence, backpressure, gap, cancellation, and resnapshot contracts. R3 may add a package-owned compositor that consumes exact provider snapshots and owns one output namespace. Neither may weaken R0/R1 binding identity or overload request/response calls with event semantics.
