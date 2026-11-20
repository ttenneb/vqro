/** Opt-in installed-Pi1 admission fixture. Public provider/stream APIs, files only, no fetch/model API. */
import { appendFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
const root = process.env.A1_REAL_ROOT!;
function trace(kind: string, value: any = {}) { appendFileSync(join(root, "native.jsonl"), JSON.stringify({ kind, ...value }) + "\n"); }
export default function (pi: any) {
  trace("loaded", { pid: process.pid });
  pi.registerProvider("a1-offline", {
    api: "a1-offline", baseUrl: "http://127.0.0.1:1/never-used", apiKey: "not-a-credential",
    models: [{ id: "barrier", name: "A1 offline barrier", reasoning: false, input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 128 }],
    streamSimple(model: any, context: any) {
      trace("model", { messages: context.messages });
      const stream = createAssistantMessageEventStream();
      const output: any = { role: "assistant", content: [{ type: "text", text: "offline fixture complete" }],
        api: model.api, provider: model.provider, model: model.id, timestamp: Date.now(), stopReason: "stop",
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
      queueMicrotask(() => { stream.push({ type: "start", partial: output });
        stream.push({ type: "text_start", contentIndex: 0, partial: output });
        stream.push({ type: "text_delta", contentIndex: 0, delta: output.content[0].text, partial: output });
        stream.push({ type: "text_end", contentIndex: 0, content: output.content[0].text, partial: output });
        stream.push({ type: "done", reason: "stop", message: output }); stream.end(); });
      return stream;
    },
  });
  // Observation/pass-through only: never transforms, handles or expands input.
  pi.on("input", async (event: any) => { trace("input", { text: event.text }); });
  pi.on("before_agent_start", async (event: any) => {
    trace("preflight", { prompt: event.prompt });
    while (!existsSync(join(root, "release"))) await new Promise(resolve => setTimeout(resolve, 5));
    trace("released");
  });
  for (const event of ["agent_start", "agent_settled"]) pi.on(event, async () => { trace(event); });
}
