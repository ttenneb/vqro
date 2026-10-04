import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import extension, {
  assistantText,
  boundedReplyBytes,
  messageConfirmsOrigin,
  renderMessage,
  replyTarget,
  MAILBOX_POLL_MS,
  MAX_REPLY_BYTES,
} from "./pi";
import adapter from "../vqro-pi/index";

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

function harness(exec?: (args: string[]) => any, consumerFirst = false): Harness {
  const handlers = new Map<string, ((event: any, ctx: any) => Promise<any>)[]>();
  const entries: any[] = [];
  const calls: { args: string[] }[] = [];
  const sent: string[] = [];
  const bus = new Map<string, Set<(value: any) => void>>();
  const pi: any = {
    events: {
      emit(name: string, value: any) { for (const listener of [...(bus.get(name) ?? [])]) listener(value); },
      on(name: string, listener: (value: any) => void) {
        const set = bus.get(name) ?? new Set(); set.add(listener); bus.set(name, set);
        return () => set.delete(listener);
      },
    },
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
    sendMessage(message: any) {
      sent.push(String(message?.content ?? ""));
    },
    async exec(_bin: string, args: string[]) {
      calls.push({ args });
      if (exec) {
        const response = await exec(args);
        if (!(response?.code === 0 && !response.stdout && ((args[0] === "agent" && args[1] === "get") || (args[0] === "task" && args[1] === "whoami")))) return response;
      }
      if (args[0] === "task" && args[1] === "whoami") return { code: 0, stdout: JSON.stringify({ caller: { class: "agent" } }), stderr: "" };
      if (args[0] === "agent" && args[1] === "get") {
        return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta", execution: "exec-beta" } } }), stderr: "" };
      }
      return { code: 0, stdout: "", stderr: "" };
    },
  };
  if (consumerFirst) extension(pi);
  adapter(pi);
  if (!consumerFirst) extension(pi);
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
const sends = (h: Harness) => h.calls.filter((call) => call.args[0] === "message" && call.args[1] === "send");
const bindingOf = (execution: string) =>
  JSON.stringify({ class: "agent", terminal_id: `term-${execution}`, agent: "pi", execution });
const messageFixture = (id: string, body: string, execution = "exec-alpha", replyTo: string | null = null) => ({
  id,
  body,
  reply_to: replyTo,
  sender: bindingOf(execution),
});
const flush = async () => { for (let i = 0; i < 20; i += 1) await Promise.resolve(); };

/** A private harness where the consumer polls, claims the given messages and delivers them. */
function mailboxHarness(claimed: any[], activation = true, consumerFirst = false): Harness {
  return harness((args) => {
    if (args[0] === "agent" && args[1] === "get") return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta", execution: "exec-beta" } } }), stderr: "" };
    if (args[0] === "message" && args[1] === "list") return { code: 0, stdout: JSON.stringify({ activation, waiting: claimed.map((message) => ({ id: message.id })), messages: [] }), stderr: "" };
    if (args[0] === "message" && args[1] === "claim") return { code: 0, stdout: JSON.stringify({ claimed }), stderr: "" };
    return { code: 0, stdout: "", stderr: "" };
  }, consumerFirst);
}

beforeEach(() => {
  setVqroEnv();
});
afterEach(() => {
  process.env = { ...savedEnv };
});

test("a delivered batch is answered by the sender's exact binding with reply_to", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "please review")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("please review");
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("turn_end", turnText("here is the review"));
    await h.emit("agent_settled", {});
    const recorded = sends(h);
    expect(recorded).toHaveLength(1);
    expect(recorded[0].args).toEqual(["message", "send", bindingOf("exec-alpha"), "here is the review", "--reply-to", "m1"]);
    expect(h.entries).toContainEqual(expect.objectContaining({ data: expect.objectContaining({ kind: "replied", recipient: "exec-alpha" }) }));
  } finally {
    jest.useRealTimers();
  }
});

