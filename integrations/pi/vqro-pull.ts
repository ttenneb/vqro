// vqro-pull: the tier-1 Pi plugin (coordination-v0.md §7). Load with `pi -e <this file>`.
//
// A Pi running in a backend terminal pulls its vqro inbox at Pi's own safe points. vqro never types into the
// terminal, and this plugin never reads or writes the editor.
//
//   turn_end             tool results recorded, next model request not yet made   -> take {at: "turn"}
//   agent_before_settle  the run would otherwise end (continues once)             -> take {at: "settle"}
//   idle                 a counts-only wake while Pi is idle                      -> take {at: "idle"}, start a turn
//
// Insertion is a `custom_message` entry (user role in the next request), so the declaration is
// `pull {points: [turn, settle, idle], insert: user, evidence: entry}`. Stages are reported from evidence only:
//   inserted  the `context` hook shows the entry in the conversation Pi is about to send (durable session entry);
//   read      an assistant message for a request containing it finished normally (an abort right after a take is
//             inserted but unread: measured in P5, and neither `context` nor `message_start` counts).
// A new consumer (every session_start) reconciles open claims against the session's entries first.
//
// Environment: VQRO_SOCKET (core endpoint), VQRO_AGENT (this agent's rN, injected by its launcher). Without both,
// the plugin does nothing. Errors never break Pi: they are logged to VQRO_PULL_LOG when set.
// Tested with Pi 0.87.0 and 1.5.0 (tests/pi_pull_e2e.py).
import net from "node:net";
import fs from "node:fs";
import { randomBytes } from "node:crypto";

const SOCK = process.env.VQRO_SOCKET;
const AGENT = process.env.VQRO_AGENT;
const LOG = process.env.VQRO_PULL_LOG;
const FAULT = process.env.VQRO_PULL_FAULT ?? ""; // test-only fault injection: crash_after_take | drop_report | abort_after_take
const TYPE = "vqro-inbox";
const CONTENT = ["text", "image", "resource_link"]; // block types inserted without loss
const LIMIT = 16;

type Rec = { delivery: string; from: unknown; mode: string; content: any[]; claim: string };

