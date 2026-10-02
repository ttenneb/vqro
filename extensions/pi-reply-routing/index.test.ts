import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import extension, {
  assistantText,
  boundedReplyBytes,
  parseProvenanceLabel,
  promptConfirmsOrigin,
  renderMessage,
  MAILBOX_POLL_MS,
  MAX_REPLY_BYTES,
} from "./index";

const savedEnv = { ...process.env };

function setVqroEnv() {
  process.env.VQRO_ENV = "1";
  process.env.VQRO_SOCKET_PATH = "/private/vqro.sock";
  process.env.VQRO_BIN_PATH = "/private/vqro";
  process.env.VQRO_PANE_ID = "w1:p1";
  delete process.env.HERDR_ENV;
  delete process.env.HERDR_SOCKET_PATH;
  delete process.env.HERDR_CLIENT_SOCKET_PATH;
}

function clearVqroEnv() {
  delete process.env.VQRO_ENV;
  delete process.env.VQRO_SOCKET_PATH;
  delete process.env.VQRO_BIN_PATH;
  delete process.env.VQRO_PANE_ID;
}

interface Harness {
  entries: any[];
  calls: { args: string[] }[];
  sent: string[];
  emit: (name: string, event: any, ctx?: any) => Promise<any[]>;
}

function harness(exec?: (args: string[]) => any): Harness {
  const handlers = new Map<string, ((event: any, ctx: any) => Promise<any>)[]>();
  const entries: any[] = [];
  const calls: { args: string[] }[] = [];
  const sent: string[] = [];
  const pi: any = {
    on(name: string, handler: (event: any, ctx: any) => Promise<any>) {
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
    },
    appendEntry(type: string, data: any) {
      entries.push({ type, data });
    },
    sendUserMessage(text: string) {
      sent.push(text);
    },
    async exec(_bin: string, args: string[]) {
      calls.push({ args });
      if (exec) return exec(args);
      if (args[0] === "agent" && args[1] === "get") {
        return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta", execution: "exec-beta" } } }), stderr: "" };
      }
      return { code: 0, stdout: "", stderr: "" };
    },
  };
  extension(pi);
  return {
    entries,
    calls,
    sent,
    async emit(name, event, ctx = {}) {
      const results: any[] = [];
      for (const handler of handlers.get(name) ?? []) results.push(await handler(event, ctx));
      return results;
    },
  };
}

const turnText = (text: string) => ({ message: { role: "assistant", content: [{ type: "text", text }] } });
const prompts = (h: Harness) => h.calls.filter((call) => call.args[0] === "agent" && call.args[1] === "prompt");

beforeEach(() => {
  setVqroEnv();
});
afterEach(() => {
  process.env = { ...savedEnv };
});

test("provenance labels distinguish agent requests, replies, and human input", () => {
  expect(parseProvenanceLabel("[agent alpha · reply expected] summarise X")).toEqual({
    from: "alpha",
    replyExpected: true,
  });
  expect(parseProvenanceLabel("[agent w2P:pFM] done")).toEqual({
    from: "w2P:pFM",
    replyExpected: false,
  });
  expect(parseProvenanceLabel("[host notice] restarted")).toBeNull();
  expect(parseProvenanceLabel("just a human typing")).toBeNull();
  expect(parseProvenanceLabel("[agentalpha] nope")).toBeNull();
});

test("A2 trusts only the CLI's own successful prompt response for the origin", () => {
  const success = JSON.stringify({ result: { type: "agent_prompted", agent: { name: "alpha", pane_id: "w1:p2" } } });
  expect(promptConfirmsOrigin(success, "alpha")).toBe(true);
  expect(promptConfirmsOrigin(success, "w1:p2")).toBe(true);
  expect(promptConfirmsOrigin(success, "gamma")).toBe(false);
  const error = JSON.stringify({ error: { code: "agent_not_ready", message: "alpha" } });
  expect(promptConfirmsOrigin(error, "alpha")).toBe(false);
  expect(promptConfirmsOrigin('echo "vqro agent prompt alpha"', "alpha")).toBe(false);
  expect(promptConfirmsOrigin("", "alpha")).toBe(false);
  // A mention of alpha in one response must not combine with a send to gamma.
  const combined =
    JSON.stringify({ id: "cli:agent:get", result: { agent: { name: "alpha" } } }) +
    "\n" +
    JSON.stringify({ result: { type: "agent_prompted", agent: { name: "gamma" } } });
  expect(promptConfirmsOrigin(combined, "alpha")).toBe(false);
});