test("a same-sender batch answer links the newest message", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "first"), messageFixture("m2", "second")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "first\n\nsecond" });
    await h.emit("turn_end", turnText("both answered"));
    await h.emit("agent_settled", {});
    expect(sends(h)[0]?.args).toEqual(["message", "send", bindingOf("exec-alpha"), "both answered", "--reply-to", "m2"]);
    // The answer is a reply, so it cannot arm another automatic answer.
    const answer = messageFixture("auto1", "both answered", "exec-beta", "m2");
    expect(replyTarget([answer])).toBeUndefined();
  } finally {
    jest.useRealTimers();
  }
});

test("a run another observed input owns is not answered automatically", async () => {
  jest.useFakeTimers();
  try {
    // The SDK stays idle while a human input hook awaits, so an idle handoff
    // alone does not prove this delivery owns the next run.
    const h = mailboxHarness([messageFixture("m1", "colleague request")]);
    await h.emit("session_start", {});
    await h.emit("input", { source: "interactive", text: "human question" });
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("colleague request");
    await h.emit("before_agent_start", { prompt: "human question" });
    await h.emit("turn_end", turnText("private human answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("a foreign input that wins the run after a handoff is not answered", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "colleague request")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("input", { source: "interactive", text: "human question" });
    await h.emit("before_agent_start", { prompt: "human question" });
    await h.emit("turn_end", turnText("private human answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("the run whose prompt is the delivered content answers it", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "colleague request")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    // The delivery's own input may also be observed; it is not a foreign input.
    const delivered = renderMessage(messageFixture("m1", "colleague request"));
    await h.emit("input", { source: "extension", text: delivered });
    await h.emit("before_agent_start", { prompt: delivered });
    await h.emit("turn_end", turnText("answer to the colleague"));
    await h.emit("agent_settled", {});
    expect(sends(h)[0]?.args).toEqual(["message", "send", bindingOf("exec-alpha"), "answer to the colleague", "--reply-to", "m1"]);
  } finally {
    jest.useRealTimers();
  }
});

test("a handoff queued behind a busy SDK does not route the active turn's final", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "future request")]);
    await h.emit("session_start", {}, { isIdle: () => false });
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("future request");
    // A human-started run settles first; its text is not the colleague's answer.
    await h.emit("before_agent_start", { prompt: "human question, not the request" }, { isIdle: () => false });
    await h.emit("turn_end", turnText("private answer for the human turn"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("a delivered reply never starts another automatic reply", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "answer", "exec-alpha", "m0")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("answer");
    await h.emit("before_agent_start", { prompt: "answer" });
    await h.emit("turn_end", turnText("thanks, got it"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
    expect(h.entries).not.toContainEqual(expect.objectContaining({ data: expect.objectContaining({ kind: "replied" }) }));
  } finally {
    jest.useRealTimers();
  }
});

test("a batch from more than one sender is delivered without an automatic reply", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "one"), messageFixture("m2", "two", "exec-gamma")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "one\n\ntwo" });
    await h.emit("turn_end", turnText("answered both"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("an empty final turn records no reply and a volatile label", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "please review")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
    expect(h.entries).toContainEqual(expect.objectContaining({ data: expect.objectContaining({ kind: "no-reply", recipient: "exec-alpha" }) }));
    const metadata = h.calls.find((call) => call.args[0] === "pane" && call.args[1] === "report-metadata");
    expect(metadata?.args).toContain("idle=no reply to pi exec-alpha");
  } finally {
    jest.useRealTimers();
  }
});

test("an explicit message to the origin suppresses the automatic reply", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "please review")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    const confirmed = JSON.stringify({ message: { id: "m9", recipient_execution: "exec-alpha", state: "waiting", revision: 1 }, operation_id: "op" });
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("tool_result", { toolName: "bash", toolCallId: "t1", content: [{ type: "text", text: confirmed }], isError: false });
    await h.emit("turn_end", turnText("sent it myself"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
    expect(h.entries).toContainEqual(expect.objectContaining({ data: expect.objectContaining({ kind: "explicit", recipient: "exec-alpha" }) }));
  } finally {
    jest.useRealTimers();
  }
});

