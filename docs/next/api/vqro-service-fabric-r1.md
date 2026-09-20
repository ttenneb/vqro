# Extension Service Fabric R0/R1

Status: experimental manifest, binding, and component-call contract. Collections and Spaces do not use this fabric yet.

The accepted architecture is [`extension-service-fabric-r0-r1.md`](../architecture/extension-service-fabric-r0-r1.md). This document summarizes the implemented public data and endpoint surface.

## Manifest v3

Manifest v3 is a separately parsed language. Manifest v2 remains frozen and its `provides` strings are not fabric exports.

A v3 component service may declare exact exports and imports:

```toml
manifest_version = 3
package_contract = "vqro.package.v1"
id = "example.consumer"
version = "1.0.0"
min_vqro_version = "0.9.0"
namespaces = ["example.consumer"]

[[services]]
id = "main"
capabilities = []

[[services.imports]]
id = "echo-source"
descriptor = "contracts/example.echo.v1.json"
descriptor_sha256 = "<lowercase SHA-256 of canonical descriptor JSON>"
required = true

[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
```

Exports use `[[services.exports]]` with the same `id`, `descriptor`, and `descriptor_sha256` fields. IDs are local to one service. An import and export match only when their exact contract identities match.

The generated schema is [`vqro-extension-manifest-v3.schema.json`](vqro-extension-manifest-v3.schema.json).

## `vqro.service-contract.v1`

A descriptor is closed, bounded JSON:

```json
{
  "contract": "vqro.service-contract.v1",
  "contract_id": "example.echo",
  "contract_version": "1.0.0",
  "methods": [
    {
      "id": "echo",
      "request_schema": { "type": "object" },
      "response_schema": { "type": "object" },
      "request_information": ["user.supplied.text"],
      "response_information": ["provider.generated.text"],
      "max_request_bytes": 4096,
      "max_response_bytes": 4096
    }
  ]
}
```

Contract identity is `(contract_id, contract_version, descriptor_sha256)`. The digest covers host-canonical JSON, so formatting and object-key order do not change identity. Duplicate object keys, floats, unknown fields, malformed identifiers, unsorted methods/categories, empty method sets, excessive nesting, and out-of-range byte limits are rejected.

Request and response schemas use a deterministic closed subset of JSON Schema 2020-12. Supported keywords are `$schema` (only the canonical 2020-12 URI), local `$ref` into `$defs`, `$defs`, `$id`, `type`, `properties`, `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minLength`, `maxLength`, `minimum`, `maximum`, `enum`, `const`, `oneOf`, `anyOf`, `allOf`, `not`, `pattern`, `format`, `title`, `description`, and `default`. Schema-valued keywords must contain object schemas, regular expressions must compile, bounds must be ordered integers, and unknown keywords or malformed keyword values are rejected. This validation establishes a stable identity language; R1 does not perform general payload validation against those schemas.

Information categories disclose the reviewed direction of data flow. The host enforces exact artifacts, methods, schemas-as-identity, direction, and byte limits; it does not claim to infer the semantic category of arbitrary JSON. The generated descriptor schema is [`vqro-service-contract-v1.schema.json`](vqro-service-contract-v1.schema.json).

## Durable bindings

`extensions.json` schema 2 contains `vqro.service-binding-registry.v1`. Schema 1 migrates to schema 2 with an empty registry. No binding is inferred from manifest-v2 `provides` strings.

A binding records:

- exact consumer and provider package/version/package SHA-256/manifest SHA-256;
- service and import/export endpoint IDs;
- exact contract identity;
- all approved methods, information categories, and byte bounds;
- consent SHA-256;
- binding and registry revisions;
- state `approved` or `revoked`.

R1 binding approval is limited to enabled exact-Git component packages; curated discovery, developer links, file imports, legacy processes, and manifest-v2 `provides` do not qualify. Bindings are exact-artifact authority. Package replacement, disablement, removal, runtime failure, or declaration mismatch derives `suspended`; there is no automatic provider selection or rebinding. Restoring identical approved bytes may become ready again. Revocation prevents future calls and cancels runtime leases but cannot erase information already disclosed.

## Endpoint methods

All methods are additive generation-1 endpoint methods:

- `extension.service.binding.inspect` — resolve two declared endpoints and return the exact consent document and digest without mutation;
- `extension.service.binding.approve` — persist the reviewed edge using registry/binding revision CAS, exact consent digest, and `confirmed: true`;
- `extension.service.binding.list` — list durable records and derived `ready`, `suspended`, or `revoked` status;
- `extension.service.binding.revoke` — revoke using registry/binding revision CAS and `confirmed: true`;
- `extension.service.binding.invoke` — start one bounded outer consumer invocation using an exact binding revision and consumer method. Completion is deferred off the App actor and returns the consumer result or a normalized failure.

Package inspection and installed-package inventory also expose `provided_contracts` and `required_contracts`, including method information-flow disclosure. The Extensions detail/confirmation surface renders these declarations.

## R1 call

The consumer component uses the existing WIT `host.call` import with:

```json
{
  "type": "host_call",
  "call_id": "consumer-local-correlation",
  "identity": { "package_id": "...", "namespace": "...", "service_id": "...", "generation": 1 },
  "depth": 1,
  "capability": "service.read",
  "method": "service.read.call",
  "namespace": "consumer.namespace",
  "params": {
    "contract": "vqro.service-read-call.v1",
    "import_id": "echo-source",
    "method": "echo",
    "payload": {}
  }
}
```

The package cannot name its provider. Before the outer consumer invocation, the App resolves the exact durable binding and mints a short-lived revocable lease containing the provider worker endpoint and current artifact/runtime generations. The consumer worker uses `try_send` to the provider's existing bounded queue and waits only until cancellation or deadline. The provider runs with the no-host-call router, so nested calls and capability inheritance are impossible. Completion returns directly to the consumer worker and does not occupy the App event channel.

Only one service call is admitted per outer invocation. Queue saturation, cancellation, timeout, revocation, replacement, retirement, malformed output, and stale generations fail closed. Request and response payloads are JSON and are bounded by the approved method descriptor. Every required import on the consumer service must have a ready exact binding before the host starts the consumer.

The experimental `extension.service.binding.invoke` API and `vqro service-binding call` CLI provide the ordinary host-started path without making a product composition authoritative. The App resolves authority and enqueues the consumer, then returns to its event loop; the consumer and provider workers execute off-actor, and a bounded completion or timeout settles the API response. Component conformance separately uses two independently installed manifest-v3 package artifacts to prove the direct broker path. The platform status remains experimental until an ordinary product composition ships.
