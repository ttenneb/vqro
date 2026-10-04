import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import net from "node:net";
import path from "node:path";
import { randomUUID } from "node:crypto";

export const READY = "vqro:pi-adapter:v1:ready";
export const DISCOVER = "vqro:pi-adapter:v1:discover";
export const MAX_FACT_BYTES = 64 * 1024;
export interface Run {
  version: 1; session: string; generation: number; run: number; sequence: number;
  state: "idle" | "working" | "closed";
  self?: { name?: string; execution?: string; terminal_id?: string; pane_id?: string };
  prompt: string; prompt_truncated?: boolean; source: string; origin?: { label: string; trust: "self_declared" };
  final: string; truncated: boolean;
}
export interface Fact extends Run { kind: string; toolName?: string; text?: string; text_truncated?: boolean }
export interface ContextInput { message: { customType: string; content: string; display: boolean }; discard?: () => void }
export interface AdapterV1 {
  version: 1;
  snapshot(): Run;
  current(token: Run): boolean;
  onFact(listener: (fact: Fact) => void | Promise<void>): () => void;
  beforeRun(listener: (run: Run) => Promise<ContextInput | void>): () => void;
  exec(args: string[], timeout: number): Promise<any>;
  deliver(content: string, token: Run): Promise<boolean>;
}
export function assistantText(message: any): string {
  if (typeof message?.content === "string") return message.content;
  return Array.isArray(message?.content) ? message.content.filter((b: any) => b?.type === "text").map((b: any) => b.text).join("\n") : "";
}
export function boundedText(text: string) {
  const bytes = Buffer.from(text, "utf8");
  return { text: bytes.length <= MAX_FACT_BYTES ? text : bytes.subarray(0, MAX_FACT_BYTES).toString("utf8").replace(/\uFFFD+$/, ""), truncated: bytes.length > MAX_FACT_BYTES };
}