test("reply bytes are capped without splitting a character", () => {
  expect(boundedReplyBytes("short")).toBe("short");
  const long = "a".repeat(MAX_REPLY_BYTES + 10);
  expect(boundedReplyBytes(long).endsWith("bytes]")).toBe(true);
  expect(boundedReplyBytes("🙂".repeat(MAX_REPLY_BYTES)).includes("\uFFFD")).toBe(false);
});

test("assistantText reads only text blocks", () => {
  expect(assistantText(turnText("the answer").message)).toBe("the answer");
  expect(assistantText({ content: [{ type: "toolCall" }, { type: "text", text: "hi" }] })).toBe("hi");
  expect(assistantText({ content: [] })).toBe("");
});

test("an agent-started run routes its final text back with --no-reply-expected", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("turn_end", turnText("here is the summary"));
  await h.emit("agent_settled", {});
  expect(prompts(h)[0]?.args).toEqual(["agent", "prompt", "alpha", "here is the summary", "--no-reply-expected"]);
  expect(h.entries).toEqual([expect.objectContaining({ data: expect.objectContaining({ kind: "replied", from: "alpha" }) })]);
});

test("a no-reply-expected label never expects another reply", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha] thanks" });
  await h.emit("turn_end", turnText("ok"));
  await h.emit("agent_settled", {});
  expect(prompts(h)).toHaveLength(0);
});

test("an empty run records no reply and a volatile label", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("agent_settled", {});
  expect(h.entries[0]?.data).toMatchObject({ kind: "no-reply", from: "alpha" });
  const metadata = h.calls.find((call) => call.args[0] === "pane" && call.args[1] === "report-metadata");
  expect(metadata?.args).toContain("idle=no reply to alpha");
});

test("no run text is not reused across runs", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] one" });
  await h.emit("turn_end", turnText("first answer"));
  await h.emit("agent_settled", {});
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] two" });
  await h.emit("agent_settled", {});
  expect(h.entries.map((entry) => entry.data.kind)).toEqual(["replied", "no-reply"]);
});

test("an empty final turn does not reuse an intermediate turn's text", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] x" });
  await h.emit("turn_end", turnText("intermediate"));
  await h.emit("turn_end", { message: { role: "assistant", content: [{ type: "toolCall" }] } });
  await h.emit("agent_settled", {});
  expect(prompts(h)).toHaveLength(0);
  expect(h.entries[0]?.data).toMatchObject({ kind: "no-reply", from: "alpha" });
});

test("a queued human follow-up cannot claim the run", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("input", { source: "interactive", text: "actually never mind" });
  await h.emit("turn_end", turnText("summary"));
  await h.emit("agent_settled", {});
  expect(prompts(h)[0]?.args[2]).toBe("alpha");
});

test("human and unlabelled input are never routed", async () => {
  const h = harness();
  for (const prompt of ["just asking", "[host notice] restarted"]) {
    await h.emit("before_agent_start", { prompt });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
  }
  expect(prompts(h)).toHaveLength(0);
  expect(h.entries).toHaveLength(0);
});

test("a successful explicit message to the origin suppresses the automatic reply", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  const confirmed = JSON.stringify({ result: { type: "agent_prompted", agent: { name: "alpha" } } });
  await h.emit("tool_result", { toolName: "bash", toolCallId: "t1", content: [{ type: "text", text: confirmed }], isError: false });
  await h.emit("turn_end", turnText("also this"));
  await h.emit("agent_settled", {});
  expect(prompts(h)).toHaveLength(0);
  expect(h.entries[0]?.data).toMatchObject({ kind: "explicit", from: "alpha" });
});