test("a failed or unrelated message send does not suppress the reply", async () => {
  jest.useFakeTimers();
  try {
    const other = JSON.stringify({ message: { id: "m9", recipient_execution: "exec-gamma", state: "waiting", revision: 1 }, operation_id: "op" });
    const errors = JSON.stringify({ error: { code: "invalid_request", message: "exec-alpha" } });
    const h = mailboxHarness([messageFixture("m1", "please review")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("tool_result", { toolName: "bash", toolCallId: "t1", content: [{ type: "text", text: other }], isError: false });
    await h.emit("tool_result", { toolName: "bash", toolCallId: "t2", content: [{ type: "text", text: errors }], isError: false });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(1);
  } finally {
    jest.useRealTimers();
  }
});

test("unknown or self identity fails closed", async () => {
  const unknown = harness(() => ({ code: 1, stdout: "", stderr: "no agent" }));
  await unknown.emit("before_agent_start", { prompt: "x" });
  await unknown.emit("turn_end", turnText("answer"));
  await unknown.emit("agent_settled", {});
  expect(sends(unknown)).toHaveLength(0);

  jest.useFakeTimers();
  try {
    // The sender is this pane's own execution: never answer yourself.
    const h = mailboxHarness([messageFixture("m1", "note", "exec-beta")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "note" });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("routing stays off without the managed Vqro environment", async () => {
  clearVqroEnv();
  const h = mailboxHarness([messageFixture("m1", "please review")]);
  await h.emit("session_start", {});
  await h.emit("before_agent_start", { prompt: "x" });
  await h.emit("turn_end", turnText("answer"));
  await h.emit("agent_settled", {});
  expect(h.calls).toHaveLength(0);
  expect(h.entries).toHaveLength(0);
});

test("unavailable Tasks cannot record a reply", async () => {
  jest.useFakeTimers();
  try {
    const h = harness((args) => {
      if (args[0] === "task" && args[1] === "whoami") return { code: 1, stdout: "", stderr: "unavailable" };
      if (args[0] === "agent" && args[1] === "get") return { code: 0, stdout: JSON.stringify({ result: { agent: { name: "beta", execution: "exec-beta" } } }), stderr: "" };
      const claimed = [messageFixture("m1", "please review")];
      if (args[0] === "message" && args[1] === "list") return { code: 0, stdout: JSON.stringify({ activation: true, waiting: [{ id: "m1" }], messages: [] }), stderr: "" };
      if (args[0] === "message" && args[1] === "claim") return { code: 0, stdout: JSON.stringify({ claimed }), stderr: "" };
      return { code: 0, stdout: "", stderr: "" };
    });
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(0);
    expect(h.entries.some((entry) => entry.data.kind === "replied")).toBe(false);
  } finally {
    jest.useRealTimers();
  }
});

test("reply bytes are capped without splitting a character", () => {
  expect(boundedReplyBytes("short")).toBe("short");
  const long = "a".repeat(MAX_REPLY_BYTES + 10);
  expect(boundedReplyBytes(long).endsWith("bytes]")).toBe(true);
  expect(boundedReplyBytes("\u{1F642}".repeat(MAX_REPLY_BYTES)).includes("\uFFFD")).toBe(false);
});

test("assistantText reads only text blocks", () => {
  expect(assistantText(turnText("the answer").message)).toBe("the answer");
  expect(assistantText({ content: [{ type: "toolCall" }, { type: "text", text: "hi" }] })).toBe("hi");
  expect(assistantText({ content: [] })).toBe("");
});

test("replyTarget requires one agent sender and a non-reply message", () => {
  expect(replyTarget([messageFixture("m1", "x")])).toEqual({
    binding: bindingOf("exec-alpha"),
    execution: "exec-alpha",
    display: "pi exec-alpha",
    id: "m1",
  });
  expect(replyTarget([messageFixture("m1", "x"), messageFixture("m2", "y")])?.id).toBe("m2");
  expect(replyTarget([messageFixture("m1", "x"), messageFixture("m2", "y", "exec-gamma")])).toBeUndefined();
  expect(replyTarget([messageFixture("m1", "x", "exec-alpha", "m0")])).toBeUndefined();
  expect(replyTarget([{ id: "m1", body: "x", sender: JSON.stringify({ class: "human_unverified", uid: 1 }) }])).toBeUndefined();
  expect(replyTarget([{ id: "m1", body: "x" }])).toBeUndefined();
  expect(replyTarget([])).toBeUndefined();
});

test("messageConfirmsOrigin trusts only a newly committed send to that execution", () => {
  const success = JSON.stringify({ message: { id: "m2", recipient_execution: "exec-alpha", state: "waiting", revision: 1 }, operation_id: "op" });
  expect(messageConfirmsOrigin(success, "exec-alpha")).toBe(true);
  expect(messageConfirmsOrigin(success, "exec-beta")).toBe(false);
  // `message edit` reports the same message shape, at a later revision.
  const edit = JSON.stringify({ message: { id: "m1", recipient_execution: "exec-alpha", state: "waiting", revision: 2 }, operation_id: "edit-op" });
  expect(messageConfirmsOrigin(edit, "exec-alpha")).toBe(false);
  expect(messageConfirmsOrigin(JSON.stringify({ error: { code: "invalid_request", message: "exec-alpha" } }), "exec-alpha")).toBe(false);
  expect(messageConfirmsOrigin('echo "vqro message send exec-alpha hi"', "exec-alpha")).toBe(false);
  expect(messageConfirmsOrigin("", "exec-alpha")).toBe(false);
});

test("renderMessage names the host-attributed sender and carries the body", () => {
  const agentMessage = { id: "m1", body: "please review", sender: JSON.stringify({ class: "agent", execution: "boot:1:2" }) };
  expect(renderMessage(agentMessage)).toContain("vqro message m1 from agent boot:1:2");
  expect(renderMessage({ id: "m2", body: "x" })).toContain("from an agent");
});

test("a waiting message is claimed into a starting turn without an automatic reply", async () => {
  const claimed = [messageFixture("m1", "please review")];
  const h = harness((args) =>
    args[0] === "message" && args[1] === "claim" ? { code: 0, stdout: JSON.stringify({ claimed }), stderr: "" } : { code: 0, stdout: "", stderr: "" },
  );
  const results = await h.emit("before_agent_start", { prompt: "just asking" });
  const injected = results.find((result) => result?.message)?.message;
  expect(injected?.content).toContain("please review");
  expect(h.entries.some((entry) => entry.data.kind === "messages-claimed")).toBe(true);
  await h.emit("turn_end", turnText("answer"));
  await h.emit("agent_settled", {});
  expect(sends(h)).toHaveLength(0);
});

test("activation on: the idle mailbox poll claims and delivers", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "wake up")]);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("wake up");
  } finally {
    jest.useRealTimers();
  }
});

