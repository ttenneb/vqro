# Identity, registry and live-connection hygiene (2026-11-17)

Implemented on `identity-hygiene`, based on `25cb901`. This is mechanism hardening, not a new ownership/grant model,
execution lifetime policy, relay or agent transport. Living contracts: `../spec/core-protocol.md` and
`../spec/provider-v0.md` section9. The old in-process architecture/backend draft is no longer authoritative.

## Why these changes

- `IdentityMap` retained every retired lifetime and repeatedly scanned/cloned it. It now retains active records,
  indexed by tuple/ref/provider, plus a durable monotonic `next`. Loading older files first accounts for **all** id
  keys, including retired/legacy records; pruning can never make those ids available again. A retired tuple gets a
  new id, including after restart. Layouts are not rebound. Repeated known facts use the indexed no-write path.
- `Core::upsert_resources` admits an inventory/sync chunk, computes allocation/removal in one identity candidate,
  writes once, then publishes. Final-chunk authoritative removals share that write. Pending persistence recovery
  is batched too. Pre-rename failures publish no ids; post-rename uncertainty adopts/reports the entire candidate.
  Observed replacement/closure still fences old routes when disk retirement fails. No effects/input are replayed.
- Retiring 600 hosts exposed another per-item durable loop: bridge resume expiry cloned/wrote the credential map
  per host (and cloned it even for agent ids). `Tokens::expire_hosts` removes a host batch with one existing durable
  replace and avoids cloning if none match. It does not move credential ownership or change resume policy.
- The old 32-provider ceiling cannot represent one provider per agent. Admission now allows 1024 names and 4 MiB
  total retained registration metadata, with a 1 KiB/provider accounting floor and the existing 128 KiB individual
  limit. Native/wire paths use the same preflight, before runtime credential rotation. Cached charges make the
  aggregate check constant-time.
- Disconnected runtime rows/compatibility aliases/error state leave only when active identities, retained resources
  and pending facts are all absent. Host-bound resume credentials remain independent, bounded by retained hosts;
  backend rows and stale runtime resources are not silently deleted. Reclamation emits `provider.removed` and the
  compatibility alias. A core-wide source/generation lease counter preserves monotonic generations without an
  unbounded historical-name table. Old source tokens/routes still cannot affect a replacement.
- Event subscription tasks select peer closure alongside broadcast input, so an idle bus cannot retain departed
  receivers/buffers. Live panels cache their own encoded charge and the aggregate; a publish encodes only its
  candidate. Clear/replacement/disconnect update exact charges, including late old-connection cleanup.
- Only the existing supervised-lifetime token proves `ext:<name>`. Other claimed labels are descriptive client
  metadata, not extension authorship. Live-view overwrite/clear rejects another live author just as command
  registration does; a verified same-extension replacement may take over. This is not entry access control.
  SDK/D24 tests now use actual private supervised token fixtures instead of unverified extension labels.

## Regressions and measurements

`5458d2c` fails all six public `tests/identity_hygiene.rs` regressions and the provider-scale probe. The latter stops
at the old 32-name ceiling. Baseline inventory600 held the core lock 1070.740 ms; 10k idle subscribers grew Core RSS
from 15.96 to 127.42 MiB. No production credentials or endpoints were used for that comparison.

Latest focused runs, debug build, first 8 available CPUs and nice 10:

