# Rex backend (optional)

`vqro-rex` makes [Rex](https://www.superlogical.com/rex), Superlogical's terminal multiplexer, a vqro backend like
herdr-core and tmux. It is **not** one of vqro's default extensions: Rex is proprietary alpha software that you
install yourself, and vqro only talks to its local server socket. Details and evidence: `docs/research/rex.md`.

## Turn it on

With Rex installed (`rex version` works) and vqro running:

```sh
vqro call workspace.apply '{"ops":[{"op":"set","key":"extension/rex","value":{"argv":["vqro-rex","--name","rex"],"kind":"provider","manifest":"optional/rex/rex.json"}}],"note":"Rex backend"}'
```

It then appears in Settings → Extensions, where it can be turned off and on again. To remove it:
`vqro call workspace.apply '{"ops":[{"op":"delete","key":"extension/rex"}]}'`.

## Which Rex server

- **Default: a vqro-owned server** under vqro's state directory (`<state>/rex/server.sock`), started the first time
  the provider runs. It is separate from your own Rex sessions, outlives the provider and vqro's core, and stops
  only with `vqro-rex server stop` (which asks for the server's instance first).
- **Your own server:** add `"--rex-socket","/run/user/<uid>/rex/server.sock"` to `argv`. Your existing sessions
  and terminals appear in vqro; vqro never starts or stops that server. New terminals go to a Rex session labelled
  `vqro` (`--session NAME` to change it).

Other arguments: `--rex BIN` (the `rex` program), `--history LINES` (most history sent in a repaint, default 2000).

## What it does and doesn't do

- Attaching shows the terminal's current screen (and some history), then live output. Colours and modes come along;
  Rex's own default palette does not (vqro's theme stays).
- Status reported by programs (OSC 7501) is stored by Rex, so it is never lost; exit codes are reported.
- Rex has **no per-terminal environment**: a spawn that asks for `env` is refused (nothing starts).
- Rex takes **text input only** (UTF-8). Other bytes are refused for that stream, never changed.
- Viewing a terminal makes vqro its size owner in Rex (like any Rex client that resizes it).
- After a Rex server restart, Rex restores its terminals with new programs in them; vqro shows them as new
  resources.
