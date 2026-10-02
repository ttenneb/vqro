import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import path from "node:path";

/** Routed replies are capped so one turn cannot flood the origin pane's input. */
export const MAX_REPLY_BYTES = 8192;
/** The no-reply attention label is a volatile observation, not durable state. */
export const METADATA_TTL_MS = 600_000;

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
      return self;
    } catch {
      return undefined;
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

  pi.on("before_agent_start", async (event) => {
    runOrigin = parseProvenanceLabel(event.prompt);
    runText = "";
    explicitSend = false;
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

  pi.on("agent_settled", async () => {
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
