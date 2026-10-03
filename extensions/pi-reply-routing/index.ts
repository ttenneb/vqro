import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import path from "node:path";

/** Routed replies are capped so one turn cannot flood the origin pane's input. */
export const MAX_REPLY_BYTES = 8192;
/** The no-reply attention label is a volatile observation, not durable state. */
export const METADATA_TTL_MS = 600_000;
/** Idle mailbox poll while the activation setting is on; one bounded read per tick. */
export const MAILBOX_POLL_MS = 10_000;

export interface Origin {
  /** Agent name, or pane id when the sender had no registered name. */
  from: string;
  replyExpected: boolean;
}

/**
 * Parse the provenance label that `vqro agent prompt` writes when it runs inside
 * an agent pane. Absent or unrecognized text is human input to be ignored. The
 * label is self-declared and spoofable in trusted same-user scope, not
 * authenticated caller identity.
 */
export function parseProvenanceLabel(text: string): Origin | null {
  const match = /^\[agent ([^\]\s]+)(?: · (reply expected))?\]\s?/.exec(text);
  if (!match) return null;
  return { from: match[1], replyExpected: match[2] === "reply expected" };
}

export function boundedReplyBytes(text: string, maxBytes = MAX_REPLY_BYTES): string {
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length <= maxBytes) return text;
  const cut = bytes.subarray(0, maxBytes).toString("utf8").replace(/\uFFFD+$/, "");
  return `${cut}\n[truncated by vqro reply routing at ${maxBytes} bytes]`;
}

/** Assistant text blocks of one message, or "" when it has none. */
export function assistantText(message: any): string {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .filter((block: any) => block?.type === "text")
      .map((block: any) => block.text)
      .join("\n");
  }
  return "";
}

/**
 * True when a bash command's output contains one successful `vqro agent prompt`
 * response whose own target is `from`. The CLI prints each response as one
 * compact JSON line, so each line is parsed and bound on its own; a name from
 * one response cannot combine with a send to another. This avoids shell parsing.
 */
export function promptConfirmsOrigin(output: string, from: string): boolean {
  for (const line of output.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("{")) continue;
    try {
      const value = JSON.parse(trimmed);
      const agent = value?.result?.agent;
      if (value?.result?.type === "agent_prompted" && (agent?.name === from || agent?.pane_id === from)) {
        return true;
      }
    } catch {
      // not a single-line response; ignore it
    }
  }
  return false;
}

/** Render a claimed message with its host-attributed sender class/execution. */
export function renderMessage(message: any): string {
  let sender = "an agent";
  try {
    const binding = JSON.parse(message?.sender ?? "{}");
    if (binding.class === "agent") sender = `agent ${String(binding.execution ?? "unknown")}`;
    else if (binding.class === "human_unverified") sender = "a human";
  } catch {
    // keep the generic sender label
  }
  return `[vqro message ${message.id} from ${sender}]\n${message.body}`;
}

