# vqro

> **Early-stage, provisionally named fork of [Herdr](https://github.com/herdrdev/herdr). Not ready for use.**

vqro is exploring an open, terminal-native multiplexer for organizing human and agent work. It is intended to remain a community public good: provider-neutral, independently operable, and not dependent on a proprietary service.

## Principles

- **Terminals are canonical.** Agents remain real, independently interactive and addressable PTYs—not reconstructed chats or opaque jobs.
- **Defaults are not ceilings.** A coherent built-in experience should rest on composable primitives and replaceable policy.
- **Extensions are first-class.** Open, versioned interfaces should let external tools participate without unstable access to renderer or runtime internals.
- **Work has structure.** Sessions should organize repositories, checkouts, related terminals, and parallel agents without hiding their underlying capabilities.
- **Coexistence is safe.** vqro and Herdr must not accidentally share mutable state, sockets, protocols, plugins, or update channels.

## Alpha implementation status

The integrated slice is under active review and is not yet merge-ready. The current `feature/integrated-alpha` branch implements:

- an isolated `vqro` binary, config/state directories, sockets, sessions, logs, environment routing, and temporary runtime paths;
- repository-aware **Spaces** that group primary and linked checkouts while preserving ordinary non-Git workspaces;
- pane **Collections** with compact terminal and client-local nested-list presentations, multiple independently interactive inline PTY previews, ordered/delegation-indented members, archive/restore, promotion, persistence, and provenance indicators;
- an on-demand terminal/resource/agent inspector rendered as an embedded pane component—not a global popup—with privacy-safe Linux CPU, memory, thread, and I/O telemetry;
- supervised out-of-process extension services using bounded, versioned JSON framing; and
- constrained declarative pane documents rendered and styled by the native client shell.

A Git repository remains one Space whether it has one checkout or many. The sidebar uses the repository label as that Space’s identity, orders its primary and linked checkouts from the server-owned projection, and shows branch/upstream or detached-head context plus ahead/behind counts without inventing a separate repository-only row. Collapsing a repository keeps its last-focused checkout as the navigation target and aggregates hidden checkout attention; the compact sidebar renders one numbered destination per Space.

Existing Herdr state can be copied non-destructively into a new named vqro session with `vqro session import-herdr [HERDR_SESSION] --name <VQRO_SESSION>`. Omitting the source imports Herdr’s default session. The importer validates the persisted snapshot and restores its workspace, tab, layout, working-directory, repository, Collection, and delegation metadata when the new session starts. It does not transfer live PTY processes, terminal scrollback, or resumable agent-session metadata, preventing a second agent from attaching to the same external conversation. It never overwrites an existing vqro session or modifies the Herdr source.

A collection header’s **list** control switches that client from the compact selected-terminal presentation to the nested list. Rows preserve delegation depth and active/archive status. Click a disclosure glyph or press Space to expand a child, click an inline terminal or press Enter to enter it, and use the configured prefix followed by Esc to return to list mode. `z` maximizes/restores the selected child within the collection; drag the centered `[ drag height ↕ ]` grip or use `+`/`-` to adjust its preview height. `a` archives/restores it, `p` promotes it beside an ordinary pane, and Ctrl+Up/Ctrl+Down reorder it. The collection surface itself does not scroll; mouse-wheel and terminal scrolling apply only to an individually focused inline PTY. Each preview remains a real independently addressable PTY with its own bounded scrollbar, while list geometry and interaction state are client-local.

The bundled reference extension is under [`extensions/terminal-inspector`](extensions/terminal-inspector). Link it with `vqro plugin link ./extensions/terminal-inspector`, restart the vqro server, then use **Inspect terminal** from a pane context menu or the collection header’s **inspect** control. The supervised service returns a bounded declarative contribution; vqro renders it inside the target pane while the rest of the workspace remains interactive.

Extension services are **trusted same-user subprocesses, not sandboxed plugins**. Requested/granted capabilities constrain what the vqro service protocol will perform, but cannot remove the subprocess's ordinary operating-system access. vqro does not inject its API socket by default. The reference extension receives explicit `view_contribution` and `terminal_interrupt` protocol grants; terminal interruption is a host-defined Ctrl-C effect bound to the contribution's exact pane and requires host-rendered user confirmation.

Supervised extension services are supported on Linux, macOS, and Windows. On Windows, vqro creates the child suspended, assigns it to a kill-on-close Job Object, and resumes it only after containment succeeds; setup failures terminate the child and fail closed. These services remain trusted same-user subprocesses rather than sandboxes on every platform.

This remains work-in-progress alpha software: contracts may evolve, final project gates are still pending, remote vqro deployment is not yet a supported release path, and inherited agent-integration install/remove commands are disabled until their assets have a fully isolated vqro namespace. Process-based agent detection remains available. The retained upstream documentation below still describes Herdr.

vqro is not affiliated with or endorsed by the Herdr project. It will retain upstream attribution, track useful upstream work where practical, and treat interoperability as an explicit compatibility contract rather than an assumption.

## Upstream README

The documentation below is retained from the current Herdr baseline for reference. Its installation commands, release links, branding, and service endpoints refer to Herdr—not vqro.

---

# herdr


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="https://herdr.dev/docs/quick-start/">quick start</a> · <a href="https://herdr.dev/docs/">docs</a>
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr/releases"><img src="https://img.shields.io/github/downloads/herdrdev/herdr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/herdrdev/herdr/stargazers"><img src="https://img.shields.io/github/stars/herdrdev/herdr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
  <a href="https://formulae.brew.sh/formula/herdr"><img src="https://img.shields.io/homebrew/v/herdr?label=homebrew&labelColor=333333&color=666666" alt="Homebrew version" /></a>
  <a href="https://x.com/herdrdev"><img src="https://img.shields.io/badge/follow-%40herdrdev-000000?logo=x&logoColor=white" alt="follow @herdrdev on X" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**the runtime your coding agents live on.**

- **detach without stopping work** — herdr keeps terminals running in a background server when you close the client or lose your SSH connection. after a server or machine restart, herdr restores the saved layout and can resume supported agent sessions; the original processes do not survive. [session state →](https://herdr.dev/docs/session-state/)
- **several machines, one window** — keep local work and saved ssh machines together, with a combined agent list and independent reconnects. [remote machines →](https://herdr.dev/docs/connecting-machines/)
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, herdr says so.
- **agent-native** — agents drive herdr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://herdr.dev/docs/agent-skill/)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. herdr doesn't wrap or replace them; it owns their terminals.
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://herdr.dev/plugins/)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or `brew install herdr` · `mise use -g herdr` · windows: `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"` · [endpoint-protected Windows](https://herdr.dev/docs/windows-beta/) · [binaries](https://github.com/herdrdev/herdr/releases)

then start it where the work lives:

```bash
herdr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `herdr` reattaches. [quick start →](https://herdr.dev/docs/quick-start/)

## docs

everything lives at [herdr.dev/docs](https://herdr.dev/docs/): [quick start](https://herdr.dev/docs/quick-start/) · [concepts](https://herdr.dev/docs/concepts/) · [supported agents](https://herdr.dev/docs/agents/) · [keyboard](https://herdr.dev/docs/keyboard/) · [configuration](https://herdr.dev/docs/configuration/) · [session state](https://herdr.dev/docs/session-state/) · [connecting machines](https://herdr.dev/docs/connecting-machines/) · [remote](https://herdr.dev/docs/persistence-remote/) · [integrations](https://herdr.dev/docs/integrations/) · [plugins](https://herdr.dev/docs/plugins/) · [socket api](https://herdr.dev/docs/socket-api/)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

enterprise / partnership: hey@herdr.dev

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/herdrdev/herdr
cd herdr
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Herdr is licensed under the [Apache License 2.0](LICENSE).
