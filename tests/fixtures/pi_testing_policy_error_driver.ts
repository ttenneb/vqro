// Drive the default adapter and public hooks against the real private checker CLI.
import plugin from "../../integrations/pi/vqro-testing";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

const handlers = new Map<string, Function>(), tools = new Map<string, any>();
let providerRequests = 0;
plugin({
  on: (name: string, fn: Function) => handlers.set(name, fn),
  registerTool: (tool: any) => tools.set(tool.name, tool),
  sendMessage: () => { providerRequests++; throw new Error("unexpected inference"); },
  sendUserMessage: () => { providerRequests++; throw new Error("unexpected inference"); },
});
const ctx = { hasUI: true, ui: { setStatus() {} } };
const configure = (value: any) => writeFileSync(join(process.env.VQRO_TESTING_STATE_DIR!, "fixture-mode.json"), JSON.stringify(value));
configure({ policy: false });
await handlers.get("session_start")!({}, ctx);
await handlers.get("before_agent_start")!({ systemPrompt: "base" }, ctx);
async function sample(value: any) {
  configure(value);
  await handlers.get("tool_result")!({ toolName: "write" }, ctx);
  const context = await handlers.get("context")!({ messages: [] }, ctx);
  const prompt = await handlers.get("before_agent_start")!({ systemPrompt: "base" }, ctx);
  let finish: any;
  try { finish = (await tools.get("vqro_setup_finish").execute("private", {}, null, null, ctx)).details; }
  catch (error) { finish = JSON.parse((error as Error).message); }
  return { prompt: prompt.systemPrompt, context: context?.messages.at(-1)?.content, finish };
}
const on = await sample({ policy: true });
const off = await sample({ policy: false });
const invalidPolicy = await sample({ policy: "not boolean" });
await sample({ policy: true });
const unavailable = await sample({ policy: true, unavailable: true });
const malformed: any[] = [];
for (const mode of ["text", "partial", "finish", "extra", "version"]) {
  await sample({ policy: true });
  malformed.push(await sample({ policy: true, malformed: mode }));
}
console.log(JSON.stringify({ on, off, invalidPolicy, unavailable, malformed, providerRequests }));
