// No provider/runtime is started: drive the real plugin against the private Python checker CLI.
import plugin from "../../integrations/pi/vqro-testing";
import { appendFileSync } from "node:fs";
import { join } from "node:path";

const handlers = new Map<string, Function>(), tools = new Map<string, any>(), notices: string[] = [];
plugin({
  on: (name: string, fn: Function) => handlers.set(name, fn),
  registerTool: (tool: any) => tools.set(tool.name, tool),
  sendMessage: () => { throw new Error("unexpected provider-work injection"); },
  sendUserMessage: () => { throw new Error("unexpected provider-work injection"); },
});
const ctx = { hasUI: true, ui: { setStatus(_key: string, message: string) { notices.push(message); } } };
await handlers.get("session_start")!({}, ctx);
if (process.argv[2] === "change") {
  appendFileSync(join(process.env.VQRO_TESTING_EXTENSIONS_DIR!, "sample.py"), "\n# changed by private fixture\n");
  await handlers.get("tool_result")!({ toolName: "write" }, ctx);
}
let result: any;
try {
  result = (await tools.get("vqro_setup_finish").execute("private-call", {}, undefined, undefined, ctx)).details;
} catch (error) {
  try { result = JSON.parse((error as Error).message); }
  catch { throw error; }
}
const final = await handlers.get("agent_before_settle")!({ entries: [] }, ctx);
console.log(JSON.stringify({ ...result, notices, entries: final?.entries || [] }));
