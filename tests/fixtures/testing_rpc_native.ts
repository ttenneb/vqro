// Deterministic native RPC double loads the real installed plugin and drives its actual hooks.
// No model/provider implementation exists in this program; any scheduling request is a failure.
import { createInterface } from "node:readline";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
if (process.argv.includes("--version")) { console.log("Pi model-free RPC fixture 1"); process.exit(0); }
const args = process.argv.slice(2);
const argument = (name: string) => args.includes(name) ? args[args.indexOf(name) + 1] : undefined;
const root = process.env.FIXTURE_RPC_ROOT!;
const label = process.env.FIXTURE_RPC_LABEL!;
const hooks = new Map<string, Function>(), tools = new Map<string, any>(), commands = new Map<string, any>();
const events: string[] = [];
let providerRequests = 0, finishRefused: boolean | undefined, settle: any;
const plugin = argument("-e"), skill = argument("--skill");
const context = { hasUI: false, ui: { setStatus() {} } };
const pi = {
  on: (name: string, fn: Function) => hooks.set(name, fn),
  registerTool: (tool: any) => tools.set(tool.name, tool),
  registerCommand: (name: string, command: any) => commands.set(name, command),
  sendMessage() { providerRequests++; throw new Error("fixture forbids provider work"); },
  sendUserMessage() { providerRequests++; throw new Error("fixture forbids provider work"); },
};
async function emit(name: string, data: any = {}) {
  events.push(name);
  return await hooks.get(name)?.(data, context);
}
function persist() {
  writeFileSync(join(root, label + "-native.json"), JSON.stringify({
    events, providerRequests, finishRefused, settle, commands: [...commands.keys()], tools: [...tools.keys()],
    protectedEnvironment: Object.keys(process.env).filter(name => /^(VQRO_|HERDR_)/.test(name)),
    selectors: { socket: process.env.VQRO_TESTING_SOCKET, state: process.env.VQRO_TESTING_STATE_DIR },
  }, null, 2));
}
if (plugin) {
  const loaded = await import(pathToFileURL(plugin).href);
  loaded.default(pi);
  await emit("session_start");
}
persist();
const state = { sessionId: "fixture-session", isStreaming: false, isCompacting: false,
  steeringMode: "all", followUpMode: "all", model: null, thinkingLevel: "off", autoCompactionEnabled: false };
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  let data: any = {};
  switch (request.type) {
    case "get_state": data = state; break;
    case "get_commands":
      data = { commands: [...commands].map(([name, value]) => ({ name, description: value.description,
        source: "extension", sourceInfo: { path: plugin } })) };
      if (skill && existsSync(skill)) {
        const name = readFileSync(skill, "utf8").match(/^name:\s*(.+)$/m)?.[1];
        data.commands.push({ name: "skill:" + name, source: "skill", sourceInfo: { path: skill } });
      }
      break;
    case "get_available_models": data = { models: [] }; break;
    case "get_available_thinking_levels": data = { levels: ["off"] }; break;
    case "get_session_stats": data = { sessionId: state.sessionId }; break;
    case "get_messages": data = { messages: [] }; break;
    case "prompt":
      if (!plugin) throw new Error("unhooked fixture must not receive input");
      await emit("before_agent_start", { systemPrompt: "model-free fixture" });
      writeFileSync(join(root, "ext/assets-rpc-change.txt"), "observed tool edit");
      await emit("tool_result", { toolName: "write" });
      try { await tools.get("vqro_setup_finish").execute("fixture", {}, undefined, undefined, context); finishRefused = false; }
      catch { finishRefused = true; }
      settle = await emit("agent_before_settle", { entries: [] });
      persist();
      break;
  }
  console.log(JSON.stringify({ type: "response", command: request.type, id: request.id, success: true, data }));
  if (request.type === "prompt") console.log(JSON.stringify({ type: "agent_end", messages: [] }));
}
