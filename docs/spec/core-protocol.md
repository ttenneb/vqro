# prim core protocol, v0

> **Status:** this is the living protocol of a **working prototype, and fluid**: it documents what the code does
> today and will change as we learn. Nothing here is frozen yet (`../roadmap.md`, step 5).

Status: implemented in slice 1 (`src/server.rs`), not frozen (D16). JSON-RPC 2.0, one message per line, over a
local socket (`PRIM_SOCKET`, default `~/.local/state/prim/prim.sock`, mode 0600). Both sides may send requests:
the core forwards `command.invoke` to the connection that registered the command. Errors carry a stable
`data.kind`: `invalid`, `conflict`, `not_found`, `unavailable`, `forbidden`, `backend`, `disconnected`.

## Session

| Method | Params | Result |
|---|---|---|
| `hello` | `client?` or `extension?` (name) | `server`, `version`, `protocol`, `you` (author id), `seq`, `safe_mode` |

Every change is recorded with the connection's author (`ext:<name>`, `<client>:<n>`).

## Workspace (prim-owned state, D15)

| Method | Params | Result |
|---|---|---|
| `workspace.snapshot` | | `seq`, `entries` (key → `{version, value}`), `resources`, `commands`, `extensions`, `adapters`, `safe_mode` |
| `workspace.apply` | `ops: [{op: "set", key, value, expect?} \| {op: "delete", key, expect?}]`, `note?` | `txn`, `changes` |
| `workspace.history` | `limit?` | `txns` (newest first) |
| `workspace.revert` | `txn` | the reverting `txn` |

Transactions are validated against the resulting state and applied all-or-nothing. `expect` is compare-and-set
(0 = must not exist). Key namespaces and value shapes: `src/store.rs` header. Layouts may only reference
existing views; `workspace/active` must name an existing layout.

## Events

| Method | Params | Result |
|---|---|---|
| `events.subscribe` | `from?` (seq) | `seq` |

Then notifications `event {seq, type, data}` with types `entries`, `resource`, `resource.removed`, `command`,
`extension`, `adapter`, `safe_mode`; or `missed` when the bounded log can't serve the cursor (re-snapshot).

## Resources (backend-owned, D14)

| Method | Params | Result |
|---|---|---|
| `resource.list` | | `resources` |
| `resource.spawn` | `argv?` (default `$SHELL`), `cwd?`, `name?`, `adapter?` | `resource` |
| `resource.kill` | `resource` | `outcome`, `backend` |
| `resource.attach` | `resource`, `cols`, `rows`, `history?` (scrollback lines to replay) | `stream` |
| `stream.input` | `stream`, `data` (base64) or `text` | `outcome: dispatched` (also accepted as a notification) |
| `stream.resize` | `stream`, `cols`, `rows` | |
| `stream.close` | `stream` | |

Streams push `stream.data {stream, data}` and `stream.closed {stream, reason}`. Stream messages are handled in
order. Resource ids (`r1`, ...) are prim's and stay stable across core restarts while the backend keeps the
resource.

## Commands

| Method | Params | Result |
|---|---|---|
| `command.register` | `name`, `title?` | the command; `prim.*` is reserved for the core |
| `command.list` | | `commands` |
| `command.invoke` | `name`, `args?` | whatever the owner returns; `unavailable` if its owner is disconnected |

Built-in, non-replaceable commands (recovery surface, D15): `prim.layout.reset`, `prim.revert`
(default: last change not made by an extension), `prim.safe_mode`, `prim.extension.disable|enable|restart`.
Everything opinionated is an extension (D17).

## Contributions (D17)

Extensions contribute `menu/*` and `binding/*` entries carrying `owner`; clients hide them while that extension is
disabled, removed or in safe mode, or when their command is unavailable.

