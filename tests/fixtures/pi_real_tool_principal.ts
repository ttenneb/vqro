/** Opt-in G11 component fixture: public fake provider, actual installed bash tool, no network/model API. */
import { appendFileSync } from "node:fs";
import { join } from "node:path";
import { createAssistantMessageEventStream } from "@earendil-works/pi-ai";
const root = process.env.A1_REAL_ROOT!;
let calls = 0;
export default function (pi: any) {
  appendFileSync(join(root, "native.jsonl"), JSON.stringify({ kind: "loaded", pid: process.pid }) + "\n");
  pi.registerProvider("a1-tool-offline", {
    api: "a1-tool-offline", baseUrl: "http://127.0.0.1:1/never-used", apiKey: "not-a-credential",
    models: [{ id: "principal", name: "A1 offline tool principal", reasoning: false, input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 128 }],
    streamSimple(model: any) {
      calls++;
      appendFileSync(join(root, "native.jsonl"), JSON.stringify({ kind: "model", call: calls }) + "\n");
      if (calls > 2) throw new Error("unexpected third fake model call; no retry allowed");
      const tool = calls === 1;
      const output: any = { role: "assistant", content: tool
        ? [{ type: "toolCall", id: "a1-g11-tool", name: "bash", arguments: { command: process.env.A1_TOOL_COMMAND!, timeout: 10 } }]
        : [{ type: "text", text: "A1_TOOL_DONE" }],
        api: model.api, provider: model.provider, model: model.id, timestamp: Date.now(), stopReason: tool ? "toolUse" : "stop",
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
      const stream = createAssistantMessageEventStream();
      queueMicrotask(() => { stream.push({ type: "start", partial: output });
        stream.push({ type: "done", reason: output.stopReason, message: output }); stream.end(); });
      return stream;
    },
  });
}
