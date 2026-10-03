# Neutral Vqro Pi adapter

`index.ts` bridges Pi SDK lifecycle observations and explicit queued input. It
contains no Tasks, message activation, recipient selection or reply policy.
Consumers register through the SDK-scoped version1 interface described in
[`docs/next/api/pi-adapter-v1.md`](../../docs/next/api/pi-adapter-v1.md).

The bundled Tasks consumer is `../tasks/pi.ts`; its legacy prompt-reply workflow
has a tracked accepted transitional decision. Adapter observations are not
durable messages, authenticated identity, accepted work or evidence of model
understanding. Ordinary host identity/admission checks remain authoritative.

`just pi-reply-routing-test` runs neutral-adapter and moved Tasks policy tests
without a model/server. `just pi-profile-demo` exercises the private actual Pi
resource profile; `just readiness-demo` runs the unchanged guide workflow.