test("activation off: the preflight never claims", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "wake up")], false);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.calls.filter((call) => call.args[1] === "claim")).toHaveLength(0);
    expect(h.sent).toHaveLength(0);
  } finally {
    jest.useRealTimers();
  }
});

test("an idle SDK state wakes even when the local turn flag is stale", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "wake")]);
    await h.emit("session_start", {}, { isIdle: () => true });
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    expect(h.sent.join("\n")).toContain("wake");
  } finally {
    jest.useRealTimers();
  }
});

test("a starting-turn claim after shutdown is recorded, not injected", async () => {
  const claimed = [messageFixture("m1", "x")];
  let release: any, started: any;
  const begun = new Promise<void>(resolve => { started = resolve; });
  const h = harness((args) => {
    if (args[0] === "message" && args[1] === "claim") {
      started(); return new Promise(resolve => { release = () => resolve({ code: 0, stdout: JSON.stringify({ claimed }), stderr: "" }); });
    }
    return { code: 0, stdout: "", stderr: "" };
  });
  const pending = h.emit("before_agent_start", { prompt: "hi" });
  await begun;
  await h.emit("session_shutdown", {});
  release();
  const results = await pending;
  expect(results.some((result) => result?.message)).toBe(false);
  expect(h.entries.some((entry) => entry.data.kind === "messages-undelivered")).toBe(true);
});

test("consumer loaded before adapter discovers the same interface", async () => {
  jest.useFakeTimers();
  try {
    const h = mailboxHarness([messageFixture("m1", "please review")], true, true);
    await h.emit("session_start", {});
    jest.advanceTimersByTime(MAILBOX_POLL_MS + 5);
    await flush();
    await h.emit("before_agent_start", { prompt: "please review" });
    await h.emit("turn_end", turnText("answer"));
    await h.emit("agent_settled", {});
    expect(sends(h)).toHaveLength(1);
  } finally {
    jest.useRealTimers();
  }
});
