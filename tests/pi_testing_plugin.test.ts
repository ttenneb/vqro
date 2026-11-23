import { describe, test, expect } from "bun:test";
import plugin, { installTesting } from "../integrations/pi/vqro-testing";
import { mkdtempSync, readFileSync, writeFileSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function status(id = "a", generation = 0, patch: any = {}): any {
  return { setup_id: id.repeat(64), generation, require_unit: true, finish_ok: true,
    instance: { socket: "/private/core.sock", state_dir: "/private/state", extensions_dir: "/private/ext", epoch: "epoch-1" }, coverage_gaps: [],
    unit: { state: "pass", receipt: "/private/unit.json", verdict: "pass", current: true, exception: null },
    integration: { state: "pass", receipt: "/private/integration.json", skip_reason: null, current: true },
    performance: { recommended: false, why: "", state: "missing" }, not_verified_reasons: [], ...patch };
}

function fixture(initial: any = status()) {
  const handlers = new Map<string, Function>();
  const tools = new Map<string, any>();
  const statuses: string[] = [], stderr: string[] = [], calls: string[] = [];
  let value = initial, units = 0, providerRequests = 0;
  let unit: () => Promise<void> = async () => {
    value = status(value.setup_id[0], value.generation);
  };
  let read: () => Promise<unknown> = async () => value;
  const pi = {
    on: (event: string, handler: Function) => handlers.set(event, handler),
    registerTool: (tool: any) => tools.set(tool.name, tool),
    sendMessage: () => { providerRequests++; throw new Error("must not queue model work"); },
    sendUserMessage: () => { providerRequests++; throw new Error("must not queue model work"); },
  };
  const ctx: any = { hasUI: true, ui: { setStatus: (_key: string, message: string) => statuses.push(message) } };
  installTesting(pi, {
    status: async () => { calls.push("status"); return read(); },
    unit: async () => { calls.push("unit"); units++; await unit(); },
    stderr: (message) => stderr.push(message),
  });
  return {
    ctx, statuses, stderr, calls, handlers,
    get units() { return units; }, get providerRequests() { return providerRequests; },
    set: (next: any) => { value = next; },
    unit: (next: () => Promise<void>) => { unit = next; },
    read: (next: () => Promise<unknown>) => { read = next; },
    emit: (event: string, data: any = {}) => handlers.get(event)!(data, ctx),
    finish: () => tools.get("vqro_setup_finish").execute("call", {}, undefined, undefined, ctx),
  };
}

describe("model-free Pi testing policy", () => {
  test("real CLI transport uses explicit selectors and --record, with no chosen filename", async () => {
    const root = mkdtempSync(join(tmpdir(), "pi-testing-cli-"));
    const names = ["VQRO_EXTENSIONS_DIR", "VQRO_TESTING_EXTENSIONS_DIR", "VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR", "HERDR_SOCKET", "VQRO_EXTENSION_TOKEN"];
    const saved = Object.fromEntries(names.map(name => [name, process.env[name]]));
    try {
      const ext = join(root, "ext"); mkdirSync(ext);
      const state = join(root, "state"); mkdirSync(state);
      process.env.VQRO_EXTENSIONS_DIR = join(root, "wrong-ambient-extensions");
      process.env.VQRO_TESTING_EXTENSIONS_DIR = ext;
      process.env.VQRO_TESTING_SOCKET = "opaque:selected-test-endpoint";
      process.env.VQRO_TESTING_STATE_DIR = state;
      process.env.HERDR_SOCKET = "must-not-leak";
      process.env.VQRO_EXTENSION_TOKEN = "must-not-leak";
      writeFileSync(join(ext, "testing_unit.py"), `import json, os, pathlib, sys
root=pathlib.Path(__file__).parent
args=sys.argv[1:]
assert args[args.index('--socket')+1]=='opaque:selected-test-endpoint'
assert args[args.index('--state-dir')+1]==str(root.parent/'state')
assert not any(k.startswith(('HERDR_', 'HC_')) or k=='VQRO_EXTENSION_TOKEN' for k in os.environ)
with (root/'calls').open('a') as f: f.write(json.dumps(args)+'\\n')
generation=int((root/'generation').read_text()) if (root/'generation').exists() else 0
if '--record' in args:
 assert '--out' not in args
 (root/'recorded').write_text(str(generation))
 print(json.dumps({'kind':'vqro.test-receipt','suite':'unit','receipt':'checker-owned.json'}))
else:
 passed=not generation or (root/'recorded').exists()
 print(json.dumps({'setup_id':('a' if generation==0 else 'b')*64,'generation':generation,'require_unit':True,'finish_ok':passed,
 'unit':{'state':'pass' if passed else 'stale','receipt':'checker-owned.json' if passed else None,'verdict':'pass' if passed else 'unverified','exception':None,'current':passed},
 'integration':{'state':'skipped','receipt':None,'skip_reason':'fixture only'},
 'performance':{'recommended':False,'why':'','state':'missing'},'not_verified_reasons':[], 'coverage_gaps':[],
 'instance':{'socket':'opaque:selected-test-endpoint','state_dir':str(root.parent/'state'),'extensions_dir':str(root),'epoch':'fixture'}}))
`);
      const handlers = new Map<string, Function>(), tools = new Map<string, any>();
      plugin({ on: (name: string, fn: Function) => handlers.set(name, fn), registerTool: (tool: any) => tools.set(tool.name, tool) });
      const ctx = { hasUI: true, ui: { setStatus() {} } };
      await handlers.get("session_start")!({}, ctx);
      writeFileSync(join(ext, "generation"), "1");
      await handlers.get("tool_result")!({ toolName: "write" }, ctx);
      const result = await tools.get("vqro_setup_finish").execute("call", {}, null, null, ctx);
      expect(result.details.finish_ok).toBe(true);
      const calls = readFileSync(join(ext, "calls"), "utf8").trim().split("\n").map(line => JSON.parse(line));
      expect(calls.filter(args => args.includes("--record"))).toHaveLength(1);
      expect(calls.every(args => !args.includes("--out"))).toBe(true);
      expect(calls.filter(args => args.includes("--setup-status"))).toHaveLength(4);
    } finally {
      for (const name of names) { if (saved[name] === undefined) delete process.env[name]; else process.env[name] = saved[name]; }
      rmSync(root, { recursive: true, force: true });
    }
  });
  test("baseline, dirty tool, Unit, status, final entry; never an additional provider request", async () => {
    const f = fixture();
    await f.emit("session_start");
    expect(f.units).toBe(0);
    const rule = await f.emit("before_agent_start", { systemPrompt: "original" });
    expect(rule.systemPrompt).toStartWith("original");
    expect(rule.systemPrompt).toContain("vqro_setup_finish");
    f.set(status("b", 1));
    await f.emit("tool_result", { toolName: "write" });
    expect(f.calls).toEqual(["status", "status", "unit", "status"]);
    const existing = { type: "custom", customType: "inbox-control", data: {} };
    const final = await f.emit("agent_before_settle", { entries: [existing], continue: true });
    expect(final.entries[0]).toBe(existing);
    expect(final.entries.at(-1).display).toBe(true);
    expect(final.entries.at(-1).content).toContain("Unit PASS");
    expect(final).not.toHaveProperty("continue");
    expect(f.units).toBe(1);
    expect(f.providerRequests).toBe(0);
    expect((await f.finish()).details.finish_ok).toBe(true);
  });

  test("unchanged conversations do not run Unit or announce a fabricated setup change", async () => {
    const f = fixture();
    await f.emit("session_start");
    expect(await f.emit("agent_before_settle", { entries: [], continue: false })).toBeUndefined();
    expect(f.units).toBe(0);
    expect(f.providerRequests).toBe(0);
  });

  test("instance epoch changes are observed even if identity and generation were reused", async () => {
    const f = fixture();
    await f.emit("session_start");
    const next = status(); next.instance.epoch = "epoch-2";
    f.set(next);
    f.unit(async () => {});
    await f.emit("tool_result", { toolName: "write" });
    expect(f.units).toBe(1);
  });

  test("coverage gaps are shown but never independently block finishing", async () => {
    const f = fixture(status("a", 0, { coverage_gaps: ["no tests installed for sample"] }));
    await f.emit("session_start");
    const result = await f.finish();
    expect(result.details.finish_ok).toBe(true);
    expect(result.details.summary).toContain("Coverage gaps: no tests installed for sample");
  });

  test("stale Performance is disclosed as NOT VERIFIED but never blocks finish", async () => {
    const f = fixture(status("a", 0, { performance: { recommended: false, why: "", state: "stale" } }));
    await f.emit("session_start");
    const result = await f.finish();
    expect(result.details.finish_ok).toBe(true);
    expect(result.details.summary).toContain("Performance STALE (NOT VERIFIED: result is from an older setup)");
    expect(result.details.summary).toContain("never blocking");
  });

  test("unknown setup identity cannot grant a clean finish", async () => {
    const f = fixture(status("a", 0, { setup_id: null }));
    await f.emit("session_start");
    await expect(f.finish()).rejects.toThrow("setup identity is unknown");
    expect(f.units).toBe(0);
  });

  test("unknown/custom tool changes are caught by final-boundary rescan", async () => {
    const f = fixture();
    await f.emit("session_start");
    f.set(status("c", 2));
    await f.emit("tool_result", { toolName: "custom_installer" });
    expect(f.units).toBe(0);
    await f.emit("agent_before_settle", { entries: [] });
    expect(f.units).toBe(1);
  });

  test("checker error remains NOT VERIFIED and finish is a real tool error", async () => {
    const f = fixture();
    f.read(async () => { throw new Error("endpoint unavailable"); });
    await f.emit("session_start");
    const final = await f.emit("agent_before_settle", { entries: [] });
    expect(final.entries.at(-1).content).toContain("NOT VERIFIED");
    expect(final.entries.at(-1).details.finish_ok).toBe(false);
    await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    expect(f.providerRequests).toBe(0);
  });

  test("Unit failure is not retried for the same identity and cannot finish cleanly", async () => {
    const failed = status("b", 1, { finish_ok: false, unit: { state: "fail", receipt: "/private/fail.json", verdict: "fail", exception: null } });
    const f = fixture();
    await f.emit("session_start");
    f.set(failed);
    f.unit(async () => {});
    await Promise.all([f.emit("tool_result", { toolName: "edit" }), f.emit("tool_result", { toolName: "bash" })]);
    const final = await f.emit("agent_before_settle", { entries: [] });
    expect(final.entries.at(-1).content).toContain("Unit FAIL");
    expect(f.units).toBe(1);
    await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    expect(f.units).toBe(1);
  });

  test("timed out Unit is unknown, not replayed, and a concurrent setup change is not blessed", async () => {
    const f = fixture();
    await f.emit("session_start");
    f.set(status("b", 1));
    f.unit(async () => { throw new Error("Unit timed out"); });
    await f.emit("tool_result", { toolName: "bash" });
    expect(f.statuses.at(-1)).toContain("NOT VERIFIED");
    // Authoritative backend must not contain a current pass for the timed-out run.
    f.set(status("b", 1, { finish_ok: false, unit: { state: "pending", receipt: null, verdict: null, exception: null } }));
    await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    expect(f.units).toBe(1);
    f.set(status("c", 2));
    f.unit(async () => { f.set(status("d", 3, { finish_ok: false })); });
    await f.emit("tool_result", { toolName: "write" });
    expect(f.statuses.at(-1)).toContain("setup changed during Unit");
    expect(f.statuses.at(-1)).toContain("NOT VERIFIED");
  });

  test("malformed/inconsistent status and stale exceptions never grant finish", async () => {
    for (const invalid of [{}, status("a", 0, { unit: { state: "missing", receipt: null, exception: null } }),
      status("a", 1, { unit: { state: "fail", receipt: "/fail", exception: { reason: "accepted", setup_id: "a".repeat(64), generation: 0, accepted_by: "owner" } } })]) {
      const f = fixture(invalid);
      await f.emit("session_start");
      await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    }
  });

  test("a stale Integration path is not mistaken for current evidence", async () => {
    const f = fixture(status("a", 0, { finish_ok: false, integration: { state: "stale", receipt: "/old.json", skip_reason: null } }));
    await f.emit("session_start");
    await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    expect(f.statuses.at(-1)).toContain("NOT VERIFIED: result is from an older setup");
    f.set(status("a", 0, { integration: { state: "stale", receipt: "/old.json", skip_reason: null } }));
    await expect(f.finish()).rejects.toThrow("inconsistent finish approval");
  });

  test("Integration absence refuses finish; explicit skip and current human exception are reported", async () => {
    const f = fixture(status("a", 0, { finish_ok: false, integration: { state: "missing", receipt: null, skip_reason: null } }));
    await f.emit("session_start");
    await expect(f.finish()).rejects.toThrow('"finish_ok":false');
    f.set(status("a", 0, { unit: { state: "fail", receipt: "/fail", verdict: "fail", exception: { reason: "human reviewed", setup_id: "a".repeat(64), generation: 0, accepted_by: "owner" } },
      integration: { state: "skipped", receipt: null, skip_reason: "theme-only change" } }));
    const result = await f.finish();
    expect(result.details.finish_ok).toBe(true);
    expect(result.details.summary).toContain("Unit FAIL");
    expect(result.details.summary).toContain("theme-only change");
  });

  test("requirement defaults off only by backend policy; OFF is not a fabricated pass", async () => {
    const f = fixture(status("a", 0, { require_unit: false, finish_ok: true,
      unit: { state: "missing", receipt: null, verdict: null, exception: null } }));
    f.ctx.hasUI = false;
    await f.emit("session_start");
    const result = await f.finish();
    expect(result.details.finish_ok).toBe(true);
    expect(result.details.summary).toContain("NOT VERIFIED");
    expect(result.details.summary).toContain("requirement OFF");
    expect(f.stderr.length).toBeGreaterThan(0);
    expect(f.units).toBe(0);
  });
});
