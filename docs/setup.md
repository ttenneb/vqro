# Setup on a new machine

> prim is a working prototype, and fluid: expect these steps to change.

## Run prim on a new machine

1. **Prerequisites:** Rust (stable), Python 3 (extensions are stdlib-only), git.
2. **herdr-core:** install with its official installer (it verifies sha256 and codesign), e.g.
   `curl -fsSL https://releases.herdr.dev/core/install.sh` and review it before running. prim finds `hc` on PATH
   or at herdr-core's standard install (`~/.local/share/herdr-core/current/...`), or set `PRIM_HERDR`.
3. **Install prim:** `./scripts/dev-install.sh` (release build to `~/.local/bin/prim`, extensions to
   `~/.local/share/prim/extensions`). Make sure `~/.local/bin` is on PATH.
4. **Run:** `prim tui`. The first core start installs the default extensions from `extensions/defaults.json`.
   `prim status` shows the core, extensions and connected UIs.
5. **Terminal:** Ghostty or WezTerm give Nerd Font icons and the kitty keyboard protocol out of the box; other
   terminals work with plain-text icons (Settings → Interface → Icons) and without live modifier display.
6. **Moving state (optional):** your workspace, theme and keymap live in `~/.local/state/prim/`; copy
   `workspace.json` across if you want the same setup (resource ids are re-mapped to the new machine's terminals).

Nothing outside this repo moves with you. Reference clones, installed tools and the assistant's local memory all stay
on the old machine, so everything needed is listed here.

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

- Rust (stable), for prim itself.
- `pi` (recent; check RPC `steer` support) and `claude` (Claude Code 2.1.325+), both logged in, for E1.
- Optional for backend work: herdr-core (`hc`) via its official installer (verifies sha256 and codesign), tmux 3.2+.
  Building herdr-core from source needs zig and its vendored ghostty.
- Keep socket paths short: macOS limits Unix socket paths to 103 bytes. Use `~/prim/.run/` for scratch runtime dirs.

## Old spikes

`spikes/` contains the backend spikes. They have known flaws (`research/review-1-backends.md`, finding 5) and are
kept as evidence, not as code to reuse.

## More reference clones (2026-11-16)

Sparse clones used for `research/workspace-extension-models.md`, in `~/Projects/ref/`: neovim/neovim,
microsoft/vscode, zed-industries/zed, zellij-org/zellij, wezterm/wezterm, kovidgoyal/kitty, wavetermdev/waveterm.
Re-clone at depth 1 if needed; file:line references in that note are relative to them.
