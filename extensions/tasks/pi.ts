import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { AdapterV1 } from "../vqro-pi/index";

/** Tasks owns this policy; the adapter never interprets a recipient or reply. */
export const MAX_REPLY_BYTES = 8192;
export const METADATA_TTL_MS = 600_000;
export const MAILBOX_POLL_MS = 10_000;
// A mutation CLI performs whoami then the write, each with a six-second API
// deadline. Leave three seconds for process overhead and unknown-outcome text;
// an outer kill at either inner deadline can hide the original receipt scope.
export const MESSAGE_MUTATION_TIMEOUT_MS = 15_000;

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
    if (Object.keys(value).length !== 4 || ![value.terminal_id, value.agent, value.execution].every((field: any) => typeof field === "string" && field && Buffer.byteLength(field, "utf8") <= 256)) return undefined;
    return { binding: JSON.stringify(value), execution: value.execution, display: `${value.agent} ${value.execution}` };
  } catch {
    return undefined;
  }
}

/**
 * The reply target for one delivered batch: a single agent sender, and no message
 * that is itself a reply. That last condition is what bounds the exchange — a
 * reply delivered to the other side must not start another automatic reply,
 * which is also why every automatic answer carries `reply_to` (a same-sender
 * batch links its newest message).
 */
export function replyTarget(messages: any[]): ReplyTarget | undefined {
  const senders = messages.map(agentBinding);
  const first = senders[0];
  if (!first || senders.some(sender => !sender || sender.binding !== first.binding)) return undefined;
  if (messages.some(message => message?.reply_to != null)) return undefined;
  return { ...first, id: messages[messages.length - 1]?.id };
}

/**
 * A successful explicit message send to this execution suppresses the automatic
 * reply. Only a newly committed send counts: `message edit` reports the same
 * message shape, so an edited old message at revision 1 would otherwise pass.
 */
