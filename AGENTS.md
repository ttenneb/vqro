# Working on prim

Read `README.md` first, then `docs/decisions.md`. These rules hold for every change:

- **No policy in core (D0).** If a change decides *when* or *how* something is delivered, waited on, or notified, it
  belongs in an extension. When unsure, ask.
- **The center (D14).** prim is one lean, standardized primitives layer over any multiplexer and agent runtime.
  Coordination is workspace behaviour, not a separate system. Judge every change by: lean, standardized, and does it
  keep adding a backend or runtime quick? Could an agent make this change using only public primitives?
- **Backends own execution (D14).** prim hosts no processes and builds no persistence or lifetime guarantees. Adapters
  declare capabilities and never compensate for missing ones.
- **Report, don't promise (D10).** Effects report what the layer below said happened.
- **Platform-neutral contracts (D12).** No Unix-only concepts in a contract.
- **Capabilities, not modes (D4).** Agent adapters report what the runtime natively does; they never emulate.
- **No replay.** Nothing may resend input to an agent after a restart.
- **Evidence over assumption.** Claims about backends or runtimes cite a file:line, URL, or experiment result. Mark
  anything unverified. Record new findings in `docs/research/`, binding choices in `docs/decisions.md`.
- **Two implementations before calling a contract stable.**
- **Never write into the `herdr-app` document**; prim uses its own `prim` document.
- Keep Unix socket paths under 103 bytes (macOS).
- One trunk, short branches. Commit only when asked.