export default function (pi: ExtensionAPI) {
  const bin = process.env.VQRO_BIN_PATH;
  const pane = process.env.VQRO_PANE_ID;
  const enabled =
    process.env.VQRO_ENV === "1" &&
    !!process.env.VQRO_SOCKET_PATH &&
    !!bin &&
    path.isAbsolute(bin) &&
    !!pane &&
    process.env.HERDR_ENV !== "1" &&
    !process.env.HERDR_SOCKET_PATH &&
    !process.env.HERDR_CLIENT_SOCKET_PATH;

  // Frozen for the current run: set from the prompt that started it, cleared when
  // it settles, so a queued human follow-up cannot inherit or overwrite it.
  let runOrigin: Origin | null = null;
  let runText = "";
  let explicitSend = false;
  let self: string | undefined;
  let selfExecution: string | undefined;
  let poll: ReturnType<typeof setInterval> | undefined;
  let checking = false;
  let shutDown = false;
  let turnActive = false;
  let turnGeneration = 0;
  let latestCtx: any;
  let metadataSeq = Date.now() * 1000;

  pi.registerEntryRenderer?.("vqro-reply-routing", (entry: any, _options: any, theme: any) => {
    const data = entry?.data ?? {};
    const label =
      data.kind === "no-reply"
        ? `no reply to ${data.from}`
        : data.kind === "replied"
          ? `replied to ${data.from}`
          : `reply routing: ${data.kind ?? data.reason ?? "unknown"}`;
    return { render: () => [theme.fg("dim", `[vqro] ${label}`)] };
  });

  async function run(args: string[], timeout: number) {
    return pi.exec(bin!, args, { timeout }).catch(() => null);
  }

  /** This agent's name (or pane id); undefined when its identity cannot be proven. */
  async function resolveSelf(): Promise<string | undefined> {
    if (self) return self;
    const result = await run(["agent", "get", pane!], 2000);
    if (result?.code !== 0) return undefined;
    try {
      const agent = JSON.parse(result.stdout)?.result?.agent;
      if (!agent) return undefined;
      self = agent.name || pane!;
      selfExecution = agent.execution;
      return self;
    } catch {
      return undefined;
    }
  }

  async function claimMessages(idle: boolean): Promise<any[]> {
    if (!enabled) return [];
    const result = await run(idle ? ["message", "claim", "--idle"] : ["message", "claim"], 6000);
    if (!result || result.code !== 0) return [];
    try {
      return JSON.parse(result.stdout)?.claimed ?? [];
    } catch {
      return [];
    }
  }

  async function deliverClaimed(messages: any[], activate: boolean) {
    if (!messages.length) return;
    record({
      kind: "messages-claimed",
      ids: messages.map((message) => message.id),
    });
    if (!activate) return; // a starting turn injects them via before_agent_start
    const content = messages.map(renderMessage).join("\n\n");
    // Prefer the live SDK idle state; a queued followUp never wakes a settled agent.
    const idle = typeof latestCtx?.isIdle === "function" ? latestCtx.isIdle() : !turnActive;
    if (idle) pi.sendUserMessage(content);
    else pi.sendMessage({ customType: "vqro-message", content, display: true }, { deliverAs: "followUp" });
  }

  function stopPoll() {
    if (poll) clearInterval(poll);
    poll = undefined;
  }

  function startPoll() {
    if (!enabled || poll || !selfExecution || shutDown) return;
    poll = setInterval(checkMailbox, MAILBOX_POLL_MS);
  }

  /** With activation on, a waiting message addressed to this process starts a turn. */
  async function checkMailbox() {
    // Single-flight and turn-fenced: an in-flight poll must not deliver after a
    // turn has started or after the adapter is torn down.
    if (!enabled || !selfExecution || checking || shutDown) return;
    const generation = turnGeneration;
    checking = true;
    try {
      // Read-only preflight so an empty or policy-off poll records no operation.
      const listed = await run(["message", "list"], 4000);
      if (!listed || listed.code !== 0) return;
      let value: any;
      try {
        value = JSON.parse(listed.stdout);
      } catch {
        return;
      }
      if (value?.activation !== true || !(value.waiting?.length > 0)) return;
      if (shutDown || generation !== turnGeneration) return;
      const claimed = await claimMessages(true);
      if (!claimed.length) return;
      if (shutDown) {
        record({ kind: "messages-undelivered", ids: claimed.map((message) => message.id) });
        return;
      }
      await deliverClaimed(claimed, true);
    } finally {
      checking = false;
    }
  }

  async function reportNoReply(from: string) {
    await run(
      [
        "pane",
        "report-metadata",
        pane!,
        "--source",
        "vqro.reply-routing",
        "--state-label",
        `idle=no reply to ${from}`,
        "--seq",
        String(++metadataSeq),
        "--ttl-ms",
        String(METADATA_TTL_MS),
      ],
      2000,
    );
  }

  async function clearNoReply() {
    await run(
      ["pane", "report-metadata", pane!, "--source", "vqro.reply-routing", "--clear-state-labels"],
      2000,
    );
  }

  function record(data: Record<string, unknown>) {
    pi.appendEntry("vqro-reply-routing", { ...data, at: new Date().toISOString() });
  }

  pi.on("session_start", async (_event, ctx) => {
    latestCtx = ctx;
    if (!enabled) return;
    turnActive = false;
    await resolveSelf();
    if (shutDown) return;
    startPoll();
  });

  pi.on("session_shutdown", async () => {
    shutDown = true;
    stopPoll();
  });

  pi.on("before_agent_start", async (event, ctx) => {
    latestCtx = ctx;
    stopPoll();
    turnGeneration += 1;
    turnActive = true;
    runOrigin = parseProvenanceLabel(event.prompt);
    runText = "";
    explicitSend = false;
    // A turn is starting: pick up waiting durable messages into it.
    const claimed = await claimMessages(false);
    if (!claimed.length) return;
    if (shutDown) {
      record({ kind: "messages-undelivered", ids: claimed.map((message) => message.id) });
      return;
    }
    await deliverClaimed(claimed, false);
    return {
      message: {
        customType: "vqro-message",
        content: claimed.map(renderMessage).join("\n\n"),
        display: true,
      },
    };
  });

  pi.on("turn_end", async (event) => {
    // The run's final text is the last turn's text; an empty final turn means the
    // agent produced no reply, never a reuse of an earlier turn's text.
    runText = assistantText(event.message).trim();
  });

  pi.on("tool_result", async (event) => {
    // A2: only the CLI's own success response for the origin counts as sent.
    if (event.toolName !== "bash" || !runOrigin || explicitSend) return;
    if (promptConfirmsOrigin(assistantText({ content: event.content }), runOrigin.from)) {
      explicitSend = true;
    }
  });

  pi.on("agent_settled", async (_event, ctx) => {
    latestCtx = ctx;
    // Idle again: resume the bounded mailbox poll while activation is on.
    turnActive = false;
    startPoll();
    const current = runOrigin;
    const text = runText;
    runOrigin = null;
    runText = "";
    if (!enabled || !current || !current.replyExpected) return;
    const from = current.from;
    const selfIdentity = await resolveSelf();
    if (!selfIdentity) {
      // Fail closed rather than reply with unknown provenance.
      record({ kind: "self-unknown", from });
      return;
    }
    if (from === selfIdentity) return;
    if (explicitSend) {
      record({ kind: "explicit", from });
      return;
    }
    if (!text) {
      record({ kind: "no-reply", from });
      await reportNoReply(from);
      return;
    }
    const body = boundedReplyBytes(text);
    const result = await run(["agent", "prompt", from, body, "--no-reply-expected"], 10_000);
    if (result?.code === 0) {
      record({ kind: "replied", from, bytes: Buffer.byteLength(body, "utf8") });
      await clearNoReply();
    } else {
      record({ kind: "no-reply", from, reason: result ? `agent prompt exited ${result.code}` : "agent prompt unavailable" });
      await reportNoReply(from);
    }
  });
}
