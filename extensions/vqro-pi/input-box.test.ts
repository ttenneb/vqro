// The neutral adapter's editor-fact reporter, against a mocked socket: what it
// sends, when it sends it, that it never sends editor content, and that it stops
// cleanly. Ported from QA's pinned reporter tests for the typed prompt guard.
import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import net from "node:net";
import { EventEmitter } from "node:events";

const savedEnv = { ...process.env }, savedConnect = net.createConnection;
let handlers: Map<string, any>, requests: any[], sockets: any[], text: string, hang: boolean, terminalInput: any;
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
const reports = () => requests.filter(r => r.method === "pane.report_metadata");
const alive = () => sockets.filter(s => !s.destroyed);

beforeEach(async () => {
  process.env.VQRO_ENV = "1"; process.env.VQRO_SOCKET_PATH = "/private-test/no-socket";
  process.env.VQRO_BIN_PATH = "/private-test/vqro"; process.env.VQRO_PANE_ID = "w1:p1";
  delete process.env.HERDR_ENV; delete process.env.HERDR_SOCKET_PATH; delete process.env.HERDR_CLIENT_SOCKET_PATH;
  handlers = new Map(); requests = []; sockets = []; text = ""; hang = false; terminalInput = undefined;
  jest.useFakeTimers();
  net.createConnection = (() => {
    const socket: any = new EventEmitter(); let timeout: any;
    socket.destroyed = false; socket.unref = () => {};
    socket.destroy = () => { socket.destroyed = true; if (timeout) clearTimeout(timeout); };
    socket.setTimeout = (ms: number, callback?: () => void) => { timeout = setTimeout(() => { socket.emit("timeout"); callback?.(); }, ms); return socket; };
    socket.write = (body: string) => { requests.push(JSON.parse(body)); if (!hang) queueMicrotask(() => socket.emit("data", Buffer.from("{}\n"))); };
    sockets.push(socket); queueMicrotask(() => socket.emit("connect")); return socket;
  }) as any;
  const { default: adapter } = await import("./index.ts?input-box=" + Math.random());
  adapter({
    events: { emit() {}, on() { return () => {}; } },
    on(name: string, handler: any) { handlers.set(name, handler); },
    exec: async () => ({ code: 0, stdout: JSON.stringify({ result: { agent: { execution: "fixture", pane_id: "w1:p1" } } }) }),
    sendMessage() {}, sendUserMessage() {},
  } as any);
});
afterEach(() => { jest.clearAllTimers(); jest.useRealTimers(); net.createConnection = savedConnect; process.env = { ...savedEnv }; });

async function start() {
  await handlers.get("session_start")({}, {
    isIdle: () => true,
    ui: { getEditorText: () => text, onTerminalInput: (handler: any) => { terminalInput = handler; return () => { terminalInput = undefined; }; } },
  });
  await flush();
}

test("reports the fact, never the content, and refreshes an unchanged fact slowly", async () => {
  await start();
  expect(reports()[0].params.tokens).toEqual({ input_box: "empty" });
  const before = reports().length;
  jest.advanceTimersByTime(2000); await flush();
  expect(reports()).toHaveLength(before);
  jest.advanceTimersByTime(28000); await flush();
  expect(reports().length).toBeGreaterThan(before);
  text = "private draft"; jest.advanceTimersByTime(1000); await flush();
  expect(reports().at(-1).params.tokens).toEqual({ input_box: "has_text" });
  expect(JSON.stringify(reports())).not.toContain("private draft");
});

test("a batch that returns the text to empty still refreshes the fact", async () => {
  await start();
  const before = reports().length;
  terminalInput("abc\x7f\x7f\x7f"); // the hook runs before the editor consumes the batch
  text = "abc"; text = "";          // the whole batch leaves it empty before the deferred re-read
  // The fact is re-read after the settle; sending is paced to one per second.
  jest.advanceTimersByTime(1400); await flush();
  expect(reports().length).toBeGreaterThan(before);
  expect(reports().at(-1).params.tokens).toEqual({ input_box: "empty" });
});

test("typing admits at most one report per second", async () => {
  await start();
  const before = reports().length;
  for (let i = 0; i < 10; i++) { jest.advanceTimersByTime(100); text += "x"; terminalInput("x"); jest.advanceTimersByTime(0); await flush(); }
  expect(reports().length - before).toBeLessThanOrEqual(2);
});

test("a change skipped while a request is in flight is reported once it settles", async () => {
  hang = true;
  await start();
  text = "draft"; jest.advanceTimersByTime(1000); await flush();
  expect(reports()).toHaveLength(1);
  sockets[0].emit("data", Buffer.from("{}\n")); hang = false;
  jest.advanceTimersByTime(1000); await flush();
  expect(reports().at(-1).params.tokens.input_box).toBe("has_text");
});

test("a drain after an in-flight request samples the newer draft, never a stale reading", async () => {
  hang = true;
  await start();
  terminalInput("q\x7f"); text = ""; jest.advanceTimersByTime(400); await flush();
  expect(reports()).toHaveLength(1);
  text = "new human draft"; terminalInput("new human draft");
  jest.advanceTimersByTime(100); // the new settle has not fired yet
  sockets[0].emit("data", Buffer.from("{}\n")); hang = false; await flush();
  // The drain re-reads the editor, and sending is paced, so the report that
  // carries the newer draft is the one admitted on the next tick.
  jest.advanceTimersByTime(1000); await flush();
  expect(reports().at(-1).params.tokens.input_box).toBe("has_text");
});

test("a stalled endpoint must not accumulate sockets on every tick", async () => {
  hang = true;
  await start();
  jest.advanceTimersByTime(60000); await flush();
  expect(alive().length).toBeLessThanOrEqual(2);
});

test("shutdown stops reporting and closes the outstanding report socket", async () => {
  hang = true;
  await start();
  expect(alive().length).toBe(1);
  await handlers.get("session_shutdown")();
  await flush();
  const sent = reports().length;
  text = "draft"; jest.advanceTimersByTime(2000); await flush();
  expect(reports()).toHaveLength(sent);
  expect(alive()).toHaveLength(0);
});
