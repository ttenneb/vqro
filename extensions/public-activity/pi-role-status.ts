import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import path from "node:path";

/** Exact, caller-supplied role text was observed in Pi's composed prompt *here*.
 * This is not final-provider attestation. No role file or private socket is read.
 */
export function roleObservation(expected: unknown, prompt: unknown): "requested" | "loaded-at-pi" {
  if (typeof expected !== "string" || !expected.trim() || Buffer.byteLength(expected) > 2048) return "requested";
  if (typeof prompt !== "string") return "requested";
  const first = prompt.indexOf(expected);
  return first >= 0 && prompt.indexOf(expected, first + 1) < 0 ? "loaded-at-pi" : "requested";
}

export default function (pi: ExtensionAPI) {
  pi.registerFlag("vqro-role-text", {
    description: "Exact role text to observe in Pi's composed prompt (not provider attestation)",
    type: "string",
  });
  let seq = Date.now() * 1000;
  let queued = Promise.resolve();
  const binary = process.env.VQRO_BIN_PATH;
  const pane = process.env.VQRO_PANE_ID;
  const enabled = process.env.VQRO_ENV === "1" && !!process.env.VQRO_SOCKET_PATH &&
    !!binary && path.isAbsolute(binary) && !!pane && /^w[\da-zA-Z]+:p[\da-zA-Z]+$/.test(pane) &&
    process.env.HERDR_ENV !== "1" && !process.env.HERDR_SOCKET_PATH && !process.env.HERDR_CLIENT_SOCKET_PATH;

  function publish(prompt: unknown, ctx: any) {
    const requested = pi.getFlag("vqro-role-text");
    if (typeof requested !== "string" || !requested.trim()) return;
    const state = roleObservation(requested, prompt);
    const label = state === "loaded-at-pi" ? "role loaded at Pi hook" : "role requested";
    ctx?.ui?.setStatus?.("vqro-role", label);
    if (!enabled) return;
    const args = ["pane", "report-metadata", pane!, "--source", "example.pi-role", "--agent", "pi",
      "--applies-to-source", "vqro:pi", "--seq", String(++seq), "--ttl-ms", "3600000",
      ...["idle", "working", "blocked", "unknown"].flatMap((status) => ["--state-label", `${status}=${label}`])];
    // Serialize reports; CLI responses are checked, no raw transport/ambient-file access.
    queued = queued.then(async () => {
      const result = await pi.exec(binary!, args, { timeout: 2000 });
      if (result.code !== 0) ctx?.ui?.setStatus?.("vqro-role", `${label} (Vqro report unavailable)`);
    }).catch(() => ctx?.ui?.setStatus?.("vqro-role", `${label} (Vqro report unavailable)`));
  }
  pi.on("session_start", (_event, ctx) => publish(ctx.getSystemPrompt(), ctx));
  pi.on("before_agent_start", (event, ctx) => publish(event.systemPrompt, ctx));
}
