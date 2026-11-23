// Cooperative setup evidence, not a veto on prose or a native lifecycle policy.
// No provider requests, inbox calls, automatic continuation or command replay.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const KEY = "vqro-testing";
const RPC_RULE_NOTICE = "NOT VERIFIED: rule not active for vqro-launched Pi agents yet";
const GUIDANCE = "When providing evidence, include receipts with their actual verdicts and coverage gaps; never label missing, failed or stale evidence a pass. Performance is recommended only for hot paths and never blocks. This cooperative guard cannot prevent prose claims of done.";
const UNIT_STATES = new Set(["pass", "fail", "error", "unverified", "missing", "stale", "pending"]);
const INTEGRATION_STATES = new Set(["pass", "fail", "skipped", "unverified", "missing", "pending", "stale"]);

type Status = {
  setup_id: string; generation: number; require_unit: boolean; finish_ok: boolean;
  policy_state?: "on" | "off" | "unknown";
  instance: { socket: string | null; state_dir: string | null; extensions_dir: string; epoch: string | null };
  coverage_gaps: string[];
  unit: { state: string; receipt: string | null; verdict: unknown; current?: boolean; exception: null | {
    reason: string; setup_id: string; generation: number; accepted_by: string;
  } };
  integration: { state: string; receipt: string | null; skip_reason: string | null; current?: boolean };
  performance: { recommended: boolean; why: string; state: string };
  not_verified_reasons: string[];
};
type Result = { code: number | null; stdout: string; stderr: string };
type Dependencies = {
  status: () => Promise<unknown>;
  unit: () => Promise<void>;
  stderr?: (message: string) => void;
};

type Policy = Pick<Status, "require_unit" | "policy_state" | "not_verified_reasons">;

function policyEnvelope(value: any): Policy | undefined {
  if (!value || typeof value.require_unit !== "boolean" || !Array.isArray(value.not_verified_reasons) ||
      !value.not_verified_reasons.every((reason: unknown) => typeof reason === "string") ||
      value.policy_state !== undefined && (!["on", "off", "unknown"].includes(value.policy_state) ||
        value.policy_state === "on" && !value.require_unit || value.policy_state === "off" && value.require_unit)) return;
  return { require_unit: value.require_unit, policy_state: value.policy_state, not_verified_reasons: value.not_verified_reasons };
}

function isErrorStatus(value: any): boolean {
  const exact = (object: any, keys: string[]) => object !== null && typeof object === "object" && !Array.isArray(object) &&
    Object.keys(object).length === keys.length && keys.every(key => Object.hasOwn(object, key));
  if (!exact(value, ["kind", "version", "setup_id", "generation", "require_unit", "policy_state", "unit", "integration",
      "performance", "finish_ok", "not_verified_reasons", "coverage_gaps", "instance", "error"]) ||
      value.kind !== "vqro.setup-status-error" || value.version !== 1 || value.setup_id !== null || value.generation !== 0 ||
      value.instance !== null || value.finish_ok !== false || typeof value.error !== "string" || !value.error.trim() ||
      [...value.error].length > 300 || !Array.isArray(value.coverage_gaps) || value.coverage_gaps.length !== 0 ||
      !["on", "off", "unknown"].includes(value.policy_state) || !policyEnvelope(value) ||
      value.require_unit !== (value.policy_state === "on")) return false;
  const reasons = JSON.stringify(value.not_verified_reasons);
  return (reasons === JSON.stringify([value.error]) || reasons === JSON.stringify([RPC_RULE_NOTICE, value.error])) &&
    exact(value.unit, ["state", "receipt", "verdict", "exception"]) && value.unit.state === "error" &&
    value.unit.receipt === null && value.unit.verdict === null && value.unit.exception === null &&
    exact(value.integration, ["state", "receipt", "skip_reason"]) && value.integration.state === "unverified" &&
    value.integration.receipt === null && value.integration.skip_reason === null &&
    exact(value.performance, ["state", "recommended", "why"]) && value.performance.state === "unverified" &&
    value.performance.recommended === true && value.performance.why === "checker unavailable";
}

function policyState(status?: Policy): "ON" | "OFF" | "UNKNOWN" {
  if (!status || status.policy_state === "unknown" || status.not_verified_reasons.some(reason => reason.startsWith("require_unit setting is not boolean"))) return "UNKNOWN";
  return status.require_unit ? "ON" : "OFF";
}

