# Formatted text and a multi-line composer: design — 2026-11-19

Status: design only (no code), for the agent UI (K3, spike-panel-1). Two generic panel primitives, usable by any
extension, each an optional feature with a fallback every base client already renders (D29,
`element-and-view-fallbacks.md`). Three clients implement them: the ratatui TUI, the Python text client and the
OpenTUI trial client.

## 1. Formatted text: `rich` rows (feature `vqro.rich@1`)

**Choice: structured blocks, not a markdown string.** The producer (an extension or a bridge) turns markdown into
a small block tree once; clients only lay it out. A markdown string would need a parser in every client, and three
parsers would disagree on edge cases. Blocks are also safe to bound and easy to test.

```json
{"type": "rich", "id": "m41", "requires": ["vqro.rich@1"], "focusable": true,
 "blocks": [
   {"h": 2, "runs": [{"text": "Plan"}]},
   {"p": [{"text": "Run "}, {"text": "cargo test", "code": true}, {"text": " first, "}, {"text": "then", "em": true}, {"text": " ship."}]},
   {"list": {"ordered": true, "start": 1, "items": [[{"p": [{"text": "build"}]}], [{"p": [{"text": "check"}]}]]}},
   {"code": {"lang": "sh", "text": "cargo test\nscripts/check.sh full"}},
   {"quote": [{"p": [{"text": "quoted"}]}]},
   {"rule": true}],
 "fallback": {"type": "text", "wrap": true, "focusable": true, "text": "Plan\n\nRun cargo test first, then ship.\n1. build\n2. check\n    cargo test\n    scripts/check.sh full\n> quoted"}}
```

- **Blocks:** `p` (runs), `h` (level 1-3, runs), `list` (`ordered`, `start`, `items`: lists of blocks), `code`
  (`lang?`, `text`), `quote` (blocks), `rule`. Unknown block kinds render their plain text if they have one, else
  nothing. Tables are not in v1: producers render them as a `code` block of aligned text.
- **Runs:** `{text, strong?, em?, code?, strike?, link?, style?}`. `style` is a semantic name (D18), never a colour;
  `link` is shown as text (clients may also show the URL dimmed); nothing is opened automatically.
- **Layout** (the same in every client, checked by fixtures): paragraphs and headings word-wrap at the row width by
  display width; list items indent by their marker (`• ` or `1. `), nested lists by two more; quotes prefix `│ `;
  code blocks keep their lines, are not wrapped, indent by two, and truncate with `…` (clients may scroll
  horizontally); one blank line between blocks. Headings use `accent`+bold, code `muted`, quotes `dim`.
- **Fallback:** the producer always fills `fallback` with plain text (the helper below makes both), so base clients
  show a readable `wrap` text row. The Python text client claims the feature and prints the same layout as text.
- **Bounds:** at most 64 KiB encoded per row, nesting depth 4, 4096 runs; producers split longer content into
  several rows. Control characters are removed by the producer and again by clients.
- **Producer helper:** `vqro_display.rich(markdown, width=None) -> (blocks, plain)` in the extension SDK: a small
  CommonMark subset (ATX headings, paragraphs, `*`/`_` emphasis, `**` strong, `` ` `` code, fenced code, `-`/`*`/`1.`
  lists, `>` quotes, `[text](url)`, `---`). Anything else stays literal text. A native client that renders an
  agent's item stream itself (`vqro.agent_items@1`) uses its own parser held to the same subset by the same
  fixtures.
- **Streaming:** a transcript row is republished as it grows (bounded rate, as the sidebar does); selection and
  scroll stay on the row `id`; with `follow` the panel stays at the end.
- **Fixtures:** `rich.json`: markdown → blocks (for producers) and blocks + width → lines with styled spans (for
  renderers).

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
 "modes": {"value": "start", "options": [{"label": "Send", "value": "start"}, {"label": "Steer", "value": "steer"}]},
 "fallback": {"text": "Message Pi r2…", "command": "agents.prompt", "args": {"resource": "r2"}, "input": "Message", "multiline": true}}
```

- **Submit** invokes `command` with `args` plus `text` (and `value`, the chosen mode). `submit: "Enter"` sends on
  Enter, a new line is Alt-Enter or Shift-Enter (Shift-Enter needs an outer terminal that reports it; Alt-Enter
  always works); `submit: "C-Enter"` swaps them for long-form text. The prefix key always works (recovery).
- **Draft:** client-local per view and row `id`; it survives republishes, popups and refocus, and is never sent
  implicitly. Cleared only after a successful reply; on an error it stays and the error is shown. `prefill` fills
  an empty draft only.
- **Editing** (the minimum every client implements, checked by fixtures): insert, Backspace/Delete, Left/Right,
  Up/Down between lines, Home/End of line, paste (bracketed, newlines kept, over `max_bytes` refused whole), Esc
  leaves the composer without clearing it. Focus enters with Enter on the row or a click.
- **Fallback:** an ordinary item that opens the (multi-line or single-line) prompt.
- **Per client:** OpenTUI has native `Textarea`/edit buffers; the TUI gets a small editor of its own (no new
  dependency); the text client takes text after the row number (`\n` for new lines) or a `compose <row>` mode
  that reads lines until a lone `.`.
- **Fixtures:** `composer.json`: key sequences → buffer, cursor and submitted text.

## 3. Declarations, effort, open points

- Manifests list them as `works_with: ["ui/vqro.rich@1", "ui/vqro.composer@1", "ui/vqro.multiline@1"]`.
- Effort, once agreed: about 3 h work + 1 h checks for both (spec, fixtures, three clients, the `rich` helper).
- Questions for K3: (1) Which surfaces need the composer: the agent panel bottom, request answers, both? (2) Should
  transcripts be published as `rich` rows by the agents extension now, before `vqro.agent_items@1` exists? (3) Are
  tool output and terminal-like items `code` blocks, or do they need their own block (for example collapsed
  output with an expand action)? (4) Is 16 KiB a sensible default prompt limit for Pi?
