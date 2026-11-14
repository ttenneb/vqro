import { expect, jest, test } from "bun:test";
import extension, { READY, MAX_FACT_BYTES, boundedText, type AdapterV1 } from "./index";

function harness(exec?: (args: string[]) => any) {
  process.env.VQRO_ENV = "1"; process.env.VQRO_SOCKET_PATH = "/private/api";
  process.env.VQRO_BIN_PATH = "/private/vqro"; process.env.VQRO_PANE_ID = "w1:p1";
  delete process.env.HERDR_ENV; delete process.env.HERDR_SOCKET_PATH; delete process.env.HERDR_CLIENT_SOCKET_PATH;
  const handlers = new Map<string, any>(), bus = new Map<string, Set<any>>();
  const calls: string[][] = [], sent: any[] = [], facts: any[] = [];
  let adapter: AdapterV1, execution = "execution-a";
  const pi: any = {
    events: {
      on(name: string, fn: any) { const set = bus.get(name) ?? new Set(); set.add(fn); bus.set(name, set); return () => set.delete(fn); },
      emit(name: string, value: any) { for (const listener of [...(bus.get(name) ?? [])]) listener(value); },
    },
    on(name: string, fn: any) { handlers.set(name, fn); },
    async exec(_bin: string, args: string[]) { calls.push(args); if (exec) return exec(args); return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "self", execution } } }) }; },
    sendUserMessage(text: string, options: any) { sent.push({ text, options, kind: "user" }); },
    sendMessage(message: any, options: any) { sent.push({ ...message, options }); },
  };
  pi.events.on(READY, (value: AdapterV1) => { adapter = value; adapter.onFact(fact => { facts.push(fact); }); });
  extension(pi);
  return { calls, sent, facts, api: () => adapter!, execution: (value: string) => { execution = value; },
    emit: (name: string, value = {}, ctx = {}) => handlers.get(name)?.(value, ctx) };
}

test("adapter alone reports neutral facts and only explicit input delivers", async () => {
  const h = harness();
  await h.emit("session_start", {}, { isIdle: () => true });
  await h.emit("input", { source: "rpc", text: "[agent sender] request" });
  await h.emit("before_agent_start", { prompt: "[agent sender] request" });
  await h.emit("turn_end", { message: { content: [{ type: "text", text: "final" }] } });
  await h.emit("agent_settled");
  expect(h.facts.map(f => f.kind)).toEqual(["session_started", "input_observed", "run_started", "turn_ended", "run_settled"]);
  expect(h.facts[2].origin).toEqual({ label: "[agent sender]", trust: "self_declared" });
  expect(h.facts[2].source).toBe("rpc");
  expect(h.facts[4].final).toBe("final");
  expect(h.calls.every(args => args[0] === "agent" && args[1] === "get")).toBe(true);
  expect(h.sent).toHaveLength(0);
  expect(await h.api().deliver("explicit", h.api().snapshot())).toBe(true);
  expect(h.sent[0].options).toEqual({ deliverAs: "followUp" });
  await h.emit("session_shutdown");
  expect(await h.api().deliver("after shutdown", h.api().snapshot())).toBe(false);
});

test("input message starts are bounded neutral observations, not tool or status noise", async () => {
  const h = harness();
  await h.emit("session_start");
  for (const role of ["user", "custom", "assistant", "toolResult"]) {
    await h.emit("message_start", { message: { role, content: "input text" } });
  }
  await h.emit("message_start", { message: { role: "user", content: "x".repeat(MAX_FACT_BYTES + 1) } });
  const inputs = h.facts.filter(f => f.kind === "input_message_started");
  expect(inputs.map(f => f.messageRole)).toEqual(["user", "custom", "user"]);
  expect(inputs[0].text).toBe("input text");
  expect(inputs[1].text_truncated).toBe(false);
  expect(inputs[2].text_truncated).toBe(true);
  expect(Buffer.byteLength(inputs[2].text)).toBe(MAX_FACT_BYTES);
  expect(h.sent).toHaveLength(0);
  await h.emit("session_shutdown");
  await h.emit("message_start", { message: { role: "user", content: "closed" } });
  expect(h.facts.filter(f => f.kind === "input_message_started")).toHaveLength(3);
});