- `menu/<id>`: `{where: "pane"|"tab"|"tabbar"|"status", label, icon?, command, args?, input?, pick?: "resource", when?,
  order?}`. `icon` (see Icons) is drawn before the label; an empty label shows the icon alone. `input` makes the client prompt for text (passed as `args.text`); `pick` makes it offer a choice (`args.picked`);
  `when` is matched field by field against the invocation context.
- `binding/<id>`: `{key, command, args?, where?, label?, priority?, input?, pick?, hidden?}`. `key` is a prefix key
  (`"n"`, `"Left"`, see Keys) or a gesture (`"mouse.right"`, `"mouse.middle"`) with `where` = `"pane"` or `"tab"`.
  `input` and `pick` work as for menu items; `hidden` keeps a key out of the prefix menu (it still works).
- Clients add `args.context` when invoking from a place: `{layout, layout_version, path, view, kind, resource, live,
  title}` for a pane, `{layout, layout_version, tab_path, tab_index}` for a tab. Use `layout_version` as `expect`.
- Client-side commands (mechanism, never sent to the core): `client.menu {where}`, `client.prefix`, `client.palette`,
  `client.zoom` (toggle: the focused pane fills this window; per window, not workspace state; moving focus leaves it).
- More client-side commands, so bindings can reach what the prefix keys do: `client.focus {dir: next|prev|left|right|
  up|down}` (directions pick the adjacent pane), `client.tab {to: next|prev|<n>}` (n counts from 1, in the tab group
  holding the focused pane), `client.quit` (this UI only), `client.none` (does nothing: binds a key to nothing). A
  client answers an unknown `client.*` command with a message and never sends it to the core.

## Keys

- **Prefix.** `doc/settings/tui.prefix` names the prefix key (default `C-g`). It must have Ctrl or Alt, or be a
  function key; anything missing, unparsable or unsuitable means `C-g`, never "no prefix". Clients apply it live,
  show it in their prompt, and send it to the program when it is pressed twice.
- **Key names** (tmux-like): modifiers `C-` (Ctrl), `M-` (Alt), `S-` (Shift; named keys only, since a character
  already carries Shift: `O`, `%`), in any order, then a character or one of `Up Down Left Right Home End PageUp
  PageDown Insert Delete Tab BackTab Enter Esc Backspace Space F1`..`F12`. Matching is case-insensitive for names
  and modifiers (`C-space` = `C-Space`), exact for characters. A key that doesn't parse never matches.
- **Precedence.** When several visible bindings share a key (or a gesture and `where`), the highest `priority`
  wins (an integer, default 0), then the client's own defaults, then the lowest entry id. A winning binding to
  `client.none` unbinds the key, so a profile can remove a key without deleting another extension's entry.
- **Client defaults** are bindings at priority 0: `:` `client.palette`, `o`/`O` `client.focus` next/prev, `1`-`9`
  `client.tab`, `u` `prim.revert`, `R` `prim.layout.reset`, `!` `prim.safe_mode`, `q` `client.quit`.
- **Recovery survives any keymap.** The prefix menu (prefix key, the clickable `prim` badge, or `client.prefix`)
  lists the winning binding of each key, then any recovery action (commands, undo, reset layout, safe mode, quit)
  that no key reaches any more, without a key. Safe mode hides every extension's bindings.
- **Live feedback.** While the prefix menu is open, and only then, a client may ask the terminal for richer key
  reports (the TUI uses the kitty keyboard protocol when supported) to show held modifiers in the prompt; after
  a key runs it echoes it (`C-g z → zoom pane`). Held modifiers display as dimmed words (`Shift-`, `Ctrl-`,
  `Alt-`); key names also accept these long forms (`Ctrl-a`, `Shift-Tab`).

The bundled `keys` extension owns keymap profiles (`prim`, `tmux`, `screen`; tables and gaps in
`extensions/keys.py`). Applying one with `keys.profile {value}` replaces its `binding/keys.*` entries (priority 10)
and sets the prefix; the active profile is `doc/keys/profile`, and the list is live data `keys`
(`{profile, profiles: [{label, value}]}`) for the settings page.

## Live view content and popups

