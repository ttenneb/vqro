# Terminal input transport declarations — 2026-11-17

The Pi E2E exposed native herdr advertising `terminal.input` while generic `resource.invoke` rejected it in the
adapter. Schema evidence: hc0.1.21 has no literal raw-input RPC. `terminal.send` adds Enter/quiet-time policy;
`terminal.keys` waits too. Raw DATA belongs to an attached channel. Inventing a temporary attach would also risk
resizing a human terminal, so the coordinator chose truthful transport declarations rather than emulation.

Native herdr/fake and current wire herdr/tmux/Python fake now declare `terminal.input {stream:true,effect:false}`.
The generic effect entry point rejects any capability's explicit `effect:false` before dispatch, with
`unsupported` and definite rejected outcome. Explicit input `stream:false` independently rejects stream input.
Missing flags retain legacy handler behaviour. No input is replayed and no Enter, keys or attachment is invented.

`tests/native_input.rs` proves native fake and a private framed herdr fixture reject the unsupported effect while
literal EOT still traverses the attached stream. A raw wire fixture proves the flag is generic, no request reaches
the provider when forbidden, effect:true still forwards with an unchanged dispatched outcome, and stream:false
rejects before notification. The initial two native tests failed backend versus unsupported before the guard.

Logs: `<private-notes>,focused,final,tmux}.log`.
Full suite and explicit private prim-tmux socket E2E pass; no real hc, live core, Pi/model call or installation.