test("aborted and error turns replace earlier final text with no final answer", async () => {
  for (const stopReason of ["aborted", "error"]) {
    const h = harness();
    await h.emit("before_agent_start", { prompt: "request" });
    await h.emit("turn_end", { message: { content: "earlier tool turn" } });
    await h.emit("turn_end", { message: { content: "partial answer", stopReason } });
    await h.emit("agent_settled");
    expect(h.facts.at(-1).final).toBe("");
    await h.emit("session_shutdown");
  }
});

test("consumer mutation cannot alter the producer's identity or facts", async () => {
  const h = harness();
  h.api().onFact(fact => { if (fact.self) fact.self.execution = "forged"; });
  await h.emit("session_start");
  const borrowed = h.api().snapshot(); borrowed.self!.execution = "forged";
  expect(h.api().snapshot().self?.execution).toBe("execution-a");
  expect(h.facts[0].self.execution).toBe("execution-a");
});

test("awaited context claims discarded after shutdown, not injected", async () => {
  const h = harness();
  let release: any, started: any, discarded = 0;
  const ready = new Promise<void>(resolve => { started = resolve; });
  h.api().beforeRun(async () => { started(); await new Promise(resolve => { release = resolve; }); return { message: { customType: "fixture", content: "claimed", display: true }, discard: () => { discarded += 1; } }; });
  const pending = h.emit("before_agent_start", { prompt: "human input" });
  await ready;
  await h.emit("session_shutdown"); release();
  expect(await pending).toBeUndefined(); expect(discarded).toBe(1); expect(h.sent).toHaveLength(0);
});

test("a replacement execution cannot inherit awaited context", async () => {
  const h = harness(); let discarded = 0;
  h.api().beforeRun(async () => { h.execution("replacement"); return { message: { customType: "fixture", content: "old context", display: true }, discard: () => { discarded += 1; } }; });
  expect(await h.emit("before_agent_start", { prompt: "request" })).toBeUndefined();
  expect(discarded).toBe(1);
});

test("new session/runtime cannot accept a retained token", async () => {
  const first = harness(); await first.emit("session_start");
  const token = first.api().snapshot();
  await first.emit("session_start", { reason: "new" });
  expect(first.api().current(token)).toBe(false);
  const replacement = harness(); await replacement.emit("session_start");
  expect(replacement.api().current(token)).toBe(false);
  expect(await replacement.api().deliver("stale input", token)).toBe(false);
  expect(replacement.sent).toHaveLength(0);
});

test("idle identity re-observation shares lifecycle single flight and rejects the prior epoch", async () => {
  jest.useFakeTimers();
  let reads = 0, release: any;
  const h = harness(() => {
    reads += 1;
    if (reads === 1) return { code: 1, stdout: "" };
    if (reads === 2) return new Promise(resolve => { release = () => resolve({ code: 0, stdout: JSON.stringify({ result: { agent: { execution: "old-epoch" } } }) }); });
    return { code: 0, stdout: JSON.stringify({ result: { agent: { execution: "new-epoch" } } }) };
  });
  const flush = async () => { for (let i = 0; i < 30; i += 1) await Promise.resolve(); };
  try {
    await h.emit("session_start");
    jest.advanceTimersByTime(30_000); await flush();
    expect(reads).toBe(2);
    jest.advanceTimersByTime(90_000); await flush();
    expect(reads).toBe(2);
    const next = h.emit("session_start");
    await flush();
    expect(reads).toBe(2);
    release(); await next;
    expect(reads).toBe(3);
    expect(h.api().snapshot().self?.execution).toBe("new-epoch");
    expect(h.sent).toHaveLength(0);
    expect(h.facts.map(fact => fact.kind)).toEqual(["session_started", "session_started"]);
  } finally {
    release?.();
    await h.emit("session_shutdown");
    jest.useRealTimers();
  }
});

