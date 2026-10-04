import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { AdapterV1 } from "../vqro-pi/index";

/** Tasks owns this policy; the adapter never interprets a recipient or reply. */
export const MAX_REPLY_BYTES = 8192;
export const METADATA_TTL_MS = 600_000;
export const MAILBOX_POLL_MS = 10_000;

/** The exact agent this reply answers, and the delivered message when there is one. */
export interface ReplyTarget {
  binding: string;
  execution: string;
  display: string;
  id?: string;
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

function agentBinding(message: any): Omit<ReplyTarget, "id"> | undefined {
  try {
    const value = JSON.parse(message?.sender ?? "{}");
    if (value?.class !== "agent") return undefined;
    if (![value.terminal_id, value.agent, value.execution].every((field: any) => typeof field === "string" && field)) return undefined;
    return { binding: JSON.stringify(value), execution: value.execution, display: `${value.agent} ${value.execution}` };
  } catch {
    return undefined;
  }
}

/**
 * The reply target for one delivered batch: a single agent sender, and no message
 * that is itself a reply. That last condition is what bounds the exchange — a
 * reply delivered to the other side must not start another automatic reply.
 */
export function replyTarget(messages: any[]): ReplyTarget | undefined {
  const senders = messages.map(agentBinding);
  const first = senders[0];
  if (!first || senders.some(sender => !sender || sender.binding !== first.binding)) return undefined;
  if (messages.some(message => message?.reply_to != null)) return undefined;
  return messages.length === 1 ? { ...first, id: messages[0].id } : { ...first };
}

/** A successful explicit message send to this execution suppresses the automatic reply. */
export function messageConfirmsOrigin(output: string, execution: string): boolean {
  for (const line of output.split("\n")) {
    try {
      const value = JSON.parse(line.trim());
      if (value?.message?.recipient_execution === execution) return true;
    } catch {
      /* Non-response text is not evidence of a successful send. */
    }
  }
  return false;
}

export function renderMessage(message: any): string {
  let sender = "an agent";
  try {
    const binding = JSON.parse(message?.sender ?? "{}");
    if (binding.class === "agent") sender = `agent ${String(binding.execution ?? "unknown")}`;
    else if (binding.class === "human_unverified") sender = "a human";
  } catch {
    /* Retain unknown provenance. */
  }
  return `[vqro message ${message.id} from ${sender}]\n${message.body}`;
}

export default function (pi: ExtensionAPI) {
  let active: AdapterV1 | undefined;
  let detach = () => {};
  const connect = (candidate: any) => {
    if (candidate?.version !== 1 || candidate === active || !["snapshot", "current", "onFact", "beforeRun", "exec", "deliver"].every(key => typeof candidate[key] === "function")) return;
    detach(); active = candidate;
    const adapter = candidate as AdapterV1;
    let replyTo: ReplyTarget | undefined = undefined, explicit = false, shutDown = false, checking = false;
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
        // This delivery starts (or continues) the turn that answers the message, so
        // its sender is owed the final text. A failed delivery owes nothing.
        const target = replyTarget(messages);
        if (shutDown || !adapter.current(token) || !await adapter.deliver(messages.map(renderMessage).join("\n\n"), token)) {
          undelivered(messages);
          return;
        }
        replyTo = target;
      } finally { checking = false; }
    }
    const startPoll = () => {
      if (!poll && !shutDown && adapter.snapshot().self?.execution) poll = setInterval(checkMailbox, MAILBOX_POLL_MS);
    };
    async function noReply(target: ReplyTarget) {
      await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--state-label", `idle=no reply to ${target.display}`, "--seq", String(++metadataSeq), "--ttl-ms", String(METADATA_TTL_MS)], 2000);
    }
    const unBefore = adapter.beforeRun(async token => {
      const messages = await claim(false);
      if (!messages.length) return;
      record({ kind: "messages-claimed", ids: messages.map(message => message.id) });
      if (shutDown || !adapter.current(token)) { undelivered(messages); return; }
      // Claimed into a turn something else started: recorded as delivered context,
      // but this turn does not owe the sender an automatic reply.
      return { message: { customType: "vqro-message", content: messages.map(renderMessage).join("\n\n"), display: true }, discard: () => undelivered(messages) };
    });
    const unFacts = adapter.onFact(async fact => {
      if (fact.kind === "session_closed") { shutDown = true; replyTo = undefined; stopPoll(); return; }
      if (shutDown) return;
      if (fact.kind === "session_started") { stopPoll(); replyTo = undefined; explicit = false; startPoll(); return; }
      if (fact.kind === "run_started") { stopPoll(); explicit = false; return; }
      if (fact.kind === "tool_observed" && fact.toolName === "bash" && replyTo && !explicit) {
        explicit = messageConfirmsOrigin(fact.text ?? "", replyTo.execution); return;
      }
      if (fact.kind !== "run_settled") return;
      startPoll();
      const current = replyTo; replyTo = undefined;
      if (!current) return;
      if (fact.self?.execution === current.execution) { record({ kind: "self", recipient: current.execution }); return; }
      // Disabled/unavailable Tasks cannot record the reply.
      if (!await available() || shutDown || !adapter.current(fact)) return;
      if (explicit) { record({ kind: "explicit", recipient: current.execution }); return; }
      if (!fact.final) { record({ kind: "no-reply", recipient: current.execution }); await noReply(current); return; }
      const body = boundedReplyBytes(fact.final);
      const result = await run(["message", "send", current.binding, body, ...(current.id ? ["--reply-to", current.id] : [])], 10_000);
      if (result?.code === 0) {
        record({ kind: "replied", recipient: current.execution, bytes: Buffer.byteLength(body, "utf8") });
        await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--clear-state-labels"], 2000);
      } else {
        record({ kind: "no-reply", recipient: current.execution, reason: result ? `message send exited ${result.code}` : "message send unavailable" });
        await noReply(current);
      }
    });
    detach = () => { shutDown = true; replyTo = undefined; stopPoll(); unBefore(); unFacts(); };
  };
  const unReady = pi.events.on("vqro:pi-adapter:v1:ready", connect);
  pi.events.emit("vqro:pi-adapter:v1:discover", {});
  pi.on("session_shutdown", async () => { detach(); unReady(); });
}