- `view.publish {view, content}`: live content merged over a view's entry (e.g. `{"items": [...]}`). Not saved,
  not in history; broadcast as event `view.content`; cleared when the publishing connection disconnects, so a
  panel falls back to its entry (typically "not running"). Snapshots include `live`.
- A command's reply may ask the invoking client for UI: `{"client": {"popup": {"view", "width"?}}}` or
  `{"client": {"close": true}}`. Per client and temporary; clients refuse terminal views as popups.
- Panels: `{title?, items: [row]}` or `{title?, pages: [{title, items}]}`. Rows:
  `{type?: "item"|"toggle"|"header"|"separator"|"text", text, id?, detail?, detail_style?, description?, checked?,
  style?, disabled?, badges?: [{text, style?}], children?: [row], expanded?, command?, args?, input?, pick?,
  menu?: [menu item], icon?: an icon (see Icons) before the text, in the icon column, icon_style?}`, plus `chips`
  rows (`{type: "chips", chips: [{text, icon?, checked?, style?, command, args?, input?, options?}]}`, wrapping; a
  chip with an icon and empty text is the icon alone) and `select` rows (`{type: "select", text, value, options: [{label, value}], command}`);
  anything with `options` opens a pick-one menu and adds `value` to its args; an option with `input` (e.g.
  "custom…") prompts for text instead, passed as `text`. `toolbar: [row]` pins rows above the
  list. Clients handle `client.panel {action: expand_all|collapse_all|filter|clear_filter}` for the panel it came
  from. Panels list needed features in `requires`. Unknown types render as plain items. Selection, expansion, page, filter and scroll are client
  state. Styles are semantic names resolved through the theme (below): `bold`, `dim`, `muted`, `warn`, `error`, `ok`,
  `accent`, `plain`; unknown names render plain. A badge may also carry `color` (a theme colour value) to show a
  colour as data, e.g. a swatch (feature `badge_color`; clients without it show a neutral badge). Use styles for
  everything else, so extensions follow the theme.
- `ui.notify {text, style?}`: a transient message every client shows briefly (event `notify`). Not state.
- Status-bar menu items may name a live `view`; clients take the label, icon and style from its content
  (`{label, icon?, style}`), hiding it while the label is empty.

## Themes

The active theme is the entry `doc/settings/theme` = `{name, palette: {token: colour}}`. Clients read it live and
draw their own chrome with it; terminal content keeps the colours its program set. Every client has a built-in
default palette, so it looks right with no theme and no extensions. A token the theme leaves out, or whose colour
the client can't read, keeps its default; unknown tokens are ignored (a theme may carry tokens for other clients).
An empty palette means the client's own default. In safe mode clients ignore the theme (recovery, D15).

Colours: a name (the 16 ANSI names such as `cyan`, `darkgray`, `lightblue`), `#RRGGBB`, a 256-colour index
(number or numeric string), or `default` for the terminal's own colour.

| Token | Used for | TUI default |
|---|---|---|
| `text` | ordinary text; `plain` and `bold` | `default` |
| `muted` | secondary text: inactive tabs and pages, labels, `text` rows, status message; `muted` style | `gray` |
| `dim` | details, descriptions, disabled rows, hints; `dim` style | `darkgray` |
| `accent` | headers, values, buttons, scrollbar thumb, open page; `accent` style | `cyan` |
| `ok`, `warn`, `error` | the matching styles, badges and toasts | `green`, `yellow`, `red` |
| `prompt` | keyboard modes: prompt line, open prim badge, filter | `magenta` |
| `border` | unfocused borders, rules, separators, scrollbar track | `darkgray` |
| `border_focus` | the focused pane, popups and menus | `cyan` |
| `selection`, `selection_text` | the selected row; unset `selection` means reverse video | unset |
| `badge`, `badge_text` | neutral badge and unchecked chip fill; text on any filled background | `darkgray`, `black` |
| `surface` | fill of plain chips | `#282830` |
| `tab_active`, `tab_active_text` | the active tab | `blue`, `white` |
| `tab_bar` | background of the whole tab bar row | `235` |

