# Contributing to Vqro

Vqro is a private, owner-directed project. Unsolicited issues and implementation
pull requests are not accepted. The owner explicitly authorizes each specific
outside task before an issue or pull request is opened. An issue, access to the
repository, or an entry on Herdr's inherited `.github/APPROVED_CONTRIBUTORS`
list grants no Vqro contribution authority. Herdr's PR Gate is guarded to
`herdrdev/herdr` and does not enforce this Vqro intake policy.

## Owner-authorized issues

Do not open a Vqro issue yet. The inherited bug template requires Herdr
version/channel data and the issue chooser points to Herdr Discussions while
disabling blank issues. A Vqro-compatible template and channel require separate
owner approval. For a specifically authorized report, use the private channel
and format the Vqro owner designates.

- **Authorized bug reports:** describe what you observed, what you
  expected, the shortest exact reproduction, and your Vqro commit or build ID,
  operating system, and terminal.
- **Authorized proposals and questions:** use the owner's designated channel
  and format; do not post unsolicited proposals or link to Herdr Discussions.

Before an authorized report, check the owner's designated records for duplicates.
For changes to product direction, extension contracts, persistence, protocols,
or architecture, read the
[agentic multiplexer direction](docs/next/architecture/agentic-multiplexer-direction.md)
and the [architecture decisions](docs/next/architecture/README.md) the proposal
would affect.

## Owner-authorized pull requests

Open a pull request only for the exact work the Vqro owner explicitly
authorized. An issue does not itself authorize a pull request. Small fixes,
including typos and broken links, are not an exception to owner approval.

- Keep one pull request to one agreed problem. Do not bundle unrelated cleanup or
  refactors.
- Use a lowercase conventional title such as `fix: handle pane focus`, and put
  `refs #<issue-number>` in the commit body when the work relates to an issue.
- Understand your change. Using an agent to write code is fine; you must still be
  able to explain what the change does, how it behaves at the edges, and what its
  tests prove.
- Follow [`AGENTS.md`](AGENTS.md), including its direction, invariants, and
  engineering rules. A change that deviates from a documented invariant needs a
  proposed architecture decision under `docs/next/architecture/`.
- Update the unreleased documentation under `docs/next/` when a change affects
  documented behavior or contracts.

The Vqro owner reviews and alone merges pull requests; the inherited Herdr
`.github/MAINTAINERS` list does not designate Vqro merge authority.

## Validation

The repository hook is optional; install it only with approval for local Git
configuration changes:

```bash
just install-hooks
```

Before requesting review, run formatting and tests relevant to the changed
surface and report exactly what passed, failed, or was not run. For substantive
code changes, `just ci` locally is useful when feasible:

```bash
just ci
```

Automatic paid Ubuntu PR/master advisory smoke follows the owner's standing
budget policy, but it does not override an active budget HOLD or itself authorize
pushing a branch. With already-installed tools, local `just check` is optional
and useful, not a landing gate. Only NEW local tool/SDK installs and additional
paid MANUAL hosted Rust/macOS/Windows/Nix/Collections/ConPTY dispatches need
explicit owner approval. Never report unrun platforms or packaging as passing.
`just check` adds Windows cross-compilation on Linux and macOS; see
[`AGENTS.md`](AGENTS.md#testing) for toolchain requirements, including Zig
0.16.0, and Windows cross-compilation setup. Make sure new tests exercise the
reported failure and would fail without the fix.

## Instructions for coding agents

If you are an agent, read [`AGENTS.md`](AGENTS.md) before taking any repository
action. Open issues or pull requests only for an exact scope the Vqro owner has
explicitly authorized, and follow the owner-directed flow above.
