# Extension Health R1 product composition

Status: accepted

## 1. Decision and scope

Extension Health is the first ordinary product composition on the frozen
[Extension Service Fabric R0/R1](extension-service-fabric-r0-r1.md). It proves a useful
host → consumer → provider path without adding another authority mechanism:

1. `vqro extension doctor` asks the host for one diagnostic.
2. In one App turn, the host derives a complete `vqro.extension-health-observation.v1`
   value from authoritative extension package, service, and binding state.
3. The independently packaged Doctor consumer receives that value as its complete outer
   invocation parameters and makes exactly one approved R1 call to its required Health import.
4. The independently packaged Health provider validates the complete observation and returns
   normalized bounded findings. It receives no host-call router.
5. Doctor validates the findings and owns the stable JSON diagnostic and human rendering.

This is a product use of the existing one-call read relay. It does not change manifest v3,
contract identity, binding persistence, invocation leases, R1 error classes, or endpoint
generation 1. It does not add events, subscriptions, nested calls, provider host calls,
automatic provider choice, package capability inheritance, or a public SDK.

The keywords MUST, MUST NOT, SHOULD, and MAY are normative.

## 2. Exact packages and artifacts

Doctor and the default Health policy are built and installed independently. They MUST remain
separate deterministic archives from separate repositories; a combined archive or a component
copied into the host repository does not satisfy this decision.

| Role | Package / namespace | Repository | Archive | Service | Component |
| --- | --- | --- | --- | --- | --- |
| Consumer | `vqro.extension-doctor` | `https://github.com/ttenneb/vqro-extension-doctor.git` | `dist/vqro-extension-doctor.vqrox` | `doctor` | `services/doctor.wasm` |
| Provider | `vqro.extension-health-default` | `https://github.com/ttenneb/vqro-extension-health-default.git` | `dist/vqro-extension-health-default.vqrox` | `health` | `services/health.wasm` |

Both use component world `vqro:extension/service@1.0.0`, package version `1.0.0`, and no host
capabilities. Their source URL, exact 40-character commit, repository-relative archive path,
package SHA-256, manifest SHA-256, component SHA-256, and descriptor SHA-256 MUST be recorded in
two checked-in production pin records before the product is enabled. The pin locations are:

- `extensions/first-party/extension-doctor/production-pin-v1.json`;
- `extensions/first-party/extension-health-default/production-pin-v1.json`.

This document fixes identities, reserved repository URLs, archive paths, service paths, and contract
bytes; it does not invent artifact or commit digests before the repositories produce reproducible
archives. An unset pin, placeholder digest, branch name, locally rebuilt archive, file/link
installation, or repository build instruction is not a product artifact and MUST fail the product
acceptance gate. Installing an exact artifact does not approve a binding.

### 2.1 Delivery authority boundary

Work under this decision MAY create the two local package repositories and build deterministic local
archives now. That reversible local work does not authorize creation of either remote repository,
pushing or publishing source or artifacts, installing from exact Git, or writing a production pin.
Those are external effects and remain blocked until the accountable user separately authorizes
creation and publication of both named remote repositories. The reserved URLs above describe the
intended identity; they are not a claim that either remote exists or may be created.

After that authorization, each repository must publish its independently reproducible archive at the
fixed path. Only then may reviewed commit, package, manifest, and component hashes be copied from the
actual bytes into the production pins and used for the existing explicit exact-Git install journey.
No implementation task may fabricate hashes, derive a pin from unpublished local bytes, publish on
implicit authority, or treat a local file/link install as satisfying this product boundary.

### 2.2 Doctor manifest-v3 declarations

Doctor's manifest has package ID and namespace `vqro.extension-doctor`, service ID `doctor`, and
`capabilities = []`. It declares exactly one import:

```toml
[[services.imports]]
id = "health-policy"
descriptor = "contracts/vqro.extension-health.v1.json"
descriptor_sha256 = "23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0"
required = true
```

It declares no exports. `required = true` deliberately prevents a host-started Doctor invocation
when the exact edge is absent or ineffective.

### 2.3 Health manifest-v3 declarations

Health's manifest has package ID and namespace `vqro.extension-health-default`, service ID
`health`, and `capabilities = []`. It declares exactly one export:

```toml
[[services.exports]]
id = "health"
descriptor = "contracts/vqro.extension-health.v1.json"
descriptor_sha256 = "23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0"
```

