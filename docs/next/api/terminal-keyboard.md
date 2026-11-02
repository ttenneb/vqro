# Terminal keyboard input

The host-owned Ghostty terminal is the single live owner of each pane's keyboard
modes and key encoding. Client host reports are input observations, not permission
to select a pane's mode. Framing completes escape sequences and consumes host
replies before UI shortcuts or pane routing run. The host snapshots live terminal
modes and configures the Ghostty encoder under one terminal lock for a key and
its grouped repeats. FFI key events own their UTF-8 backing until encoding ends.

## Routing and text

UI shortcuts use the produced character when the host supplies text or a shifted
alternate. A bare Shift+/ report is not evidence that slash was typed. Non-Latin
Ctrl chords can use the host's base-layout key evidence; ordinary Latin-layout
shortcuts keep their layout meaning. Literal plus bindings round-trip as `+` or
`ctrl++` as well as named `plus` forms.

Held keys keep a lease on the original input target and identity. Release
matching survives modifier changes; focus loss releases held pane reports rather
than leaving keys stuck. IME/compose text remains a text commit, not fabricated
press/release pairs. Plain text can still activate prefix/navigate shortcuts when
the host is in report-all mode.

Unnegotiated shells retain a small legacy key-normalization policy before Ghostty
encoding: Ctrl+Shift letters produce control bytes, and unsupported Super/Hyper/
Meta chords do not accidentally type their bare key or submit a command. Negotiated
Kitty and modifyOtherKeys modes use the live terminal's encoder semantics. The
handoff-only mode tracker records replay data, not a second live mode authority;
generic handoff continues to restore mode stacks even when a particular deployment
requires a restart.

Native Windows console input uses one record reader with physical identity,
repeat counts, dead keys, OEM/accent and AltGr handling. Local host replies are
never pane input. The frozen `ClientInputEvent` codec is unchanged; this port does
not add a Windows host-theme observation variant to it. Host protocol negotiation
is restored after focus/reconnect/resize without replacing endpoint ownership.

## Windows PTY forwarding

The Windows PTY actor schedules input forwarding on the existing Tokio executor,
not a dedicated blocking forwarding thread per pane. Blocking PTY reader, writer
and control work stays on its existing threads. Queue admission still returns
success, full or closed without waiting for PTY writes; accepted input remains
FIFO, and a command's text/Enter transaction cannot be overtaken by later user
input. Admission is not a receipt that the child executed the command.

Submission parts acknowledge only after the writer flushes. Delays and writer
acknowledgements yield the executor. Once text is committed, abandoning the
caller's acknowledgement does not drop its Enter. Synchronous shutdown waits
only for an Enter already queued to the writer, using a completion that the
writer releases directly even if the async executor cannot run. Closing during
the submission delay still cancels an Enter not yet queued.

## Injected commands on macOS

`pane.send_input` and `agent.start` validate the complete encoded submission
against the terminal's current canonical-line limit before queue admission. An
oversized line, including Enter, is rejected in full before agent metadata is
changed. Multiple short lines and raw-mode input remain supported. This check
cannot account for an earlier unfinished line or guarantee that terminal modes
will remain unchanged while accepted input waits in the queue. Successful
preflight does not bypass handoff/shutdown write gates. Other Unix platforms keep
their existing admission policy.

The raw API remains `vqro.api.v1`, with unchanged envelopes and adapter/package
pins. Terminal endpoint generation and private protocol admission are unchanged
by this keyboard work. Native Ghostty macOS/Fedora and Windows Terminal trials
remain deployment acceptance gates; cross-compilation is not a native input pass.
