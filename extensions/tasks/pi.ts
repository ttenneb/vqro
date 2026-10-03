import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { AdapterV1 } from "../vqro-pi/index";

/** Tasks owns this policy; the adapter never interprets a recipient or reply. */
export const MAX_REPLY_BYTES = 8192;
export const METADATA_TTL_MS = 600_000;
export const MAILBOX_POLL_MS = 10_000;
export interface Origin { from: string; replyExpected: boolean }
export function parseProvenanceLabel(text: string): Origin | null {
  const match = /^\[agent ([^\]\s]+)(?: · (reply expected))?\]\s?/.exec(text);
  return match ? { from: match[1], replyExpected: match[2] === "reply expected" } : null;
}
export function boundedReplyBytes(text: string, maxBytes = MAX_REPLY_BYTES): string {
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length <= maxBytes) return text;
  return `${bytes.subarray(0, maxBytes).toString("utf8").replace(/\uFFFD+$/, "")}\n[truncated by vqro reply routing at ${maxBytes} bytes]`;
}
export function assistantText(message: any): string {
  if (typeof message?.content === "string") return message.content;
  return Array.isArray(message?.content) ? message.content.filter((b: any) => b?.type === "text").map((b: any) => b.text).join("\n") : "";
}
export function promptConfirmsOrigin(output: string, from: string): boolean {
  for (const line of output.split("\n")) {
    try {
      const value = JSON.parse(line.trim()), agent = value?.result?.agent;
      if (value?.result?.type === "agent_prompted" && (agent?.name === from || agent?.pane_id === from)) return true;
    } catch { /* Non-response text is not evidence of a successful send. */ }
  }
  return false;
}
export function renderMessage(message: any): string {
  let sender = "an agent";
  try {
    const binding = JSON.parse(message?.sender ?? "{}");
    if (binding.class === "agent") sender = `agent ${String(binding.execution ?? "unknown")}`;
    else if (binding.class === "human_unverified") sender = "a human";
  } catch { /* Retain unknown provenance. */ }
  return `[vqro message ${message.id} from ${sender}]\n${message.body}`;
}

export default function (pi: ExtensionAPI) {
  let active: AdapterV1 | undefined;
  let detach = () => {};
  const connect = (candidate: any) => {
    if (candidate?.version !== 1 || candidate === active || !["snapshot", "current", "onFact", "beforeRun", "exec", "deliver"].every(key => typeof candidate[key] === "function")) return;
    detach(); active = candidate;
    const adapter = candidate as AdapterV1;
    let origin: Origin | null = null, explicit = false, shutDown = false, checking = false;
    let poll: ReturnType<typeof setInterval> | undefined;
    let metadataSeq = Date.now() * 1000;
    const record = (data: Record<string, unknown>) => pi.appendEntry("vqro-reply-routing", { ...data, at: new Date().toISOString() });
    const stopPoll = () => { if (poll) clearInterval(poll); poll = undefined; };
    const run = (args: string[], timeout: number) => adapter.exec(args, timeout);
    const available = async () => {
      const result = await run(["task", "whoami", "--json"], 6000);
      if (result?.code !== 0) return false;
      try { return JSON.parse(result.stdout)?.caller?.class === "agent"; } catch { return false; }
    };
    async function claim(idle: boolean) {
      const result = await run(["message", "claim", ...(idle ? ["--idle"] : []), "--json"], 6000);
      if (result?.code !== 0) {
        if (result?.stderr?.includes("outcome unknown")) record({ kind: "claim-unknown", recovery: result.stderr });
        return [];
      }
      try { return JSON.parse(result.stdout)?.claimed ?? []; } catch { return []; }
    }
    const undelivered = (messages: any[]) => record({ kind: "messages-undelivered", ids: messages.map(message => message.id) });
    async function checkMailbox() {
      if (checking || shutDown) return;
      const token = adapter.snapshot();
      if (!token.self?.execution) return;
      checking = true;
      try {
        const result = await run(["message", "list", "--json"], 4000);
        if (result?.code !== 0) return;
        let value: any;
        try { value = JSON.parse(result.stdout); } catch { return; }
        if (value?.activation !== true || !value.waiting?.length || !adapter.current(token) || shutDown) return;
        const messages = await claim(true);
        if (!messages.length) return;
        record({ kind: "messages-claimed", ids: messages.map(message => message.id) });
        if (shutDown || !adapter.current(token) || !await adapter.deliver(messages.map(renderMessage).join("\n\n"), token)) undelivered(messages);
      } finally { checking = false; }
    }
    const startPoll = () => {
      if (!poll && !shutDown && adapter.snapshot().self?.execution) poll = setInterval(checkMailbox, MAILBOX_POLL_MS);
    };
    async function noReply(from: string) {
      await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--state-label", `idle=no reply to ${from}`, "--seq", String(++metadataSeq), "--ttl-ms", String(METADATA_TTL_MS)], 2000);
    }
    const unBefore = adapter.beforeRun(async token => {
      const messages = await claim(false);
      if (!messages.length) return;
      record({ kind: "messages-claimed", ids: messages.map(message => message.id) });
      if (shutDown || !adapter.current(token)) { undelivered(messages); return; }
      return { message: { customType: "vqro-message", content: messages.map(renderMessage).join("\n\n"), display: true }, discard: () => undelivered(messages) };
    });
    const unFacts = adapter.onFact(async fact => {
      if (fact.kind === "session_closed") { shutDown = true; stopPoll(); return; }
      if (shutDown) return;
      if (fact.kind === "session_started") { stopPoll(); origin = null; explicit = false; startPoll(); return; }
      if (fact.kind === "run_started") {
        stopPoll(); origin = parseProvenanceLabel(fact.prompt); explicit = false; return;
      }
      if (fact.kind === "tool_observed" && fact.toolName === "bash" && origin && !explicit) {
        explicit = promptConfirmsOrigin(fact.text ?? "", origin.from); return;
      }
      if (fact.kind !== "run_settled") return;
      startPoll();
      const current = origin; origin = null;
      if (!current?.replyExpected) return;
      const from = current.from, self = fact.self?.name || fact.self?.pane_id;
      if (!self) { record({ kind: "self-unknown", from }); return; }
      if (self === from) return;
      // Disabled/unavailable Tasks cannot apply reply/no-reply workflow.
      if (!await available() || shutDown || !adapter.current(fact)) return;
      if (explicit) { record({ kind: "explicit", from }); return; }
      if (!fact.final) { record({ kind: "no-reply", from }); await noReply(from); return; }
      // Approved compatibility exception, not a durable message or acceptance:
      // docs/next/architecture/tasks-legacy-prompt-reply-transition.md.
      const body = boundedReplyBytes(fact.final);
      const result = await run(["agent", "prompt", from, body, "--no-reply-expected"], 10_000);
      if (result?.code === 0) {
        record({ kind: "replied", from, bytes: Buffer.byteLength(body, "utf8") });
        await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--clear-state-labels"], 2000);
      } else {
        record({ kind: "no-reply", from, reason: result ? `agent prompt exited ${result.code}` : "agent prompt unavailable" });
        await noReply(from);
      }
    });
    detach = () => { shutDown = true; stopPoll(); unBefore(); unFacts(); };
  };
  const unReady = pi.events.on("vqro:pi-adapter:v1:ready", connect);
  pi.events.emit("vqro:pi-adapter:v1:discover", {});
  pi.on("session_shutdown", async () => { detach(); unReady(); });
}