It declares no imports. The runtime MUST invoke this provider with the R1 no-host-call router;
an attempted host call fails under existing R1 semantics.

## 3. Frozen product contract

The import and export use exact identity:

- contract descriptor: `vqro.service-contract.v1`;
- contract ID: `vqro.extension-health`;
- contract version: `1.0.0`;
- canonical descriptor SHA-256:
  `23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0`;
- method: `evaluate`;
- maximum request: 16,384 bytes;
- maximum response: 4,096 bytes.

The digest is over host-canonical JSON for the following complete descriptor. This checked-in JSON
is the source for both package copies; an implementation MUST regenerate and compare the digest
rather than trusting this prose.

```json
{
  "contract": "vqro.service-contract.v1",
  "contract_id": "vqro.extension-health",
  "contract_version": "1.0.0",
  "methods": [
    {
      "id": "evaluate",
      "max_request_bytes": 16384,
      "max_response_bytes": 4096,
      "request_information": [
        "vqro.extension.aggregate-health",
        "vqro.extension.binding-authority",
        "vqro.extension.package-lifecycle"
      ],
      "request_schema": {
        "additionalProperties": false,
        "properties": {
          "bindings": {
            "additionalProperties": false,
            "properties": {
              "ready": { "maximum": 4096, "minimum": 0, "type": "integer" },
              "revoked": { "maximum": 4096, "minimum": 0, "type": "integer" },
              "suspended": { "maximum": 4096, "minimum": 0, "type": "integer" },
              "total": { "maximum": 4096, "minimum": 0, "type": "integer" }
            },
            "required": ["ready", "revoked", "suspended", "total"],
            "type": "object"
          },
          "contract": {
            "const": "vqro.extension-health-observation.v1",
            "type": "string"
          },
          "doctor_edge": {
            "additionalProperties": false,
            "properties": {
              "binding_id": { "maxLength": 128, "minLength": 1, "type": "string" },
              "binding_revision": {
                "maximum": 18446744073709551615,
                "minimum": 1,
                "type": "integer"
              },
              "consumer_manifest_sha256": {
                "pattern": "^[0-9a-f]{64}$",
                "type": "string"
              },
              "consumer_package_sha256": {
                "pattern": "^[0-9a-f]{64}$",
                "type": "string"
              },
              "provider_manifest_sha256": {
                "pattern": "^[0-9a-f]{64}$",
                "type": "string"
              },
              "provider_package_sha256": {
                "pattern": "^[0-9a-f]{64}$",
                "type": "string"
              }
            },
            "required": [
              "binding_id",
              "binding_revision",
              "consumer_package_sha256",
              "consumer_manifest_sha256",
              "provider_package_sha256",
              "provider_manifest_sha256"
            ],
            "type": "object"
          },
          "packages": {
            "additionalProperties": false,
            "properties": {
              "disabled": { "maximum": 1024, "minimum": 0, "type": "integer" },
              "enabled_ready": { "maximum": 1024, "minimum": 0, "type": "integer" },
              "enabled_unavailable": { "maximum": 1024, "minimum": 0, "type": "integer" },
              "total": { "maximum": 1024, "minimum": 0, "type": "integer" }
            },
            "required": ["disabled", "enabled_ready", "enabled_unavailable", "total"],
            "type": "object"
          },
          "registry_revision": {
            "maximum": 18446744073709551615,
            "minimum": 0,
            "type": "integer"
          },
          "services": {
            "additionalProperties": false,
            "properties": {
              "ready": { "maximum": 32768, "minimum": 0, "type": "integer" },
              "total": { "maximum": 32768, "minimum": 0, "type": "integer" },
              "unavailable": { "maximum": 32768, "minimum": 0, "type": "integer" }
            },
            "required": ["ready", "total", "unavailable"],
            "type": "object"
          }
        },
        "required": [
          "contract",
          "registry_revision",
          "packages",
          "services",
          "bindings",
          "doctor_edge"
        ],
        "type": "object"
      },
      "response_information": [
        "vqro.extension.normalized-health-findings"
      ],
      "response_schema": {
        "additionalProperties": false,
        "properties": {
          "contract": {
            "const": "vqro.extension-health-findings.v1",
            "type": "string"
          },
          "findings": {
            "items": {
              "additionalProperties": false,
              "properties": {
                "code": {
                  "enum": [
                    "binding_revoked",
                    "binding_suspended",
                    "package_disabled",
                    "package_unavailable",
                    "service_unavailable"
                  ],
                  "type": "string"
                },
                "count": { "maximum": 32768, "minimum": 1, "type": "integer" },
                "severity": {
                  "enum": ["error", "warning"],
                  "type": "string"
                }
              },
              "required": ["code", "severity", "count"],
              "type": "object"
            },
            "maxItems": 5,
            "type": "array"
          },
          "status": {
            "enum": ["healthy", "degraded", "unhealthy"],
            "type": "string"
          }
        },
        "required": ["contract", "status", "findings"],
        "type": "object"
      }
    }
  ]
}
```