/** SDK bridge only: no mailbox, workflow, recipient or reply policy. */
export default function (pi: ExtensionAPI) {
  const bin = process.env.VQRO_BIN_PATH, pane = process.env.VQRO_PANE_ID;
  const enabled = process.env.VQRO_ENV === "1" && !!process.env.VQRO_SOCKET_PATH && !!bin && path.isAbsolute(bin) && !!pane && process.env.HERDR_ENV !== "1" && !process.env.HERDR_SOCKET_PATH && !process.env.HERDR_CLIENT_SOCKET_PATH;
  if (!enabled) return;
  let latestCtx: any, source = "unknown";
  let state: Run = { version: 1, session: randomUUID(), generation: 1, run: 0, sequence: 0, state: "idle", prompt: "", source, final: "", truncated: false };
  const facts = new Set<(fact: Fact) => void | Promise<void>>();
  const before = new Set<(run: Run) => Promise<ContextInput | void>>();
  const snapshot = () => structuredClone(state);
  const current = (token: Run) => state.state !== "closed" && token.session === state.session && token.generation === state.generation && token.run === state.run && token.self?.execution === state.self?.execution;
  const exec = async (args: string[], timeout: number) => state.state === "closed" ? null : pi.exec(bin!, args, { timeout }).catch(() => null);
  async function identity() {
    const result = await exec(["agent", "get", pane!], 2000);
    if (result?.code !== 0) return undefined;
    try {
      const agent = JSON.parse(result.stdout)?.result?.agent;
      if (!agent) return undefined;
      return { name: agent.name, execution: agent.execution, terminal_id: agent.terminal_id, pane_id: agent.pane_id };
    } catch { return undefined; }
  }
  async function publish(kind: string, extra: Partial<Fact> = {}) {
    state.sequence += 1;
    const observation = { ...snapshot(), kind, ...structuredClone(extra) };
    for (const listener of [...facts]) {
      try { await listener(structuredClone(observation)); }
      catch { /* An optional consumer cannot fail the SDK lifecycle. */ }
    }
  }
  function discard(contexts: ContextInput[]) {
    for (const context of contexts) {
      try { context.discard?.(); } catch { /* Failed consumer cannot break another context. */ }
    }
  }
  function subscribe<T>(set: Set<T>, listener: T) {
    if (set.size >= 16 || state.state === "closed") return () => {};
    set.add(listener); return () => { set.delete(listener); };
  }

  // The host refuses to type into an editor that already holds the human's
  // unsent text, so it needs to know whether this one does. Only the fact is
  // reported (never the content), once a second, as the same neutral
  // pane.report_metadata request the other integrations send. Reporting only a
  // flip is not enough: the host discards a report older than the human's
  // latest key, and a line typed and deleted between two polls never flips.
  // The socket write is deliberate: a CLI spawn per pane per second is not
  // bounded enough to run in every Vqro pane.
  let editorPoll: ReturnType<typeof setInterval> | undefined;
  let inputBoxSeq = Date.now() * 1000;

  function readInputBoxHasText(): boolean | undefined {
    try {
      const text = latestCtx?.ui?.getEditorText?.();
      return typeof text === "string" ? text.trim().length > 0 : undefined;
    } catch {
      return undefined;
    }
  }

  function reportInputBox(): void {
    const socketPath = process.env.VQRO_SOCKET_PATH;
    const hasText = readInputBoxHasText();
    if (!socketPath || hasText === undefined || state.state === "closed") return;
    inputBoxSeq += 1;
    const request = JSON.stringify({
      id: `vqro:pi:input-box:${inputBoxSeq}`,
      method: "pane.report_metadata",
      params: { pane_id: pane, source: "vqro:pi", seq: inputBoxSeq, tokens: { input_box: hasText ? "has_text" : "empty" } },
    });
    const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\${socketPath}` : socketPath;
    const socket = net.createConnection(endpoint);
    socket.on("error", () => socket.destroy());
    socket.on("connect", () => socket.write(`${request}\n`));
    socket.on("data", () => socket.destroy());
    socket.unref?.();
  }

  function watchInputBox(): void {
    if (editorPoll) clearInterval(editorPoll);
    reportInputBox();
    editorPoll = setInterval(reportInputBox, 1000);
    editorPoll.unref?.();
  }

  function stopInputBox(): void {
    if (editorPoll) clearInterval(editorPoll);
    editorPoll = undefined;
  }
  const adapter: AdapterV1 = {
    version: 1, snapshot, current, exec,
    onFact: listener => subscribe(facts, listener),
    beforeRun: listener => subscribe(before, listener),
    async deliver(content, token) {
      if (Buffer.byteLength(content, "utf8") > 128 * 1024 || !current(token) || !token.self?.execution) return false;
      const fresh = await identity();
      if (!current(token) || fresh?.execution !== token.self.execution) return false;
      // Always queue: an observed idle state can race a turn beginning inside
      // SDK input hooks. Facts themselves never invoke either delivery method.
      try {
        const idle = typeof latestCtx?.isIdle === "function" ? latestCtx.isIdle() : state.state === "idle";
        if (idle) pi.sendUserMessage(content, { deliverAs: "followUp" });
        else pi.sendMessage({ customType: "vqro-input", content, display: true }, { deliverAs: "followUp" });
        return true;
      } catch { return false; }
    },
  };
  // Synchronous SDK-scoped discovery, independent of extension load order.
  const announce = () => { if (state.state !== "closed") pi.events.emit(READY, adapter); };
  const unDiscover = pi.events.on(DISCOVER, announce);
  announce();
  pi.on("input", async event => {
    source = event.source;
    const value = boundedText(event.text);
    await publish("input_observed", { text: value.text, text_truncated: value.truncated, source });
  });
  pi.on("session_start", async (_event, ctx) => {
    if (state.state === "closed") return;
    latestCtx = ctx;
    watchInputBox();
    state.generation += 1; state.run = 0; state.state = "idle";
    state.self = undefined; state.prompt = ""; state.origin = undefined;
    state.final = ""; state.truncated = false; source = "unknown"; state.source = source;
    const generation = state.generation;
    const self = await identity();
    if (generation !== state.generation || state.state === "closed") return;
    state.self = self;
    await publish("session_started");
  });
  pi.on("before_agent_start", async (event, ctx) => {
    if (state.state === "closed") return;
    latestCtx = ctx;
    state.run += 1; state.state = "working"; state.final = ""; state.truncated = false;
    const prompt = boundedText(event.prompt);
    state.prompt = prompt.text; state.prompt_truncated = prompt.truncated; state.source = source; source = "unknown";
    const label = /^\[[^\]\r\n]{1,256}\]/.exec(event.prompt)?.[0];
    state.origin = label ? { label, trust: "self_declared" } : undefined;
    const generation = state.generation, run = state.run;
    const self = await identity();
    if (generation !== state.generation || run !== state.run || state.state === "closed") return;
    state.self = self;
    const token = snapshot();
    await publish("run_started");
    if (!current(token)) return;
    const accepted: ContextInput[] = [];
    // EventEmitter.emit does not await listeners. Await these callbacks in the
    // actual SDK hook, then recheck execution/run before returning any context.
    for (const listener of [...before]) {
      if (!current(token)) break;
      try {
        const context = await listener(structuredClone(token));
        if (context && typeof context.message?.content === "string") accepted.push(context);
      } catch { /* Failed consumer contributes no context. */ }
    }
    if (accepted.length === 0) return;
    const fresh = await identity();
    if (!current(token) || !token.self?.execution || fresh?.execution !== token.self.execution) {
      discard(accepted);
      return;
    }
    const content = accepted.map(context => context.message.content).join("\n\n");
    if (Buffer.byteLength(content, "utf8") > 128 * 1024) {
      discard(accepted);
      return;
    }
    if (content) return { message: accepted.length === 1 ? structuredClone(accepted[0].message) : { customType: "vqro-input", content, display: true } };
  });
  pi.on("turn_end", async event => {
    if (state.state === "closed") return;
    const value = boundedText(assistantText(event.message).trim());
    state.final = value.text; state.truncated = value.truncated;
    await publish("turn_ended");
  });
  pi.on("tool_result", async event => {
    if (state.state === "closed") return;
    const value = boundedText(assistantText({ content: event.content }));
    await publish("tool_observed", { toolName: event.toolName, text: value.text, text_truncated: value.truncated });
  });
  pi.on("agent_settled", async (_event, ctx) => {
    if (state.state === "closed") return;
    latestCtx = ctx; state.state = "idle";
    const token = snapshot(), self = await identity();
    if (!current(token)) return;
    // A changed/unknown execution cannot inherit the previous run's result.
    if (self?.execution !== token.self?.execution) { state.final = ""; state.origin = undefined; }
    state.self = self;
    await publish("run_settled");
  });
  pi.on("session_shutdown", async () => {
    stopInputBox();
    state.generation += 1; state.state = "closed";
    unDiscover();
    await publish("session_closed");
    facts.clear(); before.clear();
  });
}
