# `vqro.service.v1`

Status: M1b runtime-neutral dispatch foundation. Process protocol 2 is the only executable backend; component execution is intentionally deferred.

Every runtime is bound to an exact package, namespace, service ID, and nonzero generation. The dispatcher validates this identity on startup and invocation, enforces deadlines and cancellation, checks bounded canonical frames, records auditable request metadata, and rejects stale generations before transport I/O.

The runtime interface has backend-neutral start, invoke, event-delivery, and stop semantics. Lifecycle states and restart policy remain shared above the process backend. Existing manifest-v1 and bundled process services enter through a named legacy compatibility factory and retain process protocol 2 behavior.

The generated [`vqro-service-v1.schema.json`](vqro-service-v1.schema.json) defines request, event, host-call, host-response, identity, and normalized error envelopes. Host calls carry generation, call ID, depth, capability, method, namespace, and parameters. Depth is limited to one, process transport permits one outstanding host call and eight total host calls per invocation, and queues and frames are bounded. The dispatcher performs capability and identity admission, but all neutral host-call methods return `unsupported_before_m2`; no state, terminal, relationship, evidence, container, or managed-file authority is granted in M1b.

Installed process packages execute only a verified package-relative payload selected by the manifest for the current target. The host revalidates the immutable archive on every start, copies the checksum-bound payload into unpredictable private staging, and uses that canonical absolute copy with fixed argv, no shell command or PATH lookup, an immutable package working directory, and a cleared environment containing only explicit non-secret Vqro metadata. Native processes remain trusted same-user code rather than sandboxes. Unix process groups contain descendants that remain in the group; Windows kill-on-close Job Objects contain the process tree. Both are retired on cancellation, deadlines, detected crashes, and drop.

Process protocol limits remain 256 KiB frames, a three-second handshake, a 750 ms default request deadline, sequential requests, bounded writer/frame queues, bounded stderr lines/rate, and the existing three-restart backoff policy. No runtime request occurs from rendering or per-frame client work.
