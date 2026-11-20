# Formatted text, a multi-line composer and bottom-pinned rows: design — 2026-11-19

Status: design only (no code), for the agent UI (K3, spike-panel-1; mockup `k3-agent-view-mockup.md` section 11).
Generic panel primitives, usable by any
extension, each an optional feature with a fallback every base client already renders (D29,
`element-and-view-fallbacks.md`). Three clients implement them: the ratatui TUI, the Python text client and the
OpenTUI trial client.

## 1. Formatted text: `format: "markdown"` on text rows (feature `vqro.markdown@1`)

Revised after spike-panel-1's mockup (K3 phase 2, section 11). The first draft proposed structured blocks made by
the producer; this one keeps the producer's markdown and fixes the subset instead. Reasons: the fallback needs no
extra field (a client without the feature ignores `format` and shows the raw text, wrapped), producers stay
trivial, and a native transcript renderer (`vqro.agent_items@1`) has to parse item text anyway.

```json
{"type": "text", "wrap": true, "focusable": true, "format": "markdown", "id": "m41",
 "text": "## Plan\nRun `cargo test` first, then **ship**:\n1. build\n2. check\n```sh\ncargo test\n```"}
```

- **Subset (fixed, small):** ATX headings `#` to `###`; paragraphs; `-`/`*` bullet and `1.` numbered lists (nested
  by indenting two or more spaces); `**bold**`; `` `inline code` ``; fenced code blocks (```` ``` ```` with an
  optional language word). Everything else, including HTML, links, images, tables, emphasis with one `*`, quotes,
  is shown literally. No feature is optional inside the subset, so every claiming client renders the same.
- **Layout** (the same in every client, checked by fixtures): paragraphs and headings word-wrap at the row width
  by display width; headings use `accent` + bold; bold runs bold; inline code `muted`; list items indent by their
  marker (`• `, `1. `), nested lists by two more; code blocks keep their lines, are not wrapped, indent by two,
  truncate with `…` (clients may scroll sideways) and use `muted`. Styles stay semantic (D18).
- **No `requires`:** the field degrades by itself. Clients claim `vqro.markdown@1` in `hello` so Settings and
  `client.list` can say who renders it.
- **Bounds:** the row's text at most 64 KiB; producers split longer transcripts into rows (one per message).
  Control characters other than newline are removed by clients.
- **Streaming:** a growing message row is republished at a bounded rate; selection and scroll stay on its `id`;
  `follow` keeps the panel at the end.
- **Fixtures:** `markdown.json`: text + width → lines of styled spans, the same for the TUI, the Python text client
  (which prints the layout as text) and the OpenTUI client.

## 2. Multi-line input

Two shapes, both degrading to today's single-line prompt.

**a. A multi-line prompt** (feature `vqro.multiline@1`): any item that has `input` may add `multiline: true` and
`max_bytes` (default 16 KiB, at most 64 KiB). Clients with the feature open a multi-line editor for the prompt;
others ignore the field and open their usual one-line prompt, which still works for one-line answers. This also
covers agent-v0 §5 `text` requests with `multiline`.

**b. An inline composer row** (feature `vqro.composer@1`): a chat-style box that stays in the panel, for example at
the bottom of an agent panel:

```json
{"type": "composer", "id": "prompt-r2", "requires": ["vqro.composer@1"], "command": "agents.prompt", "args": {"resource": "r2"},
 "placeholder": "Message Pi r2", "rows": 3, "max_bytes": 16384, "submit": "Enter",
 "controls": [{"type": "select", "text": "To", "arg": "to", "value": "r2", "options": [{"label": "Pi r2", "value": "r2"}]},
              {"type": "chips", "arg": "mode", "chips": [{"text": "auto", "value": "auto", "checked": true}, {"text": "steer", "value": "steer"}]},
              {"type": "select", "text": "Priority", "arg": "priority", "value": "normal", "options": [{"label": "normal", "value": "normal"}, {"label": "high", "value": "high"}]}],
 "fallback": {"text": "Message Pi r2…", "command": "agents.prompt", "args": {"resource": "r2"}, "input": "Message", "multiline": true}}
```

- **Controls:** `controls` are ordinary `select` and `chips` rows drawn under the text with a Send button; each
  has an `arg` name, and its current value (a select's `value`, a chips row's checked chip) is added to the
  submitted args under that name. Changing a control is client-local until Send (no command per click). Only the
  choices the producer lists are shown (for example only the modes an agent declares).
- **Submit** invokes `command` with `args`, the controls' values and `text`. `submit: "Enter"` sends on Enter, and a
  new line is Alt-Enter, Ctrl-J or Shift-Enter (Shift-Enter needs an outer terminal that reports it; Alt-Enter and
  Ctrl-J always work); `submit: "C-Enter"` swaps them for long-form text (the human's open question in the mockup
  picks the default). The box grows to `rows` lines (default a third of the pane) and then scrolls. The prefix key
  always works (recovery).
- **Draft:** client-local per view and row `id`; it survives republishes, popups and refocus, and is never sent
  implicitly. Cleared only after a successful reply; on an error it stays and the error is shown. `prefill` fills
  an empty draft only.
- **Editing** (the minimum every client implements, checked by fixtures): insert, Backspace/Delete, Left/Right,
  Up/Down between lines, Home/End of line, paste (bracketed, newlines kept, over `max_bytes` refused whole), Esc
  leaves the composer without clearing it. Focus enters with Enter on the row or a click.
- **Fallback:** an ordinary item that opens the (multi-line or single-line) prompt; the controls' choices go in its
  `menu` (or as separate rows), as in the mockup's section 8.
- **Per client:** OpenTUI has native `Textarea`/edit buffers; the TUI gets a small editor of its own (no new
  dependency); the text client takes text after the row number (`\n` for new lines) or a `compose <row>` mode
  that reads lines until a lone `.`.
- **Fixtures:** `composer.json`: key sequences → buffer, cursor and submitted text.

## 3. Rows pinned to the bottom: `pin: "bottom"` (feature `vqro.pin@1`)

For the request card, the messages bar and the facts footer. Rather than a separate `footer: [row]` array (which a
client without the feature would silently drop, unless the producer duplicated the rows), a top-level row in
`items` carries `pin: "bottom"`. Clients with the feature draw those rows, in order, below the scrolling list and
above the composer, never scrolled away; clients without it ignore the field and show the rows where they are,
so producers put them at the end of `items` and the fallback is exactly the mockup's "rows at the end of the list".
Pinned rows stay selectable and keep their ids; filtering never hides them (like the toolbar). A composer row may be
pinned too. The existing `toolbar` stays as it is.

## 4. Declarations, effort, open points

- Manifests list them as `works_with: ["ui/vqro.markdown@1", "ui/vqro.composer@1", "ui/vqro.multiline@1",
  "ui/vqro.pin@1"]`.
- Effort, once agreed: about 3.5 h work + 1 h checks for all of them (spec, fixtures, three clients).
- Questions for K3: (1) Which surfaces need the composer: the agent panel bottom, request answers, both? (2) Should
  transcripts be published as `rich` rows by the agents extension now, before `vqro.agent_items@1` exists? (3) Are
  tool output and terminal-like items `code` blocks, or do they need their own block (for example collapsed
  output with an expand action)? (4) Is 16 KiB a sensible default prompt limit for Pi?
