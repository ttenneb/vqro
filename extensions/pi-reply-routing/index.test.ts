import { afterEach, beforeEach, expect, test } from "bun:test";
import extension, {
  boundedReplyBytes,
  finalAssistantText,
  parseProvenanceLabel,
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
  pi: any;
  entries: any[];
  calls: { args: string[] }[];
  emit: (name: string, event: any, ctx?: any) => Promise<void>;
}

function harness(overrides: Record<string, any> = {}): Harness {
  const handlers = new Map<string, ((event: any, ctx: any) => Promise<any>)[]>();
  const entries: any[] = [];
  const calls: { args: string[] }[] = [];
  const pi: any = {
    on(name: string, handler: (event: any, ctx: any) => Promise<any>) {
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
    },
    appendEntry(type: string, data: any) {
      entries.push({ type, data });
    },
    async exec(_bin: string, args: string[]) {
      calls.push({ args });
      return overrides.exec ? overrides.exec(args) : { code: 0, stdout: "", stderr: "" };
    },
  };
  extension(pi);
  return {
    pi,
    entries,
    calls,
    async emit(name, event, ctx = {}) {
      for (const handler of handlers.get(name) ?? []) await handler(event, ctx);
    },
  };
}

function assistantCtx(text: string) {
  return {
    sessionManager: {
      getBranch: () => [{ type: "message", message: { role: "assistant", content: [{ type: "text", text }] } }],
    },
  };
}

beforeEach(() => {
  setVqroEnv();
});
afterEach(() => {
  process.env = { ...savedEnv };
});

test("provenance labels distinguish human, agent, reply, and host", () => {
  expect(parseProvenanceLabel("[agent alpha · reply expected] summarise X")).toEqual({
    kind: "agent",
    from: "alpha",
    replyExpected: true,
  });
  expect(parseProvenanceLabel("[agent w2P:pFM · reply] done")).toEqual({
    kind: "agent",
    from: "w2P:pFM",
    replyExpected: false,
  });
  expect(parseProvenanceLabel("[host notice] restarted")).toEqual({ kind: "host", replyExpected: false });
  expect(parseProvenanceLabel("just a human typing")).toBeNull();
  expect(parseProvenanceLabel("[agentalpha] nope")).toBeNull();
});

test("reply bytes are capped without splitting a character", () => {
  expect(boundedReplyBytes("short")).toBe("short");
  const long = "a".repeat(MAX_REPLY_BYTES + 10);
  const capped = boundedReplyBytes(long);
  expect(Buffer.byteLength(capped, "utf8")).toBeLessThanOrEqual(MAX_REPLY_BYTES + 60);
  expect(capped.endsWith("bytes]")).toBe(true);
  const multibyte = "🙂".repeat(MAX_REPLY_BYTES);
  expect(boundedReplyBytes(multibyte).includes("\uFFFD")).toBe(false);
});

test("final assistant text is the last text-bearing assistant message", () => {
  expect(finalAssistantText(assistantCtx("the answer"))).toBe("the answer");
  expect(finalAssistantText({ sessionManager: { getBranch: () => [] } })).toBe("");
});

test("an agent-started turn routes its final text back with --reply", async () => {
  const h = harness();
  await h.emit("input", { source: "interactive", text: "[agent alpha · reply expected] summarise X" });
  await h.emit("agent_settled", {}, assistantCtx("here is the summary"));
  const prompt = h.calls.find((call) => call.args[0] === "agent" && call.args[1] === "prompt");
  expect(prompt?.args).toEqual(["agent", "prompt", "alpha", "here is the summary", "--reply"]);
  expect(h.entries).toEqual([
    expect.objectContaining({ data: expect.objectContaining({ kind: "replied", from: "alpha" }) }),
  ]);
  // A reply label expects no reply, so a stopped turn routes nothing.
  const h2 = harness();
  await h2.emit("input", { source: "interactive", text: "[agent alpha · reply] thanks" });
  await h2.emit("agent_settled", {}, assistantCtx("ok"));
  expect(h2.calls.filter((call) => call.args[1] === "prompt")).toHaveLength(0);
});

test("an empty agent-started turn records no reply and a volatile label", async () => {
  const h = harness();
  await h.emit("input", { source: "interactive", text: "[agent alpha · reply expected] summarise X" });
  await h.emit("agent_settled", {}, assistantCtx("   "));
  expect(h.entries[0]?.data).toMatchObject({ kind: "no-reply", from: "alpha" });
  const metadata = h.calls.find((call) => call.args[0] === "pane" && call.args[1] === "report-metadata");
  expect(metadata?.args).toContain("idle=no reply to alpha");
});

test("human and host input are never routed", async () => {
  for (const text of ["just asking", "[host notice] restarted", "[agent alpha · reply] already a reply"]) {
    const h = harness();
    await h.emit("input", { source: "interactive", text });
    await h.emit("agent_settled", {}, assistantCtx("answer"));
    expect(h.calls).toHaveLength(0);
    expect(h.entries).toHaveLength(0);
  }
});

test("an explicit message to the origin suppresses the automatic reply", async () => {
  const h = harness();
  await h.emit("input", { source: "interactive", text: "[agent alpha · reply expected] summarise X" });
  await h.emit("tool_call", { toolName: "bash", input: { command: 'vqro agent prompt alpha "done already"' } });
  await h.emit("agent_settled", {}, assistantCtx("also this"));
  expect(h.calls.filter((call) => call.args[1] === "prompt")).toHaveLength(0);
  expect(h.entries[0]?.data).toMatchObject({ kind: "explicit", from: "alpha" });
});

test("a reply to self is not routed", async () => {
  const h = harness({
    exec: (args: string[]) =>
      args[0] === "agent" && args[1] === "get"
        ? { code: 0, stdout: JSON.stringify({ result: { agent: { name: "alpha" } } }), stderr: "" }
        : { code: 0, stdout: "", stderr: "" },
  });
  await h.emit("input", { source: "interactive", text: "[agent alpha · reply expected] note" });
  await h.emit("agent_settled", {}, assistantCtx("answer"));
  expect(h.calls.filter((call) => call.args[1] === "prompt")).toHaveLength(0);
});

test("routing stays off without the managed Vqro environment", async () => {
  clearVqroEnv();
  const h = harness();
  await h.emit("input", { source: "interactive", text: "[agent alpha · reply expected] summarise X" });
  await h.emit("agent_settled", {}, assistantCtx("answer"));
  expect(h.calls).toHaveLength(0);
  expect(h.entries).toHaveLength(0);
});