test("A2 does not suppress on a failed send or a mention", async () => {
  for (const output of [
    JSON.stringify({ error: { code: "agent_not_ready", message: "alpha" } }),
    JSON.stringify({ result: { type: "agent_prompted", agent: { name: "gamma" } } }),
    'echo "vqro agent prompt alpha"',
  ]) {
    const h = harness();
    await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] x" });
    await h.emit("tool_result", { toolName: "bash", toolCallId: "t1", content: [{ type: "text", text: output }], isError: false });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
    expect(prompts(h)).toHaveLength(1);
  }
});

test("unknown or self identity fails closed", async () => {
  const unknown = harness(() => ({ code: 1, stdout: "", stderr: "no agent" }));
  await unknown.emit("before_agent_start", { prompt: "[agent alpha · reply expected] x" });
  await unknown.emit("turn_end", turnText("answer"));
  await unknown.emit("agent_settled", {});
  expect(prompts(unknown)).toHaveLength(0);
  expect(unknown.entries[0]?.data).toMatchObject({ kind: "self-unknown", from: "alpha" });

  const self = harness((args) =>
    args[0] === "agent" && args[1] === "get"
      ? { code: 0, stdout: JSON.stringify({ result: { agent: { name: "alpha" } } }), stderr: "" }
      : { code: 0, stdout: "", stderr: "" },
  );
  await self.emit("before_agent_start", { prompt: "[agent alpha · reply expected] note" });
  await self.emit("turn_end", turnText("answer"));
  await self.emit("agent_settled", {});
  expect(prompts(self)).toHaveLength(0);
});

test("routing stays off without the managed Vqro environment", async () => {
  clearVqroEnv();
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("turn_end", turnText("answer"));
  await h.emit("agent_settled", {});
  expect(h.calls).toHaveLength(0);
  expect(h.entries).toHaveLength(0);
});

test("renderMessage names the host-attributed sender and carries the body", () => {
  const agentMessage = { id: "m1", body: "please review", sender: JSON.stringify({ class: "agent", execution: "boot:1:2" }) };
  expect(renderMessage(agentMessage)).toContain("vqro message m1 from agent boot:1:2");
  expect(renderMessage({ id: "m2", body: "x" })).toContain("from an agent");
});

test("a waiting message is claimed into a starting turn", async () => {
  const claimed = [{ id: "m1", body: "please review", sender_address: "alpha" }];
  const h = harness((args) =>
    args[0] === "message" && args[1] === "claim" ? { code: 0, stdout: JSON.stringify({ claimed }), stderr: "" } : { code: 0, stdout: "", stderr: "" },
  );
  const results = await h.emit("before_agent_start", { prompt: "just asking" });
  const injected = results.find((result) => result?.message)?.message;
  expect(injected?.content).toContain("please review");
  expect(h.entries.some((entry) => entry.data.kind === "messages-claimed")).toBe(true);
  expect(prompts(h)).toHaveLength(0);
});

test("activation on: the idle mailbox poll claims and starts a turn", async () => {
  jest.useFakeTimers();
  try {
    const claimed = [{ id: "m1", body: "wake up", sender_address: "alpha" }];
    const h = harness((args) => {
      if (args[0] === "agent" && args[1] === "get") return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta", execution: "exec-beta" } } }), stderr: "" };
      if (args[0] === "message" && args[1] === "list") return { code: 0, stdout: JSON.stringify({ activation: true, messages: [{ id: "m1", state: "waiting", recipient_execution: "exec-beta" }] }), stderr: "" };
      if (args[0] === "message" && args[1] === "claim") return { code: 0, stdout: JSON.stringify({ claimed }), stderr: "" };
      return { code: 0, stdout: "", stderr: "" };
    });
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    for (let i = 0; i < 20; i += 1) await Promise.resolve();
    expect(h.sent.join("\n")).toContain("wake up");  } finally {
    jest.useRealTimers();
  }
});