function policyRule(status?: Policy): string {
  const conditional = "Follow the CURRENT authoritative testing policy at every step. This sampled policy label does not survive a policy change: later vqro-testing-policy context supersedes it. Only while current policy is ON is the finish step mandatory; OFF makes evidence recommended, and unknown policy must never be inferred OFF. ";
  if (policyState(status) === "ON") {
    return conditional + "Current setup policy ON: vqro_setup_finish is required before reporting a setup change finished. Stale/NOT VERIFIED receipts do not soften this duty. Attach current Unit evidence or request a human-accepted exception, plus Integration evidence or an explicit skip reason. " + GUIDANCE;
  }
  const state = policyState(status) === "OFF" ? "OFF" : "UNKNOWN/UNAVAILABLE";
  return conditional + `Current setup policy ${state}: Unit and Integration evidence are recommended, not unconditionally required by this sample. You may use vqro_setup_finish to inspect evidence. Recommended reporting: attach available receipts or explain why checks were skipped/unavailable; refresh unknown policy before claiming verified completion. ` + GUIDANCE;
}

function parseStatus(value: any): Status {
  if (value?.setup_id === null) throw new Error("setup identity is unknown: NOT VERIFIED");
  if (!value || typeof value.setup_id !== "string" || !/^[a-f0-9]{64}$/.test(value.setup_id) || !Number.isSafeInteger(value.generation) || value.generation < 0 ||
      typeof value.require_unit !== "boolean" || typeof value.finish_ok !== "boolean" ||
      !UNIT_STATES.has(value.unit?.state) || !INTEGRATION_STATES.has(value.integration?.state) ||
      !Array.isArray(value.not_verified_reasons) || !value.not_verified_reasons.every((v: unknown) => typeof v === "string") ||
      typeof value.performance?.recommended !== "boolean" || typeof value.performance?.why !== "string") {
    throw new Error("invalid test.setup.status response");
  }
  if (value.policy_state !== undefined && (!["on", "off", "unknown"].includes(value.policy_state) ||
      value.policy_state === "on" && !value.require_unit || value.policy_state === "off" && value.require_unit)) throw new Error("invalid policy state");
  const instance = value.instance;
  if (!instance || typeof instance !== "object" || !instance.extensions_dir || typeof instance.extensions_dir !== "string" ||
      ![instance.socket, instance.state_dir, instance.epoch].every(item => item === null || typeof item === "string") ||
      !Array.isArray(value.coverage_gaps) || !value.coverage_gaps.every((item: unknown) => typeof item === "string")) {
    throw new Error("invalid setup instance or coverage gaps");
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
  const unitOK = (value.unit.state === "pass" && value.unit.current === true && !!value.unit.receipt) || exception !== null;
  const integrationOK = (["pass", "fail", "unverified"].includes(value.integration.state) &&
    value.integration.current === true && !!value.integration.receipt) ||
    (value.integration.state === "skipped" && !!value.integration.skip_reason?.trim());
  if (value.finish_ok && policyState(value) === "UNKNOWN") throw new Error("unknown policy cannot approve finish");
  if (value.finish_ok && value.require_unit && !(unitOK && integrationOK)) throw new Error("inconsistent finish approval");
  return value;
}

function token(status: Status): string {
  const { socket, state_dir, extensions_dir, epoch } = status.instance;
  return JSON.stringify([socket, state_dir, extensions_dir, epoch, status.setup_id, status.generation]);
}

function summary(status?: Status, error?: string): string {
  if (!status) return `Setup change: Unit NOT VERIFIED (${error || "checker unavailable"})`;
  const verdict = status.unit.state === "pass" && status.unit.current === true ? "PASS" : status.unit.state === "fail" ? "FAIL" : "NOT VERIFIED";
  const older = "NOT VERIFIED: result is from an older setup";
  const receipt = (status.unit.receipt ? `receipt ${status.unit.receipt}` : "no current receipt") +
    (status.unit.state === "stale" ? `; ${older}` : "");
  const integration = status.integration.state.toUpperCase() +
    (status.integration.state === "stale" ? ` (${older})` : "") +
    (status.integration.receipt ? ` (receipt ${status.integration.receipt})` : "") +
    (status.integration.skip_reason ? `: ${status.integration.skip_reason}` : "");
  const scope = status.not_verified_reasons.find(reason => reason === "NOT VERIFIED: rule not active for vqro-launched Pi agents yet");
  const gaps = status.coverage_gaps.length ? ` [Coverage gaps: ${status.coverage_gaps.join("; ")}]` : "";
  return `Setup change: ${scope ? `[${scope}] · ` : ""}Unit ${verdict}${gaps} (${receipt}); Integration ${integration}; ${status.finish_ok ? "finish evidence accepted" : "NOT VERIFIED: finish refused"}` +
    (status.unit.exception ? `; human exception: ${status.unit.exception.reason}` : "") +
    (policyState(status) === "OFF" ? "; Unit requirement OFF" : policyState(status) === "UNKNOWN" ? "; Unit policy UNKNOWN" : "") +
    (["fail", "error", "unverified", "stale", "pending"].includes(status.performance.state) ?
      `; Performance ${status.performance.state.toUpperCase()}${status.performance.state === "stale" ? ` (${older})` : ""} (never blocking)` : "") +
    (status.performance.recommended ? `; Performance recommended: ${status.performance.why} (never blocking)` : "");
}

// Exported dependency seam: model-free tests exercise the actual hooks, not a reimplementation.
export function installTesting(pi: any, deps: Dependencies) {
  let previous: string | undefined;
  let attempted: string | undefined;
  let current: Status | undefined;
  let policy: Policy | undefined;
  let failedAttempt: { identity: string; receipt: string | null } | undefined;
  let failure: string | undefined;
  let notice: string | undefined;
  let reporting = false;
  let livePolicyContext: string | undefined;
  let serial: Promise<void> = Promise.resolve();
  const stderr = deps.stderr || ((message: string) => process.stderr.write(message + "\n"));

  function show(ctx: any, message: string) {
    notice = message;
    if (ctx?.hasUI) ctx.ui.setStatus(KEY, message);
    else stderr(message); // Never corrupt RPC/JSON stdout or rely on no-op print-mode UI.
  }

  async function assessment(): Promise<Status> {
    let value: unknown;
    try { value = await deps.status(); }
    catch (error) { policy = undefined; throw error; }
    policy = policyEnvelope(value); // Receipt/identity errors do not make a known ON policy optional.
    if (isErrorStatus(value)) throw new Error(`setup evidence unavailable (exit 2): ${(value as any).error}`);
    return parseStatus(value);
  }

  async function inspect(ctx: any, at: string) {
    const priorPolicy = policyState(policy);
    show(ctx, "Setup testing: Unit NOT VERIFIED (checking evidence)");
    try {
      const status = await assessment();
      const identity = token(status);
      if (failedAttempt && (identity !== failedAttempt.identity ||
          status.unit.current === true && status.unit.receipt !== failedAttempt.receipt)) failedAttempt = undefined;
      if (failedAttempt && status.require_unit) throw new Error("latest Unit attempt is NOT VERIFIED; an older receipt cannot authorize finish");
      const changed = at !== "baseline" && identity !== previous;
      reporting ||= changed || at === "finish";
      current = status;
      failure = undefined;
      if (at !== "baseline" && attempted !== identity &&
          (changed || (at === "finish" && status.require_unit && ["missing", "stale", "unverified"].includes(status.unit.state)))) {
        // Record the attempt before invocation: timeout/EOF never causes an automatic replay.
        attempted = identity;
        show(ctx, "Setup change: Unit NOT VERIFIED (Unit running)");
        failedAttempt = { identity, receipt: status.unit.receipt };
        await deps.unit();
        current = await assessment();
        if (token(current) !== identity) throw new Error("setup changed during Unit; fresh evidence required");
        failedAttempt = undefined;
      }
      previous = identity;
      if (policyState(policy) !== priorPolicy) livePolicyContext = policyRule(policy);
      show(ctx, reporting ? summary(current) : summary(current).replace("Setup change:", "Setup testing:"));
    } catch (error) {
      current = undefined;
      failure = String(error);
      if (policyState(policy) !== priorPolicy) livePolicyContext = policyRule(policy);
      show(ctx, summary(undefined, failure));
    }
  }

  function check(ctx: any, at: string) {
    serial = serial.then(() => inspect(ctx, at)).catch((error) => {
      current = undefined;
      policy = undefined;
      failure = String(error);
      // A broken UI transport must not turn a rejected finish into success.
      notice = summary(undefined, failure);
      livePolicyContext = policyRule(undefined);
      stderr(notice);
    });
    return serial;
  }

  pi.on("session_start", async (_event: any, ctx: any) => {
    await serial;
    previous = attempted = undefined;
    reporting = false;
    livePolicyContext = undefined;
    current = undefined;
    policy = undefined;
    failedAttempt = undefined;
    await check(ctx, "baseline");
  });
  pi.on("before_agent_start", async (event: any, ctx: any) => {
    await check(ctx, "prompt"); // Do not impose a cached or unavailable policy on the next run.
    livePolicyContext = undefined; // The new run-start sample already describes this observation.
    return { systemPrompt: `${event.systemPrompt}\n\n${policyRule(policy)}` };
  });
  pi.on("context", async (event: any) => {
    if (!livePolicyContext) return;
    // Public per-request context, not a queued steer/follow-up or a provider turn.
    // Keep it across retries; replace only our own earlier ephemeral context.
    return { messages: [...event.messages.filter((message: any) => !(message.role === "custom" && message.customType === "vqro-testing-policy")),
      { role: "custom", customType: "vqro-testing-policy", content: livePolicyContext, display: true, timestamp: Date.now() }] };
  });
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
    description: "Inspect current setup policy/evidence. ON requires the finish step even with stale evidence; OFF recommends evidence; unknown policy is not OFF or a pass. Follow later policy context updates. No prose veto.",
    parameters: { type: "object", properties: {}, additionalProperties: false },
    async execute(_id: string, _args: unknown, _signal: unknown, _update: unknown, ctx: any) {
      await check(ctx, "finish");
      const details = { finish_ok: current?.finish_ok === true, status: current || null, error: failure || null,
        summary: summary(current, failure) };
      // Throw for an unambiguous failed tool result; do not rely on version-specific return flags.
      if (!details.finish_ok) throw new Error(JSON.stringify(details));
      return { content: [{ type: "text", text: JSON.stringify(details) }], details };
    },
  });
  // Native get_commands reports this command's source path only after all hooks/tool are registered.
  pi.registerCommand("vqro-testing-status", {
    description: "Inspect this launch's vqro setup testing evidence (no model call)",
    handler: async (_args: string, ctx: any) => { await check(ctx, "baseline"); },
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
  const extensions = resolve(process.env.VQRO_TESTING_EXTENSIONS_DIR || process.env.VQRO_EXTENSIONS_DIR ||
    (existsSync(join(here, "..", "testing_unit.py")) ? join(here, "..") : join(here, "..", "..", "extensions")));
  const state = process.env.VQRO_TESTING_STATE_DIR || process.env.VQRO_STATE_DIR;
  const socket = process.env.VQRO_TESTING_SOCKET || process.env.VQRO_SOCKET;
  const script = join(extensions, "testing_unit.py");
  const selectors = ["--extensions-dir", extensions, "--socket", socket || "", "--state-dir", state || ""];
  const rpc = process.argv.some((arg, index) => arg === "--mode=rpc" || arg === "--mode" && process.argv[index + 1] === "rpc");
  const scope = rpc ? ["--rpc-principal", process.env.VQRO_PRINCIPAL || ""] : [];
  const selected = () => {
    if (!socket || !state) throw new Error("explicit vqro socket and state directory required; refusing default-install fallback");
  };
  installTesting(pi, {
    async status() {
      selected();
      const result = await execute(script, ["--setup-status", "--json", ...selectors, ...scope], 30_000);
      if (result.code !== 0 && result.code !== 2) throw new Error(`setup status unavailable: exit ${result.code}: ${result.stderr}`);
      const value = JSON.parse(result.stdout);
      if (result.code === 2 && !isErrorStatus(value)) throw new Error("invalid setup error-status document on exit 2");
      return value; // Exit 2 can convey fresh policy, never a successful evidence assessment.
    },
    async unit() {
      selected();
      const result = await execute(script, ["--json", "--record", ...selectors], 180_000);
      if (![0, 1, 77].includes(result.code ?? -1)) throw new Error(`Unit harness unavailable: exit ${result.code}: ${result.stderr}`);
      const receipt = JSON.parse(result.stdout);
      if (receipt.kind !== "vqro.test-receipt" || receipt.suite !== "unit") throw new Error("invalid Unit receipt");
      if (receipt.recorded !== true) throw new Error("Unit recording unsupported; finish evidence NOT VERIFIED");
      // The subsequent status call must adopt/validate the receipt; stdout alone never grants finish.
    },
  });
}