export default function (pi: any) {
  if (!SOCK || !AGENT) return;
  let conn: net.Socket | undefined;
  let buf = "";
  let nextId = 1;
  const waiting = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  let consumer = "";
  let current: any;
  let pullingIdle = false;
  let woken = false;
  let pending = 1; // last pending count from the wake view; unknown at start, so one take
  const claims = new Map<string, string>();   // delivery -> claim, taken and not yet evidenced as inserted
  const unread = new Map<string, string>();   // delivery -> claim, inserted and not yet read
  let inContext: string[] = [];
  let opSeq = 0;

  const log = (event: string, detail: any = {}) => {
    if (LOG) fs.appendFileSync(LOG, JSON.stringify({ t: Date.now(), event, ...detail }) + "\n");
  };
  const op = (what: string) => `${consumer}:${what}:${++opSeq}`;

  function call(method: string, params: any): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!conn) return reject(new Error("core not connected"));
      const id = nextId++;
      waiting.set(id, { resolve, reject });
      conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }
  const inbox = (name: string, args: any) => call("command.invoke", { name, args, deadline_ms: 3000 });

  async function connect() {
    conn = net.createConnection(SOCK!);
    conn.on("data", (chunk) => {
      buf += chunk.toString();
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const row = JSON.parse(buf.slice(0, i));
        buf = buf.slice(i + 1);
        if (row.id !== undefined && waiting.has(row.id)) {
          const w = waiting.get(row.id)!;
          waiting.delete(row.id);
          row.error ? w.reject(new Error(`${row.error.data?.kind ?? "error"}: ${row.error.message}`)) : w.resolve(row.result);
        } else if (row.method === "event" && row.params?.type === "view.content") {
          const count = row.params?.data?.content?.pending; // counts only; never message content
          if (typeof count === "number") pending = count;
          woken = pending > 0;
          void pullIdle();
        }
      }
    });
    conn.on("error", (e) => log("core_error", { message: String(e) }));
    conn.on("close", () => {
      for (const w of waiting.values()) w.reject(new Error("core closed"));
      waiting.clear();
      conn = undefined;
    });
    await new Promise<void>((resolve, reject) => { conn!.once("connect", () => resolve()); conn!.once("error", reject); });
    await call("hello", { client: "vqro-pi-pull" });
  }

  function render(rec: Rec): any[] {
    const head = `vqro message ${rec.delivery} from ${typeof rec.from === "string" ? rec.from : JSON.stringify(rec.from)} (mode ${rec.mode}):`;
    const out: any[] = [{ type: "text", text: head }];
    for (const b of rec.content) {
      if (b.type === "text") out.push({ type: "text", text: b.text });
      else if (b.type === "image") out.push({ type: "image", data: b.data, mimeType: b.mime });
      else if (b.type === "resource_link") out.push({ type: "text", text: `[link: ${b.name ?? b.uri} <${b.uri}>]` });
    }
    return out;
  }

  function draft(rec: Rec) {
    claims.set(rec.delivery, rec.claim);
    return { type: "custom_message", customType: TYPE, content: render(rec), display: true,
             details: { vqro: { delivery: rec.delivery } } }; // details are not sent to the model
  }

  async function take(at: string): Promise<Rec[]> {
    if (!conn || !consumer || pending === 0) return []; // no inbox call at a safe point without mail
    try {
      const { records } = await inbox("inbox.take", { op: op("take"), agent: AGENT, consumer, at, limit: LIMIT, content: CONTENT });
      if (records.length) log("taken", { at, deliveries: records.map((r: Rec) => r.delivery) });
      if (records.length && FAULT === "crash_after_take") { log("fault", { FAULT }); process.exit(42); }
      return records;
    } catch (e) {
      log("take_failed", { at, error: String(e) });
      return [];
    }
  }

  async function report(delivery: string, claim: string, stage: string, evidence: any = {}) {
    if (FAULT === "drop_report") return log("fault", { FAULT, delivery, stage });
    try {
      await inbox("inbox.report", { op: `${delivery}:${claim}:${stage}`, delivery, claim, stage, evidence });
      log("reported", { delivery, stage });
    } catch (e) {
      log("report_failed", { delivery, stage, error: String(e) });
    }
  }

  async function status(state: string, source: string) {
    try { await inbox("agents.terminal_status", { agent: AGENT, state, app: "pi", source }); } catch { /* status is best effort */ }
  }

  async function pullIdle() {
    if (pullingIdle || !woken || !current?.isIdle?.()) return;
    pullingIdle = true;
    woken = false;
    try {
      const recs = await take("idle");
      if (!recs.length) pending = 0; // idle serves every pullable mode: nothing eligible until the next wake
      for (const rec of recs) {
        const d = draft(rec);
        // followUp, not steer: a run that started meanwhile is not steered by an idle take.
        pi.sendMessage({ customType: TYPE, content: d.content, display: true, details: d.details }, { triggerTurn: true, deliverAs: "followUp" });
      }
    } finally {
      pullingIdle = false;
    }
  }

  pi.on("session_start", async (_event: any, ctx: any) => {
    current = ctx;
    consumer = `pi:${ctx.sessionManager.getSessionId()}:${randomBytes(6).toString("hex")}`;
    try {
      if (!conn) await connect();
      const records = ctx.sessionManager.getEntries()
        .filter((e: any) => e.type === "custom_message" && e.customType === TYPE && e.details?.vqro?.delivery)
        .map((e: any) => ({ delivery: e.details.vqro.delivery, entry: e.id }));
      const result = await inbox("inbox.reconcile", { op: op("reconcile"), agent: AGENT, consumer, records });
      log("reconciled", { consumer, ...result });
      await call("events.subscribe", { types: ["view.content"], view_content: { names: [`inbox-wake-${AGENT}`] } });
      woken = true; // one take when armed, then only on wake events
      void status("idle", "session_start");
      void pullIdle();
    } catch (e) {
      log("start_failed", { error: String(e) });
    }
  });

  pi.on("agent_start", async () => void status("working", "agent_start"));
  pi.on("ui_prompt_start", async () => void status("blocked", "ui_prompt_start"));
  pi.on("ui_prompt_end", async () => void status("working", "ui_prompt_end"));

  pi.on("turn_end", async (event: any, ctx: any) => {
    current = ctx;
    if (event.outcome !== "completed") return; // error/aborted turns are hard exits: take nothing
    const recs = await take("turn");
    if (!recs.length) return;
    if (FAULT === "abort_after_take") { log("fault", { FAULT }); ctx.abort(); }
    return { entries: [...event.entries, ...recs.map(draft)], continue: true };
  });

  pi.on("agent_before_settle", async (event: any, ctx: any) => {
    current = ctx;
    const recs = await take("settle");
    if (!recs.length) return;
    return { entries: [...event.entries, ...recs.map(draft)], continue: true };
  });

  // inserted: the conversation Pi is about to send contains the entry (boundary drafts emit no message_end).
  pi.on("context", async (event: any, ctx: any) => {
    inContext = [];
    for (const msg of event.messages) {
      const delivery = msg?.role === "custom" && msg.customType === TYPE ? msg.details?.vqro?.delivery : undefined;
      if (!delivery) continue;
      if (unread.has(delivery)) inContext.push(delivery);
      const claim = claims.get(delivery);
      if (!claim) continue;
      claims.delete(delivery);
      unread.set(delivery, claim);
      inContext.push(delivery);
      const entry = ctx.sessionManager.getEntries().find((e: any) => e.type === "custom_message" && e.details?.vqro?.delivery === delivery);
      await report(delivery, claim, "inserted", { entry: entry?.id ?? null, session: ctx.sessionManager.getSessionId() });
    }
  });

  // read: an assistant answer to a request containing it finished normally.
  pi.on("message_end", async (event: any) => {
    const stop = event.message?.stopReason;
    if (event.message?.role !== "assistant" || !inContext.length || stop === "aborted" || stop === "error") return;
    for (const delivery of inContext.splice(0)) {
      const claim = unread.get(delivery);
      if (!claim) continue;
      unread.delete(delivery);
      await report(delivery, claim, "read");
    }
  });

  pi.on("agent_settled", async (_event: any, ctx: any) => {
    current = ctx;
    void status("idle", "agent_settled");
    woken = pending > 0; // something may have arrived while busy
    void pullIdle();
  });

  pi.on("session_shutdown", async () => { conn?.end(); conn = undefined; });
}