The three request labels disclose aggregate extension health, exact binding authority, and package
lifecycle state flowing from Doctor to Health. The response label discloses normalized health
findings flowing from Health to Doctor. They are R1 consent labels, not semantic inspection claims.
Changing a label, schema, bound, method, endpoint, artifact, or digest requires a new inspected and
explicitly approved binding under the existing R1 rules.

## 4. Complete bounded observation

The host, not Doctor, constructs `vqro.extension-health-observation.v1`. It is complete because all
fields are required and its three histograms cover every authoritative package, declared component
service, and durable service binding present at one captured registry revision. It is bounded
because it carries fixed-category counts rather than inventories or raw diagnostics; its byte size
does not grow with the allowed 1,024 packages or 4,096 bindings.

The histogram invariants are:

- `packages.total = disabled + enabled_ready + enabled_unavailable`;
- `services.total = ready + unavailable`;
- `bindings.total = ready + suspended + revoked`;
- each package, service, and durable binding contributes to exactly one category;
- `doctor_edge` identifies the exact ready binding revision and both package and manifest SHA-256
  values for the exact consumer and provider artifacts used for this invocation.

`enabled_ready` means every declared component service currently required for that enabled package
is admitted; otherwise the package is `enabled_unavailable`. Binding categories are the frozen R1
derived statuses. Counts include Doctor, Health, and their binding, so the observation describes the
actual composition rather than a filtered view.

The observation MUST NOT contain credentials, environment values, request payloads, extension
payloads, package state values, terminal content, repository URLs, source or extraction paths,
host paths, arbitrary package-provided text, traps, logs, or raw errors. The only strings varying at
runtime are the bounded host-created binding ID and fixed-format hashes. Failures encountered while
reading authoritative state are not converted to free-form fields: capture fails with a normalized
host error before either package runs.

The host captures all counters, the registry revision, and `doctor_edge` in one App turn from the
same authoritative package registry, admitted-service table, and binding registry used to authorize
the call. It MUST reject arithmetic overflow, an inconsistent registry, an observation over 16,384
bytes, or a changed edge before dispatch. It MUST NOT truncate, sample, use a cache as authority, or
ask either package to discover host state.

The provider MUST independently validate the closed shape, contract string, count sums, maximum
bounds, fixed hash forms, and finding preconditions. The frozen R1 router remains responsible for
validating the actual edge and lease; the provider is not given another package's authority context.
A malformed or inconsistent observation returns `service_request_invalid`; the provider MUST NOT
guess missing values.

## 5. Meaningful ownership split

Health owns policy. From the validated facts it chooses which nonzero conditions become findings,
their severity, and the aggregate status. The default 1.0.0 policy emits at most one finding per
code in the descriptor order, uses exact counts, marks unavailable packages/services and suspended
bindings as errors, marks disabled packages and revoked bindings as warnings, returns `unhealthy`
when any error exists, `degraded` when only warnings exist, and `healthy` otherwise. A replacement
provider may adopt a different policy only through a new exact artifact and explicit binding
approval; it still has to satisfy the same bounded output contract.

Doctor owns presentation. It checks uniqueness, order, count positivity, status consistency, and the
closed response shape, then maps finding codes to stable human messages and emits the stable JSON
contract below. It never invents health policy, downgrades severity, or includes provider-authored
free text.

This boundary is useful rather than fixture indirection: the provider can evolve or be replaced as
a policy product without gaining host authority or changing CLI/API presentation, while Doctor can
improve wording, exit behavior, and JSON stability without deciding what is healthy. The host owns
facts and authorization, Health owns judgment, and Doctor owns presentation.

