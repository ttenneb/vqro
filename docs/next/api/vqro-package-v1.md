# `vqro.package.v1`

Status: package/lifecycle contract with a production exact-Git component journey and verified runtime instantiation. Exact pinned feature packages receive their separately documented leases; manifest-v3 packages may participate in the experimental Service Fabric when an exact durable binding is approved. File/link source contracts remain lower-level foundations rather than the arbitrary-Git user journey. See the [extension platform status](extension-platform-status.md) for the capability matrix.

## Package archive

A `.vqrox` file is an uncompressed deterministic tar archive. It contains regular files only, in strict bytewise path order, with zero uid/gid/mtime and modes `0644` or `0755`. Paths are UTF-8, relative, portable to Windows, and bounded. Links, devices, FIFOs, sparse entries, duplicate paths, case-fold collisions, file/descendant collisions, and unsafe names are rejected before extraction.

Required root files:

- `vqro-extension.toml`: manifest v2 or separately parsed manifest v3;
- `checksums.sha256`: exact inventory of every other file.

`checksums.sha256` uses lowercase SHA-256, two ASCII spaces, a package-relative path, and LF. Lines are in strict bytewise path order. It cannot list itself; missing, extra, duplicate, case-colliding, or mismatched entries reject the package.

Current limits are 8 MiB archive/file, 64 MiB expanded data, and 10,000 entries. The generated normative schema is [`vqro-package-v1.schema.json`](vqro-package-v1.schema.json).

## Manifest v2

Manifest v2 declares:

```toml
manifest_version = 2
package_contract = "vqro.package.v1"
id = "example.package"
version = "1.2.3"
min_vqro_version = "0.9.0"
namespaces = ["example.package"]

[[services]]
id = "main"
provides = ["host_document"]
capabilities = ["state"]

[services.runtime]
kind = "component"
component = "services/main.wasm"
world = "vqro:extension/service@1.0.0"
```

Each service has exactly one tagged runtime. A process runtime instead provides fixed `args` and an exact package-relative executable for each supported target. Shell commands, PATH lookup, `vqro` self-spawn, absolute/parent paths, duplicate service IDs, duplicate payload ownership, and undeclared payloads are rejected. Packages with no services are valid asset-only packages. The generated schema is [`vqro-extension-manifest-v2.schema.json`](vqro-extension-manifest-v2.schema.json).

Manifest v1 and service protocol 2 remain unchanged compatibility inputs. Manifest v2 does not reinterpret either contract.

## Manifest v3

Manifest v3 adds exact `services.exports` and `services.imports` declarations referencing bounded `vqro.service-contract.v1` JSON descriptors under `contracts/`. Descriptor identity includes canonical JSON SHA-256, not only a free-form contract name. Manifest v2 remains immutable: its `provides` strings never become fabric exports, and v3 is parsed through a distinct deny-unknown-fields DTO.

Imports and exports do not grant host capabilities, choose providers, or publish documents. A host-owned exact-artifact ServiceBinding is separately inspected and approved. See [`vqro-service-fabric-r1.md`](vqro-service-fabric-r1.md) and the generated [`vqro-extension-manifest-v3.schema.json`](vqro-extension-manifest-v3.schema.json).

## Sources and provenance

Package lifecycle state uses one model for:

- **curated** sources: catalog, publisher, repository, commit, package digest, manifest digest, and exact authorized namespaces;
- **file** sources: an immutable local `.vqrox` copied into content-addressed storage;
- **link** sources: a mutable canonical developer directory, revalidated on registration/reload and accepted only with an explicit bounded trust approval.

`host.*` is never package-owned. Curated `vqro.*` claims require publisher `vqro` and an exact catalog authorization. Immutable local files cannot claim `vqro.*`. Trusted developer links may claim non-`host.*` namespaces. Namespace collisions include disabled or removed packages while retained data still owns the claim.

Installed file artifacts live under a SHA-256-derived immutable directory and retain the original `.vqrox`. Extraction uses an unpredictable private staging directory and no-follow, root-relative file creation. Activation must reverify both archive and extracted inventory; mutation or an added file fails closed. Developer links remain visibly mutable, are revalidated on state reload and activation, and are never treated as curated artifacts.

## Lifecycle and recovery

The package registry records source, claims, revision, status, active and previous artifact pointers, runtime generation, retained-data state, and deterministic load order. Existing marketplace entries are exposed only through a named legacy compatibility projection; their `Bundled` and `AgentIntegration` tags are not package-v1 runtime selectors. Extension state schema 2 also stores exact, user-approved service bindings; schema 1 migrates with no fabricated bindings, and old writers reject schema 2.

The activation journal has `prepared`, `artifact_committed`, and `registry_committed` phases plus an idempotent operation ID, expected registry and package revisions, previous pointer, and candidate pointer. Journal writes are bounded, atomic, and durable. Recovery can commit an already-retained candidate, keep an already-committed candidate, roll back an uncommitted candidate, or stop on conflicting state. The current package lifecycle verifies, instantiates, negotiates, and retains enabled component services, including restoration after restart. Runtime instantiation is not a capability lease or provider admission: these services cannot yet read production host state, publish a client-visible document, or become feature authority.

Disabling or removing a package retains namespaced data. Explicit data deletion remains separately confirmed and journaled. Native process packages will remain trusted same-user code, not sandboxes.
