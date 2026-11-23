// Cooperative setup evidence, not a veto on prose or a native lifecycle policy.
// No provider requests, inbox calls, automatic continuation or command replay.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const KEY = "vqro-testing";
const RULE = "For a vqro setup change, use vqro_setup_finish before reporting completion. Attach the current Unit receipt (or ask the human to accept failure); attach Integration evidence or explicitly record why skipped. Missing/error/stale evidence is NOT VERIFIED. Performance is only recommended for hot paths and never blocks. This guard cannot prevent prose claims of done.";
const UNIT_STATES = new Set(["pass", "fail", "error", "unverified", "missing", "stale", "pending"]);
const INTEGRATION_STATES = new Set(["pass", "fail", "skipped", "unverified", "missing", "pending"]);

type Status = {
  setup_id: string; generation: number; require_unit: boolean; finish_ok: boolean;
  unit: { state: string; receipt: string | null; verdict: unknown; exception: null | {
    reason: string; setup_id: string; generation: number; accepted_by: string;
  } };
  integration: { state: string; receipt: string | null; skip_reason: string | null };
  performance: { recommended: boolean; why: string; state: string };
  not_verified_reasons: string[];
};
type Result = { code: number | null; stdout: string; stderr: string };
type Dependencies = {
  status: () => Promise<unknown>;
  unit: () => Promise<void>;
  stderr?: (message: string) => void;
};

function parseStatus(value: any): Status {
  if (!value || typeof value.setup_id !== "string" || !/^[a-f0-9]{64}$/.test(value.setup_id) || !Number.isSafeInteger(value.generation) || value.generation < 0 ||
      typeof value.require_unit !== "boolean" || typeof value.finish_ok !== "boolean" ||
      !UNIT_STATES.has(value.unit?.state) || !INTEGRATION_STATES.has(value.integration?.state) ||
      !Array.isArray(value.not_verified_reasons) || !value.not_verified_reasons.every((v: unknown) => typeof v === "string") ||
      typeof value.performance?.recommended !== "boolean" || typeof value.performance?.why !== "string") {
    throw new Error("invalid test.setup.status response");
  }
  for (const receipt of [value.unit.receipt, value.integration.receipt]) {
    if (receipt !== null && (typeof receipt !== "string" || !receipt)) throw new Error("invalid receipt reference");
  }
  if (value.integration.skip_reason !== null && typeof value.integration.skip_reason !== "string") throw new Error("invalid skip reason");
  const exception = value.unit.exception;
  if (exception !== null && (!exception || !exception.reason?.trim() || exception.setup_id !== value.setup_id ||
      exception.generation !== value.generation || typeof exception.accepted_by !== "string" || !exception.accepted_by)) {
    throw new Error("invalid or stale user exception");
  }
  const unitOK = (value.unit.state === "pass" && !!value.unit.receipt) || exception !== null;
  const integrationOK = !!value.integration.receipt || !!value.integration.skip_reason?.trim();
  if (value.finish_ok && value.require_unit && !(unitOK && integrationOK)) throw new Error("inconsistent finish approval");
  return value;
}

function token(status: Status): string { return `${status.setup_id}:${status.generation}`; }

function summary(status?: Status, error?: string): string {
  if (!status) return `Setup change: Unit NOT VERIFIED (${error || "checker unavailable"})`;
  const verdict = status.unit.state === "pass" ? "PASS" : status.unit.state === "fail" ? "FAIL" : "NOT VERIFIED";
  const receipt = status.unit.receipt ? `receipt ${status.unit.receipt}` : "no current receipt";
  const integration = status.integration.state.toUpperCase() +
    (status.integration.receipt ? ` (receipt ${status.integration.receipt})` : "") +
    (status.integration.skip_reason ? `: ${status.integration.skip_reason}` : "");
  return `Setup change: Unit ${verdict} (${receipt}); Integration ${integration}; ${status.finish_ok ? "finish evidence accepted" : "NOT VERIFIED: finish refused"}` +
    (status.unit.exception ? `; human exception: ${status.unit.exception.reason}` : "") +
    (status.require_unit ? "" : "; Unit requirement OFF") +
    (status.performance.recommended ? `; Performance recommended: ${status.performance.why} (never blocking)` : "");
}