| Probe | Result | Guard/evidence |
|---|---|---|
| 10k durable allocate/retire lifetimes | 350.380 ms total, 0.491 ms maximum lifetime; final file below 128 bytes | Same tuple gets `r10001` after reopen; retired/legacy high-water regression |
| 600 new native ids, private tmpfs | 39.165 ms public lock hold; injected writer-count test 54.875 ms | One write, 150 ms hold limit, pre-/post-rename fault assertions |
| 600 new native ids, private home **btrfs** | 43.471 ms lock hold | Same public regression with private `TMPDIR`; IO remains under core lock |
| 10k idle subscriber closes | 14.31→14.61 MiB RSS after drain | 8 MiB warm-baseline delta guard; no later event needed |
| 600 SDK runtime providers + host backend | 601 registry entries, 1200 resources; setup 9.944 s | Real provider RPC/SDK, fake host facts; no real Pi/model calls |
| Runtime registration | p50 4.412 ms / p99 16.049 ms / max 26.641 ms | Includes hello/registration/credential durability |
| Active snapshot | 868759 bytes / 189.450 ms end-to-end | 500 ms guard; includes JSON reply parsing |
| Core RSS | 13.24 MiB baseline / 88.73 MiB active / 104.60 MiB after churn | Active delta below 128 MiB; not a claim that allocators return pages |
| 600 runtime-agent retirements | 5.745 s, 600 independent authoritative RPCs | Still one durability write per distinct provider message, not a cross-peer batcher |
| 600-host retirement | 39.675 ms vs measured pre-batching 1.65 s | One final authoritative inventory and credential-expiry batch |
| 2000 further sequential lifetimes | 7.534 s | One backend alias/provider, two clients, zero resource/identity/credential records after drain |

The identity10k and runtime-scale fixtures use private tmpfs; the additional 600-identity btrfs check exercises actual
home-filesystem durability without touching user state. These are target-shape measurements, **not** worst-case
32 MiB payload/disk-latency guarantees or a 600-provider high-rate status flood. Separate existing full-gate scale
budgets still apply. No IO worker or asynchronous durability queue was added because the measured 600-new-id hold
fits 150 ms on both filesystems.

Focused validation: 116 library tests, 16 core, 7 providers, 6 undo attribution (native UI separately opt-in), 6 identity,
1 live-byte reconciliation and 1 provider-scale test pass. Unit checks cover aggregate registry bytes/count and
reaped-name generation/old-route fencing. Public managed-token fixtures exercise same-author replacement and
original-invoker SDK behavior; collision tests cover both overwrite and clear.

Full release authority is `scripts/check.sh full`, with local receipts under
`~/.local/share/herdr/scratch/prim-independent-review/identity-full-{first,final}.json`. The first run was 18/19:
Agents UI timed out opening the requests popup; an identical isolated retry passed, with no extension crash/RPC
failure. Failure receipt `ep-kf4ghhyl/receipt.json` and retry `ep-4mr7d7h5/receipt.json` are retained in scratch. No
gate was skipped or relaxed: use the final full receipt for merge acceptance, not the standalone retry.

## Integration: escaped commit metadata

After integration with the history-byte budget, the old escaped-author admission fixture timed out: it set
`hello.client` to 500 KiB of NULs and assumed that label became the transaction author. It no longer does.
`src/server.rs::apply_prepared` charges encoded `(author, on_behalf_of, note)` plus operations; the committer
reaccounts the same actual metadata at preparation. `hello.client` stays descriptive connected-client state and is
not captured/copied into each write or history record. Charging it per write would charge the wrong value.

The corrected `tests/commit_off_lock.rs` probe uses valid 4096-byte NUL notes and operation payloads near the 8 MiB
queue limit. It asserts raw metadata fits while encoded metadata does not, admits two writes and rejects the third
as overloaded. A temporary raw-note admission mutant (not shipped) fails with timeout; restoring actual encoded
charging passes. A separate test admits three small writes despite huge descriptive labels and verifies compact
connection-scoped authors in history. No product accounting or authentication was weakened. Evidence:
`admission-metadata-before.log`, `admission-metadata-negative-control.log`, `admission-metadata-green.log`.

Other local evidence: `identity-hygiene-before.log`, `identity-commit{1,2}.log`, `identity-btrfs600.log`,
`provider-registry-scale-final.{log,json}`. Tests create and clean their own processes, private state/sockets,
empty extension declarations and hc guards; no live prim/hc/tmux, installation, publication or model calls.
