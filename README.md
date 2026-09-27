# vqro

> **Early-stage, provisionally named fork of [Herdr](https://github.com/herdrdev/herdr). Not ready for use.**

vqro is exploring an open, extensible agentic multiplexer: a terminal-native environment for organizing work among humans, agents, tools, and automation. It is intended to be provider-neutral, independently operable, and not dependent on a proprietary service.

Vqro should do more than host agents: it should help direct work, context, communication, and action among them. Intelligent behavior should be part of the multiplexer's experience, built through replaceable extensions rather than tied to one model or workflow.

## Principles

- **PTYs are canonical for execution and recovery.** Agents remain real, independently interactive and addressable terminal processes—not reconstructed chats or opaque jobs.
- **Defaults are not ceilings.** A coherent built-in experience should rest on composable primitives and replaceable policy.
- **Extensions are first-class.** The useful default experience should be assembled through open, versioned interfaces available to independently developed extensions.
- **Work has structure.** Sessions should help people and agents organize responsibility, communication, evidence, and related terminals without hiding the underlying processes.
- **Coexistence is safe.** vqro and Herdr must not accidentally share mutable state, sockets, protocols, plugins, or update channels.

## Current status

Vqro is early-stage. It is separating from inherited Herdr product assumptions incrementally, so current code and documentation still contain compatibility paths and experiments that should not be read as the final architecture.

Current work is testing whether Vqro can:

1. separate session, runtime, and client presentation boundaries;
2. keep terminal/session integrity and recovery in a neutral host;
3. move product semantics and policy into removable extensions; and
4. expose semantic presentation, typed services, and automation through public versioned contracts.

What exists today:

- an isolated `vqro` binary with its own config, state, sockets, and sessions, so it can run beside an installed Herdr;
- real, recoverable terminal sessions organized into workspaces, tabs, panes, repository-aware Spaces, and pane Collections;
- an experimental extension platform in which Collections and Spaces run as independently packaged, sandboxed WebAssembly components, while legacy executable plugins still run as trusted, unsandboxed same-user code; and
- experimental, explicitly approved service calls between independently installed packages.

[Extension platform status](docs/next/api/extension-platform-status.md) records which contracts are available and which remain experimental. The broader direction—and the questions intentionally left open—is recorded in [Agentic multiplexer direction](docs/next/architecture/agentic-multiplexer-direction.md). Binding implementation decisions live in [architecture decisions](docs/next/architecture/README.md).

## Building

Vqro builds with the Rust toolchain pinned in `rust-toolchain.toml` and requires Zig 0.16.0 for the vendored `libghostty-vt`; set `ZIG` if the correct Zig is not first on `PATH`. The optional full local check also uses `just`, `cargo-nextest`, Bun, and Python 3. Automatic hosted PR/master checks run cheap advisory smoke; full hosted platform and package checks require separate approval.

```bash
cargo build
just check
```

## Upstream foundation

Vqro began as a fork of [Herdr](https://github.com/herdrdev/herdr) and retains its attribution and license. Vqro is not affiliated with or endorsed by the Herdr project. Herdr documentation, releases, installation commands, branding, and service endpoints do not describe Vqro.

Read [`AGENTS.md`](AGENTS.md) before making repository changes and [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening issues or pull requests.

## License

Vqro is licensed under the [Apache License 2.0](LICENSE).