The bundled `themes` extension publishes its themes as live data `themes` and applies one with
`themes.apply {name}`; settings shows them on its Theme page.

## Icons

Extensions ask for icons by name; the active icon set says how each name looks (D20). An icon (row `icon`, chip
`icon`, menu item `icon`, a status segment's live `icon`) is one of:

- a string: one or two characters, drawn as is;
- `{name?, nerd?, text?}`: `name` is an icon name, or a list of names tried most specific first (an agent that
  is working: `["working", "agent"]`); `nerd` and `text` are the extension's own glyphs.

The active set is the entry `doc/settings/icons` = `{name, width?, glyphs: {name: glyph}}`. Clients resolve an
icon to: the set's glyph for the first name it has; else the icon's own glyph, `nerd` or `text` by the client's
`icons` setting (`doc/settings/tui.icons`: `auto`, `nerd`, `text`; fonts can't be detected, so `auto` trusts
terminals that bundle Nerd Font symbols, Ghostty and WezTerm, or `PRIM_NERD_FONT=1`; `nerd` falls back to `text`);
else nothing. With no set, or an empty `glyphs`, every icon looks as the extension drew it. Unknown names, and
glyphs a client can't use (empty, control characters, wider than two cells), fall back the same way, so sets and
extensions can evolve independently. In safe mode clients ignore the set (recovery, D15).

Width is in terminal cells by Unicode width (UAX #11, as `unicode-width` measures it): most Unicode and Nerd Font
glyphs take 1, emoji 2. A set's glyphs share one icon column of `width` cells (1 or 2; default 1), widened to its
widest glyph; clients pad narrower glyphs to it and put one space after, so text after icons lines up across a list.
An own glyph wider than the column widens only its own row. Inline places (status bar, tab bar, chips) need no
column: the glyph, a space, the label. Panels whose rows rely on named icons, or icons on chips, list
`icon_names` in `requires`.

| Name | Marks |
|---|---|
| `repo`, `folder` | a workspace that is a git repository, or a plain folder |
| `worktree` | a git worktree of a workspace |
| `terminal`, `agent` | a terminal; one whose program reports agent status (OSC 7501) |
| `working`, `blocked`, `done`, `error` | an agent's reported state (tried before `agent`); `blocked` also counts waiting agents |
| `add`, `filter`, `refresh` | actions: add (a tab, a workspace), filter a list, refresh derived state |
| `settings`, `sidebar` | the buttons that open them |

The names are an open set: an extension may use others, and a set may leave any out. The bundled `icons`
extension publishes its sets as live data `icons` (`{data: [{name, title, description, credit, width, glyphs}],
preview, names}`) and applies one with `icons.apply {name}`; settings shows them, with the `icons` setting, on its
Icons page.

## Default extensions

On first run the core declares the extensions listed in `<extensions dir>/defaults.json` (`PRIM_EXTENSIONS_DIR`,
default `~/.local/share/prim/extensions`) and records `doc/prim/defaults`, so removing one sticks. Bundled:
`tabs`, `panes`, `gestures`, `workspaces`, `sidebar`, `themes`, `icons`, `keys`, `settings` (`extensions/`, helper
`prim_ext.py`). Client options live in `doc/settings/tui`, the theme in `doc/settings/theme`, the icon set in
`doc/settings/icons`.

## Extensions

| Method | Result |
|---|---|
| `extension.list` | per extension: `status` (running, restarting, failed, disabled, safe_mode, stopped), `pid`, `crashes`, `last_exit` |

Declared by `extension/<name>` entries `{argv, enabled?, cwd?}`. Started with `PRIM_SOCKET` and
`PRIM_EXTENSION`; output goes to `<state>/ext-<name>.log`. Crash budget: 3 exits in 5 minutes, then `failed`
until the entry changes or `prim.extension.restart`.
