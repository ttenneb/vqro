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

`vqro-rex server status|stop [--state-dir PATH]` administers only a directory marked by a successful vqro-owned
server start. It refuses an unmarked existing server; use `--rex-socket` for your own server instead (that mode
never creates an ownership marker and cannot be stopped through this command). Do not copy ownership markers
between directories. A marker coordinates vqro; it does not authenticate against another process running as you.

Stop checks a non-empty instance ID, reports the sessions and terminals it observed, and takes the same advisory
lock used by vqro's owned-server creation and attach calls. **Other Rex clients do not honor that lock:** a client
outside vqro attaching or creating terminals after the check would be stopped too. Native stop is unconditional,
so external replacement after the check is still a race; the instance confirmation is not an atomic guarantee.

Only documented CLI exit4 (server unreachable) or Unix missing/refused socket means absent. Permission, config,
other transport failures and invalid status output are unknown: startup refuses to start, and status reports
`running: "unknown"` with a reason rather than `false`. Stop refuses unknown status too.

Other arguments: `--rex BIN` (the `rex` program), `--history LINES` (most history sent in a repaint, default 2000).

## What it does and doesn't do

- Attaching shows the terminal's current screen (and some history), then live output. Colours and modes come along;
  Rex's own default palette does not (vqro's theme stays).
- Program status (OSC 7501) is stored by Rex and reread after event loss; failed or invalid observations retain
  the last known facts. Newly listed blocks are not published until their process lifecycle is observed. Exit
  codes are reported only from a valid integer witness.
- Argv storage is limited to 64 KiB (string bytes plus per-argument metadata); oversized requests are refused before
  creation. Pending creation input remains owned by its spawn until completion; unrelated observation failures cannot
  delete it. Known creation input is published as an argv list; lost metadata, external creation and provider-restart
  rediscovery publish `argv: null` (unknown), using the generic provider contract rather than `data.argv_known`.
- Rex has **no per-terminal environment**: a spawn that asks for `env` is refused (nothing starts).
- Rex takes **text input only** (UTF-8). Other bytes are refused for that stream, never changed.
- Viewing a terminal makes vqro its size owner in Rex (like any Rex client that resizes it).
- After a Rex server restart, Rex restores its terminals with new programs in them; vqro shows them as new
  resources.
