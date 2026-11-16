# prim execution host contract, v0 (draft)

Status: draft. This is the contract the core depends on (D9). Any host that satisfies it can run prim's processes:
a prim-owned supervisor (`prim-sup`), or an adapter over herdr-core. M1 compares the two against this document.
The goal is to freeze it as v1 after two hosts satisfy it and it survives the restart tests (roadmap M6).
MUST/SHOULD are as in RFC 2119.

## 1. Lifetime contract (the part that matters most)

A host MUST state, per guarantee below, whether it provides it. The core reads this from `hello.guarantees`.

| Guarantee | Meaning |
|---|---|
| `survives_client_restart` | Children keep running when the core disconnects or restarts |
| `survives_host_upgrade` | Children keep running across a host upgrade (needs handoff) |
| `child_dies_with_host` | If the host dies, children are terminated (process group, Job Object). If false, children may be orphaned and keep running |
| `orphan_reclaim` | After a host restart, surviving orphans can be found and re-adopted, or at least listed |
| `output_retained_while_detached` | Output produced while no client is attached is buffered, up to a stated bound |
| `exit_retained` | Exit status is retained until explicitly forgotten |

Children never die "because their parent died" by default: on Unix they are re-parented and may get SIGHUP (PTY) or
SIGPIPE (pipes), or keep running. A host that wants `child_dies_with_host` must do it explicitly.

## 2. Scope

The host owns processes and their byte streams. It MUST NOT parse stream contents, know about agents, make delivery
decisions, or persist anything beyond its stated buffers.

## 3. Transport

- A user-only local transport: Unix socket (mode 0600) or Windows named pipe. Unix socket paths MUST stay under the
  platform limit (103 bytes on macOS).
- Frames: 4-byte big-endian length, a JSON header, and, if the header has `"len": n`, n raw payload bytes.
  (**Open**: benchmark against JSON lines with base64 in M1.)
- Requests carry `id`; responses echo it. Pushed frames have no `id`.

## 4. Messages

| Request | Fields | Result |
|---|---|---|
| `hello` | `version`, `client` | `version`, `features[]`, `guarantees{}`, `boot_id`, `platform` |
| `spawn` | `proc` (client-chosen ID), `argv`, `env`, `cwd`, `stdio: "pipes" \| "pty"`, `size?` | `pid` |
| `write` | `proc`, payload | `offset` (stdin bytes written so far) |
| `close_stdin` | `proc` | ok |
| `resize` | `proc`, `cols`, `rows` (PTY only) | ok |
| `terminate` | `proc`, `how: "interrupt" \| "terminate" \| "kill"`, `tree?` | ok |
| `read` | `proc`, `stream: "out" \| "err"`, `from`, `max` | payload, `next`, `missed?` |
| `follow` | as `read`, without `max` | pushes `data` frames until `unfollow` or disconnect |
| `list` | | per proc: `argv`, `pid`, `state`, stream end offsets, `exit?`, `orphaned?` |
| `forget` | `proc` | drops buffers of an exited proc |

Pushed frames: `data {proc, stream, offset, payload}`, `exit {proc, code, how, out_end, err_end}`.
`how` uses portable terms; the POSIX signal or Windows exit code is included as `raw` for diagnostics only.

## 5. Semantics

- **Offsets** are absolute byte counts per stream since spawn and never reset while the proc exists.
- **Buffers** are bounded per stream (default 8 MiB, **Open**). Reading below the oldest retained offset returns
  `missed: {from, to}` and then what is retained. The host never blocks a child for a slow reader; it evicts.
  Consequence: after a long disconnect, history is lost and the core must report the gap (D10).
- **`boot_id`** changes when the host restarts. A core seeing a new `boot_id` MUST treat procs as unknown until `list`
  says otherwise (orphans may still exist).
- **No replay**: the host writes to a child only on an explicit `write`.
- **`write` delivery**: a `write` whose response was lost is `indeterminate` to the core (D10). The core may compare
  stdin offsets after reconnecting to resolve it; it never resends blindly.
- **Proc IDs** are client-chosen and unique per `boot_id`.
- **Multiple clients** MAY connect; concurrent `write`s are not coordinated (that is policy).

## 6. Mapping to herdr-core (to verify in M1)

`spawn(pty)` → `terminal.run`; `write` → attach-stream input (verify it bypasses `send`'s quiet/draft delays,
`research/backends.md`); `follow` → `terminal.attach`; `terminate` → `terminal.kill`; `list` → `terminal.list`;
`missed` → events `missed`. Gaps: no `stdio: "pipes"`; offsets may need emulating.

## Not in v0

Authentication beyond transport permissions, remote transport, disk-backed buffers, live host upgrade, resource
limits. Listed in `../open-questions.md`.
