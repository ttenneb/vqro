import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import path from "node:path";

/** Routed replies are capped so one turn cannot flood the origin pane's input. */
export const MAX_REPLY_BYTES = 8192;
/** The no-reply attention label is a volatile observation, not durable state. */
export const METADATA_TTL_MS = 600_000;

export type SenderKind = "human" | "agent" | "host";

export interface Origin {
  kind: SenderKind;
  /** Agent name, or pane id when the sender had no registered name. */
  from?: string;
  replyExpected: boolean;
}

/**
 * Parse the provenance label that `vqro agent prompt` writes when it runs inside
 * an agent pane. Absent or unrecognized text is human input. The label is
 * self-declared and spoofable in trusted same-user scope, not authenticated
 * caller identity.
 */
export function parseProvenanceLabel(text: string): Origin | null {
  if (/^\[host notice\]\s?/.test(text)) {
    return { kind: "host", replyExpected: false };
  }
  const match = /^\[agent ([^\]\s]+) · (reply expected|reply)\]\s?/.exec(text);
  if (!match) return null;
  return { kind: "agent", from: match[1], replyExpected: match[2] === "reply expected" };
}

export function boundedReplyBytes(text: string, maxBytes = MAX_REPLY_BYTES): string {
  const bytes = Buffer.from(text, "utf8");
  if (bytes.length <= maxBytes) return text;
  const cut = bytes.subarray(0, maxBytes).toString("utf8").replace(/\uFFFD+$/, "");
  return `${cut}\n[truncated by vqro reply routing at ${maxBytes} bytes]`;
}

/** The last assistant text on the active branch, or "" when the turn produced none. */
export function finalAssistantText(ctx: any): string {
  const branch = ctx?.sessionManager?.getBranch?.() ?? [];
  for (let index = branch.length - 1; index >= 0; index -= 1) {
    const entry = branch[index];
    if (entry?.type !== "message" || entry?.message?.role !== "assistant") continue;
    const content = entry.message.content;
    if (typeof content === "string") return content;
    if (Array.isArray(content)) {
      return content
        .filter((block: any) => block?.type === "text")
        .map((block: any) => block.text)
        .join("\n");
    }
  }
  return "";
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

  let origin: Origin | null = null;
  let explicitSend = false;
  let finished = false;
  let selfName: string | undefined;
  let metadataSeq = Date.now() * 1000;

  pi.registerEntryRenderer?.("vqro-reply-routing", (entry: any, _options: any, theme: any) => {
    const data = entry?.data ?? {};
    const label =
      data.kind === "no-reply"
        ? `no reply to ${data.from}`
        : data.kind === "replied"
          ? `replied to ${data.from}`
          : `reply routing: ${data.reason ?? "unknown"}`;
    return { render: () => [theme.fg("dim", `[vqro] ${label}`)] };
  });

  async function resolveSelf() {
    if (!enabled || selfName) return;
    const result = await pi.exec(bin!, ["agent", "get", pane!], { timeout: 2000 }).catch(() => null);
    if (result?.code === 0) {
      try {
        selfName = JSON.parse(result.stdout)?.result?.agent?.name ?? undefined;
      } catch {
        selfName = undefined;
      }
    }
  }

  async function run(args: string[], timeout: number) {
    return pi.exec(bin!, args, { timeout }).catch(() => null);
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
      [
        "pane",
        "report-metadata",
        pane!,
        "--source",
        "vqro.reply-routing",
        "--clear-state-labels",
      ],
      2000,
    );
  }

  pi.on("input", async (event) => {
    // Extension-injected input is neither a human gesture nor a routed agent
    // message; leave the current turn origin untouched.
    if (event.source === "extension") return { action: "continue" };
    origin = parseProvenanceLabel(event.text) ?? { kind: "human", replyExpected: false };
    explicitSend = false;
    finished = false;
    return { action: "continue" };
  });

  pi.on("tool_call", async (event) => {
    // A2: if the agent already messaged the origin during this turn, do not also
    // send its final text. Best-effort match; on an unclear command we still route.
    if (event.toolName !== "bash" || !origin?.from) return;
    const command = typeof event.input?.command === "string" ? event.input.command : "";
    if (/(^|[\s;&|])agent\s+prompt(\s|$)/.test(command) && command.includes(origin.from)) {
      explicitSend = true;
    }
  });

  pi.on("agent_settled", async (_event, ctx) => {
    const current = origin;
    origin = null;
    if (!enabled || finished || !current || current.kind !== "agent" || !current.replyExpected) return;
    finished = true;
    const from = current.from;
    if (!from) return;
    await resolveSelf();
    if (from === selfName || from === pane) return;
    if (explicitSend) {
      pi.appendEntry("vqro-reply-routing", { kind: "explicit", from, at: new Date().toISOString() });
      return;
    }
    const text = finalAssistantText(ctx).trim();
    if (!text) {
      pi.appendEntry("vqro-reply-routing", { kind: "no-reply", from, at: new Date().toISOString() });
      await reportNoReply(from);
      return;
    }
    const body = boundedReplyBytes(text);
    const result = await run(["agent", "prompt", from, body, "--reply"], 10_000);
    if (result?.code === 0) {
      pi.appendEntry("vqro-reply-routing", {
        kind: "replied",
        from,
        bytes: Buffer.byteLength(body, "utf8"),
        at: new Date().toISOString(),
      });
      await clearNoReply();
    } else {
      pi.appendEntry("vqro-reply-routing", {
        kind: "no-reply",
        from,
        reason: result ? `agent prompt exited ${result.code}` : "agent prompt unavailable",
        at: new Date().toISOString(),
      });
      await reportNoReply(from);
    }
  });
}
