# Setup on a new machine

> vqro is a working prototype, and fluid: expect these steps to change.

## Run vqro on a new machine

1. **Prerequisites:** Rust (stable), Python 3.11+ (extensions are stdlib-only), git, **Zig 0.17.1**.
   The vendored terminal engine requires that exact Zig version. Use `brew install zig` and check
   `zig version`, or `mise install zig@0.17.1`. Set `ZIG` to the binary, or link the installation to
   `~/.local/zig-0.17.1` (binary: `~/.local/zig-0.17.1/zig`). `scripts/check.sh` preserves `ZIG`.
   The first build needs network to fetch about 18 MB of Zig packages into `~/.cache/vqro/zig-pkg`
   (override with `LIBGHOSTTY_VT_ZIG_PKG_DIR`); subsequent builds can use that cache offline.
2. **herdr-core:** install with its official installer (it verifies sha256 and codesign), e.g.
   `curl -fsSL https://releases.herdr.dev/core/install.sh` and review it before running. The `herdr` provider
   extension (vqro-herdr) finds `hc` on PATH or at herdr-core's standard install (`~/.local/share/herdr-core/current/...`),
   or set JSON argv such as `VQRO_HERDR='["ssh","box","hc","connect"]'` in the environment the core starts from.
   Arguments are literal (no shell/whitespace parsing). `vqro-herdr --hc PATH --hc-arg ARG` also supports literal
   prefix arguments before `connect`; paths/arguments may contain spaces.
3. **Install vqro:** `./scripts/dev-install.sh` (release build of `vqro`, `vqro-herdr` and `vqro-tmux` to
   `~/.local/bin`, extensions to `~/.local/share/vqro/extensions`). Make sure `~/.local/bin` is on PATH.
   Backends run as extensions: `herdr` is on by default; `tmux` (a private tmux server, never your own sessions)
   is off until enabled. Every terminal gets `VQRO_SOCKET`, so `vqro call ...` works from any pane.
4. **Run:** `vqro tui`. The first core start installs the default extensions from `extensions/defaults.json`.
   `vqro status` shows the core, extensions and connected UIs.
5. **Terminal:** Ghostty or WezTerm give Nerd Font icons and the kitty keyboard protocol out of the box; other
   terminals work with plain-text icons (Settings → Interface → Icons) and without live modifier display.
6. **Moving state (optional):** your workspace, theme and keymap live in `~/.local/state/vqro/`; copy
   `workspace.json` across if you want the same setup (resource ids are re-mapped to the new machine's terminals).

Nothing outside this repo moves with you. Reference clones, installed tools and the assistant's local memory all stay
on the old machine, so everything needed is listed here.

## Checks on macOS

`scripts/check.sh quick` and `full` require Bash 4.4+ (namerefs and safe empty arrays under `set -u`);
Apple's Bash 3.2 is too old.
Install a newer Bash, e.g. `brew install bash`, and put it on PATH: the gate re-execs under the
first suitable PATH Bash, or fails clearly before running checks. Use Python 3.11+ for tests.
Portable suites can find Homebrew tmux on PATH. Harnesses pinned to `/usr/bin/tmux`, Linux namespaces,
`/proc` measurements/cleanup and systemd supervision are explicitly **SKIP** on non-Linux, not PASS;
see the step logs and receipt. A passing macOS gate therefore does not certify the Linux-only suites.
The Linux gate retains its existing commands and checks. `timing` mode requires Linux and exits 2
up front elsewhere (an all-SKIP run is not timing validation). macOS still needs native smoke validation.
For development commands that need `timeout`, `brew install coreutils` provides `gtimeout`.

## Offline licence inventory

The `licenses` step in `scripts/check.sh` uses `cargo metadata --offline --locked`, including crates
for platforms other than the current host. If crates are missing from the cache, run
`cargo fetch --locked` once (network), then retry. The licence check itself never fetches packages.

## Reference clones (read-only, into `~/Projects`, depth 1)

| Name | URL | Commit read (2026-11-16) | Why |
|---|---|---|---|
| herdr-core | https://github.com/herdrdev/herdr-core | bb8a2f7 | first backend; `schema/hc.openrpc.json` |
| herdr-app | https://github.com/herdrdev/herdr-app | 2815f59 | `docs/labels.md` conventions |
| rodeo | https://github.com/lukeed/rodeo | d50e3d8 | `herdr-app` doc schema (`crates/doc/src/schema.rs`) |
| rmux | https://github.com/helvesec/rmux | 1f4571e | deferred backend |
| pi | https://github.com/badlogic/pi-mono (now earendil-works/pi) | a9878e1 | RPC docs: `packages/coding-agent/docs/rpc*.md` |
| pi_agent_rust | https://github.com/Dicklesworthstone/pi_agent_rust | 2b4aca22 | ACP + steering extension (licence rider, see open questions) |
| xal | https://github.com/xal-sh/xal | 142fdfd | in-process steer semantics |
| vqro | https://github.com/ttenneb/vqro | | prior attempt; read `docs/next/architecture/lessons-and-motivations.md` |

## Tools

- Rust (stable), for vqro itself.
- `pi` (recent; check RPC `steer` support) and `claude` (Claude Code 2.1.325+), both logged in, for E1.
- Optional for backend work: herdr-core (`hc`) via its official installer (verifies sha256 and codesign), tmux 3.2+.
  Building herdr-core from source needs zig and its vendored ghostty.
- Keep socket paths short: macOS limits Unix socket paths to 103 bytes. Use `~/vqro/.run/` for scratch runtime dirs.

## Old spikes

`spikes/` contains the backend spikes. They have known flaws (`research/review-1-backends.md`, finding 5) and are
kept as evidence, not as code to reuse.

## More reference clones (2026-11-16)

Sparse clones used for `research/workspace-extension-models.md`, in `~/Projects/ref/`: neovim/neovim,
microsoft/vscode, zed-industries/zed, zellij-org/zellij, wezterm/wezterm, kovidgoyal/kitty, wavetermdev/waveterm.
Re-clone at depth 1 if needed; file:line references in that note are relative to them.
