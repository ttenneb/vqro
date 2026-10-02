import { afterEach, beforeEach, expect, test } from "bun:test";
import extension, {
  assistantText,
  boundedReplyBytes,
  parseProvenanceLabel,
  targetsAgentPrompt,
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
  emit: (name: string, event: any, ctx?: any) => Promise<void>;
}

function harness(exec?: (args: string[]) => any): Harness {
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
      if (exec) return exec(args);
      if (args[0] === "agent" && args[1] === "get") {
        return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta" } } }), stderr: "" };
      }
      return { code: 0, stdout: "", stderr: "" };
    },
  };
  extension(pi);
  return {
    entries,
    calls,
    async emit(name, event, ctx = {}) {
      for (const handler of handlers.get(name) ?? []) await handler(event, ctx);
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
  expect(parseProvenanceLabel("[agent w2P:pFM · reply] done")).toEqual({
    from: "w2P:pFM",
    replyExpected: false,
  });
  expect(parseProvenanceLabel("[host notice] restarted")).toBeNull();
  expect(parseProvenanceLabel("just a human typing")).toBeNull();
  expect(parseProvenanceLabel("[agentalpha] nope")).toBeNull();
});

test("A2 recognises only a direct vqro agent prompt call to the exact target", () => {
  expect(targetsAgentPrompt('vqro agent prompt alpha "done"', "alpha")).toBe(true);
  expect(targetsAgentPrompt('./vqro agent prompt alpha "done"', "alpha")).toBe(true);
  expect(targetsAgentPrompt('env A=1 vqro agent prompt alpha "done"', "alpha")).toBe(true);
  expect(targetsAgentPrompt('vqro agent prompt alpha "see; vqro agent prompt beta"', "alpha")).toBe(true);
  expect(targetsAgentPrompt('vqro agent prompt gamma "alpha was mentioned"', "alpha")).toBe(false);
  expect(targetsAgentPrompt('vqro agent prompt betamax', "beta")).toBe(false);
  expect(targetsAgentPrompt('echo vqro agent prompt alpha', "alpha")).toBe(false);
  expect(targetsAgentPrompt('echo "vqro agent prompt alpha"', "alpha")).toBe(false);
  expect(targetsAgentPrompt('cd /x && vqro agent prompt alpha', "alpha")).toBe(false);
  expect(targetsAgentPrompt('vqro agent prompt alpha done || true', "alpha")).toBe(false);
  expect(targetsAgentPrompt('cat <<EOF\nvqro agent prompt alpha\nEOF', "alpha")).toBe(false);
  expect(targetsAgentPrompt('printf \'x; vqro agent prompt alpha\'', "alpha")).toBe(false);
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

test("an agent-started run routes its final text back with --reply", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("turn_end", turnText("here is the summary"));
  await h.emit("agent_settled", {});
  expect(prompts(h)[0]?.args).toEqual(["agent", "prompt", "alpha", "here is the summary", "--reply"]);
  expect(h.entries).toEqual([expect.objectContaining({ data: expect.objectContaining({ kind: "replied", from: "alpha" }) })]);
});

test("a reply-labelled prompt never expects another reply", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply] thanks" });
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
  expect(h.calls).toHaveLength(0);
  expect(h.entries).toHaveLength(0);
});

test("a successful explicit message to the origin suppresses the automatic reply", async () => {
  const h = harness();
  await h.emit("before_agent_start", { prompt: "[agent alpha · reply expected] summarise X" });
  await h.emit("tool_call", { toolName: "bash", toolCallId: "t1", input: { command: 'vqro agent prompt alpha "done"' } });
  await h.emit("tool_result", { toolName: "bash", toolCallId: "t1", isError: false });
  await h.emit("turn_end", turnText("also this"));
  await h.emit("agent_settled", {});
  expect(prompts(h)).toHaveLength(0);
  expect(h.entries[0]?.data).toMatchObject({ kind: "explicit", from: "alpha" });
});

test("A2 does not suppress on a failed send or a mentioned name", async () => {
  const failed = harness();
  await failed.emit("before_agent_start", { prompt: "[agent alpha · reply expected] x" });
  await failed.emit("tool_call", { toolName: "bash", toolCallId: "t1", input: { command: 'vqro agent prompt alpha "done"' } });
  await failed.emit("tool_result", { toolName: "bash", toolCallId: "t1", isError: true });
  await failed.emit("turn_end", turnText("answer"));
  await failed.emit("agent_settled", {});
  expect(prompts(failed)).toHaveLength(1);

  const mentioned = harness();
  await mentioned.emit("before_agent_start", { prompt: "[agent alpha · reply expected] x" });
  await mentioned.emit("tool_call", { toolName: "bash", toolCallId: "t1", input: { command: 'vqro agent prompt gamma "alpha was mentioned"' } });
  await mentioned.emit("tool_result", { toolName: "bash", toolCallId: "t1", isError: false });
  await mentioned.emit("turn_end", turnText("answer"));
  await mentioned.emit("agent_settled", {});
  expect(prompts(mentioned)).toHaveLength(1);
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
