import { expect, test } from "bun:test";
import extension, { READY, MAX_FACT_BYTES, boundedText, type AdapterV1 } from "./index";

function harness() {
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
    async exec(_bin: string, args: string[]) { calls.push(args); return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "self", execution } } }) }; },
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
  expect(await h.api().deliver("explicit", h.api().snapshot())).toBe("idle");
  expect(h.sent[0].options).toEqual({ deliverAs: "followUp" });
  await h.emit("session_shutdown");
  expect(await h.api().deliver("after shutdown", h.api().snapshot())).toBe("unavailable");
});

test("a busy SDK handoff is reported queued, not as the next run", async () => {
  const h = harness();
  await h.emit("session_start", {}, { isIdle: () => false });
  expect(await h.api().deliver("behind in-flight work", h.api().snapshot())).toBe("busy");
  expect(h.sent[0].options).toEqual({ deliverAs: "followUp" });
});

test("consumer mutation cannot alter the producer's identity or facts", async () => {  const h = harness();
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
  expect(await replacement.api().deliver("stale input", token)).toBe("unavailable");
  expect(replacement.sent).toHaveLength(0);
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