// Exported dependency seam: model-free tests exercise the actual hooks, not a reimplementation.
export function installTesting(pi: any, deps: Dependencies) {
  let previous: string | undefined;
  let attempted: string | undefined;
  let current: Status | undefined;
  let failure: string | undefined;
  let notice: string | undefined;
  let reporting = false;
  let serial: Promise<void> = Promise.resolve();
  const stderr = deps.stderr || ((message: string) => process.stderr.write(message + "\n"));

  function show(ctx: any, message: string) {
    notice = message;
    if (ctx?.hasUI) ctx.ui.setStatus(KEY, message);
    else stderr(message); // Never corrupt RPC/JSON stdout or rely on no-op print-mode UI.
  }

  async function inspect(ctx: any, at: string) {
    show(ctx, "Setup testing: Unit NOT VERIFIED (checking evidence)");
    try {
      const status = parseStatus(await deps.status());
      const identity = token(status);
      const changed = at !== "baseline" && identity !== previous;
      reporting ||= changed || at === "finish";
      current = status;
      failure = undefined;
      if (at !== "baseline" && attempted !== identity &&
          (changed || (at === "finish" && status.require_unit && ["missing", "stale", "unverified"].includes(status.unit.state)))) {
        // Record the attempt before invocation: timeout/EOF never causes an automatic replay.
        attempted = identity;
        show(ctx, "Setup change: Unit NOT VERIFIED (Unit running)");
        await deps.unit();
        current = parseStatus(await deps.status());
        if (token(current) !== identity) throw new Error("setup changed during Unit; fresh evidence required");
      }
      previous = identity;
      show(ctx, reporting ? summary(current) : summary(current).replace("Setup change:", "Setup testing:"));
    } catch (error) {
      current = undefined;
      failure = String(error);
      show(ctx, summary(undefined, failure));
    }
  }

  function check(ctx: any, at: string) {
    serial = serial.then(() => inspect(ctx, at)).catch((error) => {
      current = undefined;
      failure = String(error);
      // A broken UI transport must not turn a rejected finish into success.
      notice = summary(undefined, failure);
      stderr(notice);
    });
    return serial;
  }

  pi.on("session_start", async (_event: any, ctx: any) => {
    await serial;
    previous = attempted = undefined;
    reporting = false;
    current = undefined;
    await check(ctx, "baseline");
  });
  pi.on("before_agent_start", async (event: any) => ({ systemPrompt: `${event.systemPrompt}\n\n${RULE}` }));
  pi.on("tool_result", async (event: any, ctx: any) => {
    // Shell parsing is not reliable: conservatively rescan after any bash/edit/write.
    // Other/custom tools are caught by the mandatory final-boundary rescan.
    if (["bash", "edit", "write", "vqro_call", "vqro_run"].includes(event.toolName?.split(".").pop())) await check(ctx, "tool");
  });
  pi.on("agent_before_settle", async (event: any, ctx: any) => {
    await check(ctx, "settle");
    if (!reporting && !failure) return;
    reporting = false;
    const content = notice || summary(undefined, failure);
    notice = undefined;
    return { entries: [...event.entries, { type: "custom_message", customType: KEY, content, display: true,
      details: { finish_ok: current?.finish_ok === true, status: current || null, error: failure || null } }] };
    // Deliberately no continue field: preserve other plugins' proposals without requesting inference.
  });
  pi.registerTool({
    name: "vqro_setup_finish", label: "Finish vqro setup change",
    description: "Validate current Unit and Integration evidence before claiming a vqro setup change finished. Does not prevent prose claims; requirement defaults off until the human enables it.",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    async execute(_id: string, _args: unknown, _signal: unknown, _update: unknown, ctx: any) {
      await check(ctx, "finish");
      const details = { finish_ok: current?.finish_ok === true, status: current || null, error: failure || null,
        summary: summary(current, failure) };
      // Pi requires execute to THROW to mark tool failure; returning isError would be ignored.
      if (!details.finish_ok) throw new Error(JSON.stringify(details));
      return { content: [{ type: "text", text: JSON.stringify(details) }], details };
    },
  });
}

function execute(script: string, args: string[], timeout: number): Promise<Result> {
  return new Promise((done, reject) => {
    // No shell and no inherited Herdr identity. Keep only the explicit setup selectors and tool PATH.
    const env: NodeJS.ProcessEnv = { PATH: process.env.PATH, HOME: process.env.HOME };
    for (const name of ["XDG_STATE_HOME", "VQRO_STATE_DIR", "VQRO_SOCKET", "VQRO_SOCKET_SOURCE", "VQRO_PRINCIPAL"]) {
      if (process.env[name]) env[name] = process.env[name];
    }
    const grouped = process.platform !== "win32";
    const child = spawn("python3", ["-B", script, ...args], { env, detached: grouped, stdio: ["ignore", "pipe", "pipe"] });
    const stop = () => {
      // On Unix the helper and its unittest children share our owned process group.
      if (grouped && child.pid) { try { process.kill(-child.pid, "SIGKILL"); return; } catch {} }
      child.kill("SIGKILL");
    };
    let stdout = "", stderr = "", failed = false;
    const timer = setTimeout(() => { failed = true; stop(); reject(new Error("testing helper timed out; outcome unknown, not retried")); }, timeout);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      if (Buffer.byteLength(stdout) > 1024 * 1024) { failed = true; stop(); reject(new Error("testing helper output exceeds bound")); }
    });
    child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-4096); });
    child.on("close", (code) => { clearTimeout(timer); if (!failed) done({ code, stdout, stderr }); });
  });
}

export default function (pi: any) {
  const here = dirname(fileURLToPath(import.meta.url));
  const extensions = resolve(process.env.VQRO_EXTENSIONS_DIR ||
    (existsSync(join(here, "..", "testing_unit.py")) ? join(here, "..") : join(here, "..", "..", "extensions")));
  const state = process.env.VQRO_TESTING_STATE_DIR || process.env.VQRO_STATE_DIR;
  const socket = process.env.VQRO_TESTING_SOCKET || process.env.VQRO_SOCKET;
  const script = join(extensions, "testing_unit.py");
  const selectors = ["--extensions-dir", extensions, "--socket", socket || "", "--state-dir", state || ""];
  const selected = () => {
    if (!socket || !state) throw new Error("explicit vqro socket and state directory required; refusing default-install fallback");
  };
  installTesting(pi, {
    async status() {
      selected();
      const result = await execute(script, ["--setup-status", "--json", ...selectors], 30_000);
      if (result.code !== 0) throw new Error(`setup status unavailable: exit ${result.code}: ${result.stderr}`);
      return JSON.parse(result.stdout);
    },
    async unit() {
      selected();
      const result = await execute(script, ["--json", "--record", ...selectors], 180_000);
      if (![0, 1, 77].includes(result.code ?? -1)) throw new Error(`Unit harness unavailable: exit ${result.code}: ${result.stderr}`);
      const receipt = JSON.parse(result.stdout);
      if (receipt.kind !== "vqro.test-receipt" || receipt.suite !== "unit") throw new Error("invalid Unit receipt");
      // The subsequent status call must adopt/validate the receipt; stdout alone never grants finish.
    },
  });
}