## 6. Command, API, and output

The ordinary command is:

```text
vqro extension doctor
vqro extension doctor --json
```

The additive generation-1 endpoint method is `extension.doctor`. It takes an empty closed object and
uses the ordinary generation-1 response envelope. Only a structurally and semantically valid Doctor
result produces `SuccessResponse`, with the frozen `extension_doctor_diagnostic` result variant:

```json
{
  "id": "REQUEST_ID",
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

The inner diagnostic is the stable `vqro.extension-doctor-diagnostic.v1` value. The CLI prints that
inner value with `--json`; otherwise it renders the human form. Typing in a pane does not dismiss or
affect the request. No preflight, relay, provider, validation, timeout, cancellation, revocation, or
stale-lease failure may be encoded as this success variant.

Each nonempty finding is `{ "code": <closed code>, "severity": "warning"|"error",
"count": <positive integer>, "message": <Doctor-owned bounded stable text> }`. Findings retain the
contract order. Doctor 1.0.0 uses these exact message templates (decimal `COUNT`, with no singular
variant):

| Code | Message |
| --- | --- |
| `binding_revoked` | `COUNT extension service bindings are revoked.` |
| `binding_suspended` | `COUNT extension service bindings are suspended.` |
| `package_disabled` | `COUNT extension packages are disabled.` |
| `package_unavailable` | `COUNT enabled extension packages are unavailable.` |
| `service_unavailable` | `COUNT extension services are unavailable.` |

The human form prints `Extension health: STATUS`, then one `SEVERITY: MESSAGE` line per finding in
contract order. JSON has no timestamps, random IDs, paths, package payloads, or raw errors, making it
stable for scripts. Exit status is 0 for `healthy`, 1 for `degraded` or `unhealthy`, and 2 when no
valid diagnostic can be produced.

The host MUST NOT expose the generic `vqro service-binding call` arguments through this command.
`extension.doctor` resolves only the exact Doctor package/service/import and current approved
binding, supplies consumer method `doctor`, and supplies the freshly captured observation. Caller
input cannot choose a provider, binding, method, payload, artifact, or information label.

## 7. Authority and approval UX

Installation of either curated exact-Git package uses the existing digest-confirmed package UX and
grants no binding. After both exact artifacts are enabled, the Extensions surface offers **Review
Extension Doctor connection**. Inspection displays, before mutation:

- exact consumer and provider package IDs, versions, package and manifest hashes;
- services `doctor` and `health`, import `health-policy`, and export `health`;
- contract identity and descriptor digest;
- method `evaluate`, all four information labels, and both byte limits;
- the complete consent digest and the fact that Health gets no host calls.

The separate **Approve connection** action requires the existing exact consent digest, registry
revision, optional prior binding revision, and `confirmed: true`. CLI approval, when used, remains
the existing inspect-then-approve Service Fabric flow. Installing, enabling, opening Doctor, or
running `vqro extension doctor` MUST NOT create, infer, repair, or approve a binding. There is no
first-run consent shortcut, bundled-provider exception, default-provider selection, approval by
catalog status, or approval carried across different artifact bytes.

Replacing either artifact suspends the edge until the user inspects and approves the new exact
consent. Restoring identical approved bytes may return it to ready under frozen R1 semantics.
Revocation remains explicit and durable.

## 8. Missing, suspended, and revoked Health

A missing provider, absent binding, suspended edge, or revoked edge prevents the required-import
consumer invocation. This is an intentional R1 preflight outcome, not a special call around the
fabric. `extension.doctor` returns the ordinary generation-1 `ErrorResponse`; it MUST NOT return a
second product-specific success or unavailable-result contract:

```json
{
  "id": "REQUEST_ID",
  "error": {
    "code": "service_binding_unavailable",
    "message": "service binding is unavailable"
  }
}
```

Absent or suspended uses `service_binding_unavailable`; revoked uses
`service_binding_revoked`. Failures after invocation starts use the applicable normalized stable R1
code, including busy, timeout, cancellation, stale lease, provider failure, invalid request, or
invalid response. The API error message remains the ordinary bounded generation-1 message; clients
MUST branch on `error.code`, not product prose. There is one failure channel and one success channel.

The CLI alone maps `service_binding_unavailable` to `Extension Doctor unavailable: an enabled Health
provider and approved connection are required.`, maps `service_binding_revoked` to `Extension Doctor
unavailable: the connection was revoked; inspect and approve a connection.`, and maps every other
stable R1 code to `Extension Doctor unavailable (CODE).` It writes that text to stderr and exits 2.
With `--json`, it emits the ordinary `ErrorResponse` instead of a diagnostic-shaped value. None of
these surfaces exposes raw worker errors, paths, or stale consent.

The host does not invoke Doctor, invoke Health, select another provider, or auto-approve after a
failed preflight. Therefore every Doctor invocation that actually begins still receives a complete
observation and makes exactly one approved R1 call; unavailable preflight attempts begin no package
invocation at all.

Once Doctor begins, it MUST attempt exactly one `service.call` to import `health-policy`, method
`evaluate`, with the observation unchanged. Zero calls, retries, a second call, a different payload,
or a different method fail the outer invocation with the applicable ordinary `ErrorResponse`.

## 9. Off-App execution, restart, and resupply

`extension.doctor` follows the frozen R1 deferred path:

1. In one nonblocking App turn, validate the exact ready edge, capture the complete observation,
   clone the provider worker's bounded endpoint, mint the ordinary immutable revocable lease, and
   release registry and state locks.
2. Enqueue Doctor on its existing bounded worker and return from the App actor without waiting.
3. Doctor makes its one synchronous R1 call. The worker uses the existing direct bounded relay and
   one-shot completion; Health runs with no host-call router.
4. Doctor returns its diagnostic through the existing bounded candidate completion. A timeout
   backstop settles the deferred API response. No nested completion occupies App event capacity.
5. Revalidate revocation, artifacts, worker generations, deadline, and cancellation before delivery;
   stale output is dropped under existing R1 fencing.

No observation, invocation lease, pending call, provider finding, or rendered diagnostic is durable.
On process restart or live handoff, pending Doctor work fails closed. The host first recovers package
activation and binding state by existing rules, admits exact workers, and derives a fresh binding
generation. Every later `extension.doctor` request then reconstructs and resupplies a new complete
observation from recovered authoritative state. Durable approval survives only when the exact
artifacts do; a lease never survives. A cached pre-restart observation MUST NOT be replayed.

## 10. Compatibility

- Manifest v2, its `provides` values, and all pre-existing packages remain unchanged.
- The two packages use manifest v3 and the existing component world; they request no new host
  capability.
- The descriptor uses only the frozen deterministic JSON Schema subset. R1 continues structural
  envelope/membership/byte validation; Health performs the product semantic validation.
- `extension.doctor` is an additive endpoint-generation-1 method. A server lacking it reports the
  normal missing-method client-local outcome without disconnecting that or other servers.
- Existing endpoint codecs, method parameter shapes, private protocol framing, package state schema
  2, binding records, PTYs, Collections, Spaces, and generic extension operations do not change.
- A newer client MUST omit or disable Doctor for a server that does not advertise
  `extension.doctor`; it MUST NOT emulate the command with a caller-selected generic binding call.
- Contract evolution requires a new contract version and digest. Existing exact bindings never
  structurally subtype, follow a compatible-looking provider, or auto-migrate.

## 11. Acceptance tests

Local contract and deterministic-build tests may run before publication authority. Product
acceptance additionally requires the two independently built and exact-Git-installed production-pin
artifacts, not in-process substitutes or a shared fixture archive.

1. **Publication authority:** before any remote repository creation, push, artifact publication,
   production-pin write, or exact-Git install, record the accountable user's separate authorization
   for both named repositories. Prove local-only work did not perform those effects. If authority is
   absent, keep every publication/install gate blocked and leave commit and artifact hashes unset.
2. **Artifact and contract identity:** after authorization and publication, verify both repositories,
   commits, archive paths, package, manifest, component, and descriptor digests; independently
   canonicalize the descriptor and obtain
   `23aec77a84334a070d1de2e918db9ac0a084c9af2332b99b5824cde0156326e0`.
3. **No implicit binding authority:** install and enable both packages, run Doctor, and prove no
   binding was created and the API returned ordinary `ErrorResponse`; inspect then approve the exact
   consent and prove one edge becomes ready.
4. **Ordinary product path:** drive `vqro extension doctor --json` and `extension.doctor` through the
   real server; prove the host supplies one complete observation containing both consumer/provider
   package and manifest hashes, Doctor makes exactly one approved call, Health makes no host call,
   and only valid output becomes the `extension_doctor_diagnostic` success variant.
5. **Complete capture:** populate every package, service, and binding status and prove totals and
   category sums match the authoritative registries at one revision, including Doctor, Health, and
   their edge. Maximum registries still produce a request below 16,384 bytes with no truncation.
6. **Data minimization:** seed credentials, payloads, repository/source paths, traps, logs, and long
   raw errors in reachable host state; prove none occur in the observation, provider request,
   findings, successful diagnostic, `ErrorResponse`, or CLI unavailable text.
7. **Provider validation:** send every missing field (including either manifest hash), unknown field,
   wrong contract, invalid hash, inconsistent total, overflow boundary, and oversized request; prove
   fail-closed `service_request_invalid` behavior and no finding is delivered. Separately prove the
   host rejects a wrong edge identity before provider dispatch.
8. **Policy semantics:** prove each default finding, severity, count, order, and aggregate status;
   prove zero counts emit no finding and mixed warning/error conditions are deterministic.
9. **Consumer validation and presentation:** reject duplicate, reordered, unknown, inconsistent,
   malformed, and oversized findings; snapshot stable human text, JSON keys, and exit statuses
   without accepting provider-authored prose.
10. **One wire failure channel:** cover missing provider, unbound import, disabled provider,
    suspended artifact, explicit revocation, busy, timeout, cancellation, provider failure, invalid
    response, and restored identical bytes. Prove every failure is ordinary `ErrorResponse` with the
    normalized R1 code, `--json` preserves that envelope, the CLI alone renders stable unavailable
    text and exits 2, and no pseudo-success unavailable contract exists.
11. **Fencing and failure:** replace/restart either worker, revoke during a slow call, saturate the
    provider, time out, cancel, trap, and return late. Prove stale findings never reach Doctor and
    each attempt performs at most one call with no retry.
12. **Off-App behavior:** hold Health until timeout while exercising pane input, PTY output,
    package inspection, and another client; prove the App actor remains responsive and nested work
    does not consume App event capacity.
13. **Restart resupply:** approve once, restart the server, restore the exact artifacts, and prove a
    fresh observation and lease are created. Mutate authoritative extension state before and after
    restart and prove the post-restart diagnostic reflects only freshly reconstructed state.
14. **Compatibility:** run frozen generation-1 endpoint fixtures and existing package, Service
    Fabric, Collections, Spaces, PTY recovery, and old-client tests unchanged; prove unsupported
    servers disable only Doctor and existing clients keep one success/error envelope split.
15. **Independent replacement:** install another independently built provider with the same exact
    contract identity, inspect and explicitly approve a new edge, and prove policy output can change
    while Doctor's diagnostic contract and presentation rules remain stable.

## 12. Concrete observations R2 must learn

R1 must retain a small, bounded product evidence record for the next design decision. It records
facts, not proposed runtime mechanisms:

- serialized observation bytes at empty, typical, and maximum package/binding counts;
- App-turn time to derive the histograms and lock hold time, with 1 and at least 15 populated
  packages and with the maximum registry smoke case;
- end-to-end Doctor latency split into capture, consumer, relay/provider, and presentation;
- queue-busy, timeout, cancellation, revocation, stale-generation, malformed-request, and malformed-
  response frequencies during tests and an opt-in dogfood period;
- whether aggregate counts were enough to resolve real failures, which finding codes were useful,
  and which investigations still required separate package inspection;
- restart/handoff resupply results, including proof that no old observation or lease was reused;
- the number of consent reviews caused by consumer, provider, descriptor, label, and artifact
  changes, and whether users understood that installation was not approval;
- whether independent provider replacement changed policy without presentation drift;
- observed App responsiveness and worker/queue saturation under slow Health execution;
- every request to add identity lists, timestamps, raw errors, paths, payloads, retries, events, or
  automatic provider selection, with the concrete user need and the bounded alternative considered.

These observations decide whether R2 needs a different snapshot shape or interaction model. They do
not declare any R2 runtime type implemented, and they do not weaken the frozen R1
identity, approval, one-call, no-provider-host-call, or off-App guarantees.