test("fast unknown failures stay paced and healthy identity adds no idle probes", async () => {
  jest.useFakeTimers();
  let attempts = 0;
  const unknown = harness(() => ++attempts === 2 ? { code: 0, stdout: "{" } : { code: 1, stdout: "" });
  const healthy = harness();
  const flush = async () => { for (let i = 0; i < 30; i += 1) await Promise.resolve(); };
  try {
    await unknown.emit("session_start"); await healthy.emit("session_start");
    jest.advanceTimersByTime(29_000); await flush(); expect(unknown.calls).toHaveLength(1);
    jest.advanceTimersByTime(1000); await flush(); expect(unknown.calls).toHaveLength(2);
    jest.advanceTimersByTime(29_000); await flush(); expect(unknown.calls).toHaveLength(2);
    jest.advanceTimersByTime(1000); await flush(); expect(unknown.calls).toHaveLength(3);
    expect(healthy.calls).toHaveLength(1);
    expect(unknown.sent).toHaveLength(0);
    expect(unknown.facts.map(fact => fact.kind)).toEqual(["session_started"]);
  } finally {
    await unknown.emit("session_shutdown"); await healthy.emit("session_shutdown");
    jest.useRealTimers();
  }
});

for (const transition of ["run", "shutdown"]) test(`late idle identity cannot cross ${transition}`, async () => {
  jest.useFakeTimers();
  let reads = 0, release: any;
  const h = harness(() => {
    reads += 1;
    if (reads === 1) return { code: 1, stdout: "" };
    if (reads === 2) return new Promise(resolve => { release = () => resolve({ code: 0, stdout: JSON.stringify({ result: { agent: { execution: "old-idle" } } }) }); });
    return { code: 0, stdout: JSON.stringify({ result: { agent: { execution: "new-run" } } }) };
  });
  const flush = async () => { for (let i = 0; i < 30; i += 1) await Promise.resolve(); };
  try {
    await h.emit("session_start");
    jest.advanceTimersByTime(30_000); await flush(); expect(reads).toBe(2);
    const pending = transition === "run" ? h.emit("before_agent_start", { prompt: "human input" }) : h.emit("session_shutdown");
    await flush(); expect(reads).toBe(2);
    release(); await pending; await flush();
    expect(h.api().snapshot().self?.execution).toBe(transition === "run" ? "new-run" : undefined);
    expect(reads).toBe(transition === "run" ? 3 : 2);
    jest.advanceTimersByTime(90_000); await flush();
    expect(reads).toBe(transition === "run" ? 3 : 2);
    expect(h.sent).toHaveLength(0);
  } finally {
    release?.(); await h.emit("session_shutdown"); jest.useRealTimers();
  }
});

test("fresh delivery waits for lifecycle probe then makes its own fresh read", async () => {
  let reads = 0, release: any;
  const h = harness(() => {
    reads += 1;
    if (reads === 2) return new Promise(resolve => { release = () => resolve({ code: 0, stdout: JSON.stringify({ result: { agent: { execution: "original" } } }) }); });
    return { code: 0, stdout: JSON.stringify({ result: { agent: { execution: reads === 1 ? "original" : "replacement" } } }) };
  });
  const flush = async () => { for (let i = 0; i < 30; i += 1) await Promise.resolve(); };
  try {
    await h.emit("session_start");
    const run = h.emit("before_agent_start", { prompt: "input" });
    await flush(); expect(reads).toBe(2);
    const delivery = h.api().deliver("must not use an earlier read", h.api().snapshot());
    await flush(); expect(reads).toBe(2);
    release(); await run;
    expect(await delivery).toBe(false);
    expect(reads).toBe(3);
    expect(h.sent).toHaveLength(0);
  } finally {
    release?.(); await h.emit("session_shutdown");
  }
});

test("final text is replaced even when final turn is empty; fields bounded", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "x".repeat(MAX_FACT_BYTES + 1) });
  expect(h.api().snapshot().prompt_truncated).toBe(true);
  await h.emit("turn_end", { message: { content: "answer" } });
  await h.emit("turn_end", { message: { content: [{ type: "toolCall" }] } });
  await h.emit("agent_settled");
  expect(h.api().snapshot().final).toBe("");
  const value = boundedText("🙂".repeat(MAX_FACT_BYTES));
  expect(value.truncated).toBe(true); expect(Buffer.byteLength(value.text)).toBeLessThanOrEqual(MAX_FACT_BYTES); expect(value.text).not.toContain("\uFFFD");
});