export function messageConfirmsOrigin(output: string, execution: string): boolean {
  for (const line of output.split("\n")) {
    try {
      const message = JSON.parse(line.trim())?.message;
      if (message?.recipient_execution === execution && message?.state === "waiting" && message?.revision === 1) return true;
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
  const exact = agentBinding(message);
  // This is the existing CLI's exact-binding recipient, not a display name or
  // bare execution id that could be resolved to another process after reuse.
  const recipient = exact ? `\n[reply recipient (exact binding JSON): ${exact.binding}]\n[reply usage: vqro message send TARGET TEXT --reply-to ${message.id}; quote JSON as one TARGET]` :
    sender.startsWith("agent ") ? "\n[reply recipient unavailable: no exact sender binding]" : "";
  return `[vqro message ${message.id} from ${sender}]${recipient}\n${message.body}`;
}

/**
 * The unique delivery id the SDK copies into the run's own input. A claimed
 * message id is random per message, so a run whose observed input carries it is
 * proven to be running that delivery — positive ownership rather than the absence
 * of a remembered competitor, and robust to an input hook prepending or appending
 * context around it.
 */
export function deliveryMarker(message: any): string | undefined {
  return typeof message?.id === "string" && message.id ? `[vqro message ${message.id} from ` : undefined;
}

export default function (pi: ExtensionAPI) {
  let active: AdapterV1 | undefined;
  let detach = () => {};
  const connect = (candidate: any) => {
    if (candidate?.version !== 1 || candidate === active || !["snapshot", "current", "onFact", "beforeRun", "exec", "deliver"].every(key => typeof candidate[key] === "function")) return;
    detach(); active = candidate;
    const adapter = candidate as AdapterV1;
    let replyTo: ReplyTarget | undefined = undefined, explicit = false, shutDown = false, checking = false;
    let inputRevision = 0, followUpRun: number | undefined;
    let initialMarker: string | undefined;
    // A delivered batch awaiting the run that runs it, identified by the unique
    // delivery id the SDK copies into that run's own input.
    let pending: { marker: string; target: ReplyTarget } | undefined = undefined;
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
      const result = await run(["message", "claim", ...(idle ? ["--idle"] : []), "--json"], MESSAGE_MUTATION_TIMEOUT_MS);
      if (result?.code !== 0) {
        if (result?.stderr?.includes("outcome unknown")) record({ kind: "claim-unknown", recovery: result.stderr });
        return [];
      }
      try { return JSON.parse(result.stdout)?.claimed ?? []; } catch { return []; }
    }
    async function outcomes(messages: any[], outcome: string): Promise<boolean> {
      const attempts = messages.map(message => ({ id: message.id, attempt: message?.attempt?.id }));
      if (attempts.some(item => !item.attempt)) return false;
      // One bounded transaction/CLI call for the whole claim batch, rather than
      // up to eight sequential mutation deadlines inside a starting SDK hook.
      const result = await run(["message", "outcomes", JSON.stringify(attempts), outcome, "--json"], MESSAGE_MUTATION_TIMEOUT_MS);
      if (result?.code === 0) return true;
      // The claim's pending attempt is already durable attention, even if this
      // write or acknowledgement is lost. Do not retry either effect.
      record({ kind: "outcome-unconfirmed", attempts, outcome, recovery: result?.stderr });
      return false;
    }
    async function undelivered(messages: any[]) {
      record({ kind: "messages-undelivered", ids: messages.map(message => message.id) });
      await outcomes(messages, "no_effect");
    }
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
        const content = messages.map(renderMessage).join("\n\n");
        const target = replyTarget(messages);
        if (shutDown || !adapter.current(token)) { await undelivered(messages); return; }
        let outcome: string;
        try {
          outcome = adapter.deliverOutcome ? await adapter.deliverOutcome(content, token) :
            await adapter.deliver(content, token) ? "submitted" : "uncertain";
        } catch { outcome = "uncertain"; }
        // Arm before awaiting the outcome write: the SDK may start its queued
        // run while the CLI receipt is in flight. This is reply correlation only,
        // never durable execution proof, and never retries a submitted effect.
        if (outcome === "submitted" && !shutDown && adapter.current(token)) {
          const marker = deliveryMarker(messages[0]);
          pending = target && marker ? { marker, target } : undefined;
        }
        await outcomes(messages, outcome);
      } finally { checking = false; }
    }
    const startPoll = () => {
      // Keep the existing clock armed for a live unknown session. checkMailbox
      // still performs no list/claim/delivery until a proved identity is seen.
      if (!poll && !shutDown) poll = setInterval(checkMailbox, MAILBOX_POLL_MS);
    };
    async function noReply(target: ReplyTarget) {
      await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--state-label", `idle=no reply to ${target.display}`, "--seq", String(++metadataSeq), "--ttl-ms", String(METADATA_TTL_MS)], 2000);
    }
    const unBefore = adapter.beforeRun(async token => {
      const messages = await claim(false);
      if (!messages.length) return;
      record({ kind: "messages-claimed", ids: messages.map(message => message.id) });
      if (shutDown || !adapter.current(token)) { await undelivered(messages); return; }
      // Hook preparation cannot prove SDK consumption: a crash after its durable
      // callback but before return is still attention, not confirmed delivery.
      return {
        message: { customType: "vqro-message", content: messages.map(renderMessage).join("\n\n"), display: true },
        discard: () => undelivered(messages),
        prepared: async () => {
          if (!await outcomes(messages, "context_prepared")) throw new Error("context outcome unconfirmed");
        },
      };
    });
    const unFacts = adapter.onFact(async fact => {
      if (fact.kind === "session_closed") { shutDown = true; replyTo = undefined; pending = undefined; initialMarker = undefined; stopPoll(); return; }
      if (shutDown) return;
      if (fact.kind === "session_started") { stopPoll(); replyTo = undefined; pending = undefined; initialMarker = undefined; followUpRun = undefined; explicit = false; startPoll(); return; }
      if (fact.kind === "input_message_started") {
        inputRevision += 1;
        // The SDK emits the starting prompt after run_started. Accept it once;
        // every other user/custom observation revokes ownership for this run.
        const initial = initialMarker; initialMarker = undefined;
        if (initial && fact.text_truncated !== true && (fact.text ?? "").includes(initial)) return;
      }
      if (fact.kind === "input_observed" || fact.kind === "input_message_started") {
        if (fact.kind === "input_observed") inputRevision += 1;
        initialMarker = undefined;
        // run_started may still be awaiting its identity read when input arrives.
        if (fact.state === "working") followUpRun = fact.run;
        // Ownership of the starting prompt does not cover steering/follow-up
        // input inside the same SDK run. Never send its answer to the old sender.
        const current = replyTo; replyTo = undefined;
        if (current) record({ kind: "no-reply", recipient: current.execution, reason: "follow-up-input" });
        return;
      }
      if (fact.kind === "run_started") {
        stopPoll(); explicit = false;
        const current = pending; pending = undefined;
        const prompt = fact.prompt ?? "";
        // Positive ownership: this run's own input carries the delivery id, so the
        // SDK is running this delivery. A truncated observation is not complete
        // proof, and anything else — a human turn that won the race, a busy-queued
        // follow-up, transformed input — is not proven either: none of those is
        // sent an automatic reply, and the pane is told so.
        const owns = current !== undefined && fact.run !== followUpRun && fact.prompt_truncated !== true && prompt.includes(current.marker);
        replyTo = owns && current ? current.target : undefined;
        initialMarker = owns ? current?.marker : undefined;
        if (current && !owns) {
          record({ kind: "no-reply", recipient: current.target.execution, reason: "delivery-not-run" });
          await noReply(current.target);
        }
        return;
      }
      if (fact.kind === "tool_observed" && fact.toolName === "bash" && replyTo && !explicit) {
        explicit = messageConfirmsOrigin(fact.text ?? "", replyTo.execution); return;
      }
      if (fact.kind !== "run_settled") return;
      startPoll();
      const current = replyTo; replyTo = undefined; initialMarker = undefined;
      const revision = inputRevision;
      if (!current) return;
      if (fact.self?.execution === current.execution) { record({ kind: "self", recipient: current.execution }); return; }
      // Recheck input after the asynchronous availability read as well.
      if (!await available() || shutDown || !adapter.current(fact) || revision !== inputRevision) return;
      if (explicit) { record({ kind: "explicit", recipient: current.execution }); return; }
      if (!fact.final) { record({ kind: "no-reply", recipient: current.execution }); await noReply(current); return; }
      const body = boundedReplyBytes(fact.final);
      const result = await run(["message", "send", current.binding, body, ...(current.id ? ["--reply-to", current.id] : [])], MESSAGE_MUTATION_TIMEOUT_MS);
      if (result?.code === 0) {
        record({ kind: "replied", recipient: current.execution, bytes: Buffer.byteLength(body, "utf8") });
        await run(["pane", "report-metadata", process.env.VQRO_PANE_ID!, "--source", "vqro.reply-routing", "--clear-state-labels"], 2000);
      } else {
        if (result?.stderr?.includes("outcome unknown")) {
          record({ kind: "reply-unknown", recipient: current.execution, recovery: result.stderr });
        }
        record({ kind: "no-reply", recipient: current.execution, reason: result ? `message send exited ${result.code}` : "message send unavailable" });
        await noReply(current);
      }
    });
    detach = () => { shutDown = true; replyTo = undefined; pending = undefined; stopPoll(); unBefore(); unFacts(); };
  };
  const unReady = pi.events.on("vqro:pi-adapter:v1:ready", connect);
  pi.events.emit("vqro:pi-adapter:v1:discover", {});
  pi.on("session_shutdown", async () => { detach(); unReady(); });
}
