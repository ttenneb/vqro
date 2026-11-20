// vqro-pull: the tier-1 Pi plugin (coordination-v0.md §7; default coordination extension `coord`, K2).
// Load with `pi -e <this file>`.
//
// A Pi running in a backend terminal pulls its vqro inbox at Pi's own safe points. vqro never types into the
// terminal, and this plugin never reads or writes the editor.
//
//   turn_end             tool results recorded, next model request not yet made   -> take {at: "turn"}
//   agent_before_settle  the run would otherwise end (continues once)             -> take {at: "settle"}
//   idle                 a counts-only wake while Pi is idle                      -> take {at: "idle"}, start a turn
//
// Insertion is a `custom_message` entry (user role in the next request). The endpoint's declaration, written by its
// launcher in `doc/agents/terminals/<rN>`, is `PULL_DECLARATION` below (every point exact: idle is Pi's in-process
// isIdle, and an idle take is delivered as a followUp, never steering a run that started meanwhile), and the
// launcher grants the plugin's principal `receive` on that endpoint. Stages come from evidence only:
//   inserted  the `context` hook shows our entry (structured `details`, never sent to the model or forgeable by
//             content) in the conversation Pi is about to send, and that entry is in the durable session;
//   read      an assistant message for a request containing it finished normally (an abort right after a take is
//             inserted but unread; neither `context` nor `message_start` counts).
// A new consumer (every session_start) reconciles open claims against the session's entries before taking.
//
// Wire: `<prefix>inbox.window/reconcile/take/report` via command.invoke (prefix `coord.` or $VQRO_COORD_PREFIX);
// every mutation's op carries the window epoch. A take whose reply is lost is inspected by its op (inbox.op), which
// returns its claims (never content): they are released, and the op is kept until that succeeds. Reconciliation
// follows its pages until `reconciled`. Wake: the `coord-status` live view's
// `wake.pending[<rN>]` (never content). The connection is re-established with backoff after a core restart.
// Tier-1 status: `agents.status {terminal, app: "pi", state, seq}` (the endpoint is the terminal), best effort.
//
// Environment: VQRO_SOCKET (core endpoint), VQRO_AGENT (this agent's terminal rN) and VQRO_PRINCIPAL (the plugin's
// principal, bound at hello), injected by its launcher. Without the first
// both the plugin does nothing. Errors never break Pi: they are logged to VQRO_PULL_LOG when set.
// No fault injection ships here: tests wrap `createPlugin` with their own hooks (tests/fixtures/pi/vqro-pull-faults.ts).
import net from "node:net";
import fs from "node:fs";
import { randomBytes } from "node:crypto";

// The endpoint declaration for launchers (coordination-v0.md §7). One line of JSON: tests read it from this file.
export const PULL_DECLARATION = {"pull": {"points": {"turn": {"precision": "exact"}, "settle": {"precision": "exact"}, "idle": {"precision": "exact"}}, "insert": "user", "evidence": "entry", "framing": {"turn": {"role": "user", "frame": "sender_header"}, "settle": {"role": "user", "frame": "sender_header"}, "idle": {"role": "user", "frame": "sender_header"}}}};

const SOCK = process.env.VQRO_SOCKET;
const AGENT = process.env.VQRO_AGENT;
const PREFIX = process.env.VQRO_COORD_PREFIX ?? "coord.";
const LOG = process.env.VQRO_PULL_LOG;
const TYPE = "vqro-inbox";
const CONTENT = new Set(["text", "image"]); // block types inserted without loss (Pi has no link block)
const LIMIT = 16;
const CALL_MS = 5000;
const WAKE_VIEW = "coord-status";

type Rec = { delivery: string; from: any; origin?: any; mode: string; content: any[]; claim: string };

// Observation points for tests. The shipped default export passes none; nothing in the environment can set them.
export type TestHooks = {
  afterTake?: (count: number) => void; // records just claimed by a take
  skipReport?: (stage: string) => boolean; // true: do not send this report
  afterTurnTake?: (ctx: any) => void; // turn_end took records, before they are inserted
};

export default createPlugin();

export function createPlugin(hooks: TestHooks = {}) {
  return (pi: any) => plugin(pi, hooks);
}

function plugin(pi: any, hooks: TestHooks) {
  if (!SOCK || !AGENT) return;
  let conn: net.Socket | undefined;
  let connecting: Promise<void> | undefined;
  let buf = "";
  let nextId = 1;
  const waiting = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void; timer: NodeJS.Timeout }>();
  let consumer = "";
  let epoch = "";
  let current: any;
  let pullingIdle = false;
  let woken = false;
  let pending = 1; // last wake count for this agent; unknown at start, so one take
  let backoff = 500;
  let stopped = false;
  const claims = new Map<string, string>();   // delivery -> claim, taken, not yet evidenced inserted
  const unread = new Map<string, string>();   // delivery -> claim, inserted, not yet read
  const inserting = new Map<string, Promise<void>>(); // delivery -> its inserted report (read follows it)
  let inContext: string[] = [];

  const log = (event: string, detail: any = {}) => {
    if (LOG) fs.appendFileSync(LOG, JSON.stringify({ t: Date.now(), event, ...detail }) + "\n");
  };

  function call(method: string, params: any): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!conn) return reject(new Error("transport: core not connected"));
      const id = nextId++;
      const timer = setTimeout(() => { waiting.delete(id); idle(); reject(new Error("transport: timeout")); }, CALL_MS);
      waiting.set(id, { resolve, reject, timer });
      conn.ref(); // a call in flight keeps Pi alive (Pi awaits our handlers), at most CALL_MS
      conn.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async function command(name: string, args: any) {
    const reply = await call("command.invoke", { name: PREFIX + name, args, deadline_ms: 3000 });
    if (reply?.window?.epoch) epoch = reply.window.epoch;
    return reply;
  }

  // A mutation: op = `<window epoch>:<suffix>`. An expired_operation created nothing, so one retry under the new
  // epoch is safe; anything else surfaces, carrying the op it was sent with (`error.op`).
  async function mutate(name: string, args: any, suffix = `${consumer}:${randomBytes(6).toString("hex")}`) {
    for (let attempt = 0; ; attempt++) {
      if (!epoch) epoch = (await command("inbox.window", {})).epoch;
      const op = `${epoch}:${suffix}`;
      try {
        return await command(name, { ...args, op });
      } catch (e) {
        if (attempt === 0 && String(e).includes("expired_operation")) { epoch = ""; continue; }
        throw Object.assign(e instanceof Error ? e : new Error(String(e)), { op });
      }
    }
  }

  const uncertain = (e: unknown) => /transport:|timeout|unavailable|indeterminate/.test(String(e));

  // Nothing the plugin holds keeps Pi alive beyond a call in flight: the idle connection and the reconnect timer
  // are unref'd, and nothing reconnects after session_shutdown.
  function idle() {
    if (!waiting.size) conn?.unref();
  }
  let reconnectTimer: NodeJS.Timeout | undefined;
  let recoveryTimer: NodeJS.Timeout | undefined;
  const recovering: string[] = []; // ops of lost takes whose claims are not yet released
  function scheduleReconnect() {
    if (stopped || reconnectTimer) return;
    const delay = backoff;
    backoff = Math.min(backoff * 2, 30000);
    log("reconnect_scheduled", { delay });
    reconnectTimer = setTimeout(() => {
      reconnectTimer = undefined;
      if (!stopped) void ensureConnected().catch(() => scheduleReconnect());
    }, delay);
    reconnectTimer.unref();
  }

  async function ensureConnected() {
    if (stopped) throw new Error("transport: plugin stopped");
    if (conn) return;
    if (connecting) return connecting;
    connecting = (async () => {
      const socket = net.createConnection(SOCK!);
      socket.on("data", (chunk) => {
        buf += chunk.toString();
        let i;
        while ((i = buf.indexOf("\n")) >= 0) {
          const line = buf.slice(0, i);
          buf = buf.slice(i + 1);
          let row: any;
          try { row = JSON.parse(line); } catch { log("bad_line"); continue; }
          if (row.id !== undefined && waiting.has(row.id)) {
            const w = waiting.get(row.id)!;
            waiting.delete(row.id);
            clearTimeout(w.timer);
            idle();
            row.error ? w.reject(new Error(`${row.error.data?.kind ?? "error"}: ${row.error.message}`)) : w.resolve(row.result);
          } else if (row.method === "event" && row.params?.type === "view.content" && row.params?.data?.view === WAKE_VIEW) {
            const count = row.params?.data?.content?.wake?.pending?.[AGENT!]; // counts only; never message content
            if (typeof count === "number") pending = count;
            woken = pending > 0;
            void pullIdle();
          }
        }
      });
      socket.on("error", (e) => log("core_error", { message: String(e) }));
      socket.on("close", () => {
        for (const w of waiting.values()) { clearTimeout(w.timer); w.reject(new Error("transport: core closed")); }
        waiting.clear();
        buf = "";
        if (conn === socket) { conn = undefined; log("core_lost"); scheduleReconnect(); }
      });
      await new Promise<void>((resolve, reject) => { socket.once("connect", () => resolve()); socket.once("error", reject); });
      conn = socket;
      try {
        // The launcher injects the plugin's principal (C2: bound at hello, never per call).
        const principal = process.env.VQRO_PRINCIPAL;
        await call("hello", { client: "vqro-pi-pull", ...(principal ? { identity: { kind: "principal", principal } } : {}) });
        await call("events.subscribe", { types: ["view.content"], view_content: { names: [WAKE_VIEW] } });
      } catch (e) {
        // Never keep a connection without its hello and subscription: drop it, the caller schedules a reconnect.
        conn = undefined;
        socket.destroy();
        log("connect_failed", { error: String(e) });
        throw e;
      }
      idle();
      backoff = 500;
      epoch = "";
      pending = 1; // unknown after a gap: one take
      woken = true;
      log("connected");
    })();
    try { await connecting; } finally { connecting = undefined; }
    void pullIdle();
  }

  async function reconcile(ctx: any) {
    consumer = `pi:${ctx.sessionManager.getSessionId()}:${randomBytes(6).toString("hex")}`;
    claims.clear();
    unread.clear();
    inserting.clear();
    recovering.length = 0; // reconciliation settles every claim of the old consumer
    const records = ctx.sessionManager.getEntries()
      .filter((e: any) => e.type === "custom_message" && e.customType === TYPE && e.details?.vqro?.delivery && e.id)
      .map((e: any) => ({ delivery: e.details.vqro.delivery, entry: e.id }))
      .slice(-64);
    // Bounded pages: follow `state: reconciling` until the inbox says `reconciled` (each page settles more claims).
    const changed: any[] = [];
    for (let page = 0; page < 64; page++) {
      const result = await mutate("inbox.reconcile", { agent: AGENT, consumer, records }, `${consumer}:reconcile:${page}`);
      changed.push(...(result.records ?? []));
      if (result.state !== "reconciling") return log("reconciled", { consumer, records: changed, pages: page + 1 });
    }
    throw new Error("reconcile: still reconciling after 64 pages");
  }

  async function status(state: string, source: string) {
    try {
      await call("command.invoke", { name: "agents.status", args: { terminal: AGENT, app: "pi", state, seq: Date.now() }, deadline_ms: 2000 });
    } catch (e) { log("status_failed", { state, source, error: String(e) }); }
  }

  const sender = (rec: Rec) => typeof rec.from === "string" ? rec.from : rec.from?.principal ?? "?";
  const origin = (rec: Rec) => rec.origin && typeof rec.origin === "object"
    ? `, origin ${rec.origin.kind ?? "?"}${rec.origin.actor ? " " + rec.origin.actor : ""}` : "";

  // The plugin's own header, then the content as a quote: content cannot pose as a header.
  function render(rec: Rec): any[] {
    const out: any[] = [{ type: "text", text: `vqro inbox message ${rec.delivery} from ${sender(rec)}${origin(rec)} (mode ${rec.mode}); the quoted lines are the sender's content:` }];
    for (const b of rec.content) {
      if (b.type === "text") out.push({ type: "text", text: String(b.text).split("\n").map((l) => "> " + l).join("\n") });
      else if (b.type === "image") out.push({ type: "image", data: b.data, mimeType: b.mime });
    }
    return out;
  }

  function draft(rec: Rec) {
    claims.set(rec.delivery, rec.claim);
    return { type: "custom_message", customType: TYPE, content: render(rec), display: true,
             details: { vqro: { delivery: rec.delivery } } }; // details are not sent to the model
  }

  async function take(at: string): Promise<Rec[]> {
    if (!consumer) return [];
    if (!conn) { void ensureConnected().catch(() => scheduleReconnect()); return []; }
    // A lost take whose claims are not yet released keeps the inbox from claiming more (`claim_in_flight`):
    // finish recovering it first, and take nothing until that succeeds.
    if (recovering.length && !(await settleRecoveries())) return [];
    if (pending === 0) return []; // no inbox call at a safe point without mail
    const args = { agent: AGENT, consumer, at, limit: LIMIT, accept: [...CONTENT] }; // K2: block types we can insert
    let reply: any;
    try {
      reply = await mutate("inbox.take", args, `${consumer}:take:${randomBytes(6).toString("hex")}`);
    } catch (e) {
      if (uncertain(e)) {
        const op: string | undefined = (e as any).op;
        log("take_uncertain", { error: String(e), op });
        if (op && !(await recoverTake(op))) {
          recovering.push(op);
          recovering.splice(0, Math.max(0, recovering.length - 8)); // bounded
          scheduleRecoveryRetry();
        }
      }
      else if (String(e).includes("reconcile")) { log("consumer_fenced"); if (current) await reconcile(current).catch(() => {}); }
      else log("take_failed", { at, error: String(e) });
      return [];
    }
    const records: Rec[] = reply.records ?? [];
    if (records.length) log("taken", { at, deliveries: records.map((r) => r.delivery) });
    else if (reply.held) log("held", { at, held: reply.held });
    if (records.length) hooks.afterTake?.(records.length);
    const usable: Rec[] = [];
    for (const rec of records) {
      // Never flatten or drop: a block this plugin cannot insert goes back, uninserted.
      if (rec.content.every((b) => CONTENT.has(b.type))) usable.push(rec);
      else await report(rec.delivery, rec.claim, "released");
    }
    return usable;
  }

  // A lost take reply: inspect that exact op (`inbox.op`, also after the window epoch moved on). It returns the claims
  // the take made, never content; release them so the mail stays deliverable instead of claimed until a restart.
  // true once settled: the claims are released (or already settled), or the op made none; false to retry later
  // (the core or inbox did not answer). The op is kept until then, so the mail is never stuck claimed.
  async function recoverTake(op: string): Promise<boolean> {
    let reply: any;
    try {
      await ensureConnected();
      reply = await command("inbox.op", { op }); // inspect that exact op; never a new take
    } catch (e) {
      log("take_recovery_failed", { op, error: String(e) });
      return !uncertain(e); // a definite refusal will not change by retrying
    }
    let settled = true;
    const claimed = [...(reply.claims ?? []), ...(reply.records ?? [])];
    for (const c of claimed) {
      try {
        await mutate("inbox.report", { delivery: c.delivery, claim: c.claim, stage: "released" }, `report:${c.delivery}:${c.claim}:released`);
      } catch (e) {
        if (uncertain(e)) settled = false;
        else log("release_refused", { delivery: c.delivery, error: String(e) }); // already settled
      }
    }
    log(settled ? "take_recovered" : "take_recovery_incomplete", { op, released: claimed.length });
    return settled;
  }

  async function settleRecoveries(): Promise<boolean> {
    for (const op of [...recovering]) {
      if (await recoverTake(op)) recovering.splice(recovering.indexOf(op), 1);
    }
    if (!recovering.length) return true;
    log("take_deferred", { recovering: recovering.length });
    scheduleRecoveryRetry();
    return false;
  }

  // Retry a pending recovery even if no turn or wake comes (an idle session).
  function scheduleRecoveryRetry() {
    if (stopped || recoveryTimer) return;
    recoveryTimer = setTimeout(() => { recoveryTimer = undefined; woken = true; void pullIdle(); }, 3000);
    recoveryTimer.unref();
  }

  async function report(delivery: string, claim: string, stage: string) {
    if (hooks.skipReport?.(stage)) return log("report_skipped", { delivery, stage });
    try {
      await mutate("inbox.report", { delivery, claim, stage }, `report:${delivery}:${claim}:${stage}`);
      log("reported", { delivery, stage });
    } catch (e) {
      log("report_failed", { delivery, stage, error: String(e) });
    }
  }

  async function pullIdle() {
    if (pullingIdle || !woken || !current?.isIdle?.()) return;
    pullingIdle = true;
    woken = false;
    try {
      for (const rec of await take("idle")) {
        const d = draft(rec);
        // followUp, not steer: a run that started meanwhile is not steered by an idle take.
        pi.sendMessage({ customType: TYPE, content: d.content, display: true, details: d.details }, { triggerTurn: true, deliverAs: "followUp" });
      }
    } finally {
      pullingIdle = false;
      if (woken) setImmediate(() => void pullIdle()); // a wake arrived while this pull ran
    }
  }

  pi.on("session_start", async (_event: any, ctx: any) => {
    current = ctx;
    stopped = false;
    try {
      await ensureConnected();
      await reconcile(ctx);
      woken = true; // one take when armed, then only on wake events
      void status("idle", "session_start");
      void pullIdle();
    } catch (e) {
      log("start_failed", { error: String(e) });
      scheduleReconnect();
    }
  });

  pi.on("agent_start", async () => void status("running", "agent_start"));
  pi.on("ui_prompt_start", async () => void status("requires_action", "ui_prompt_start"));
  pi.on("ui_prompt_end", async () => void status("running", "ui_prompt_end"));

  pi.on("turn_end", async (event: any, ctx: any) => {
    current = ctx;
    if (event.outcome !== "completed") return; // error/aborted turns are hard exits: take nothing
    const recs = await take("turn");
    if (!recs.length) return;
    hooks.afterTurnTake?.(ctx);
    return { entries: [...event.entries, ...recs.map(draft)], continue: true };
  });

  pi.on("agent_before_settle", async (event: any, ctx: any) => {
    current = ctx;
    const recs = await take("settle");
    if (!recs.length) return;
    return { entries: [...event.entries, ...recs.map(draft)], continue: true };
  });

  // inserted: the conversation Pi is about to send contains our entry and the session holds it durably (boundary
  // drafts emit no message_end). Reports go out concurrently; the model request never waits on the inbox.
  pi.on("context", async (event: any, ctx: any) => {
    inContext = [];
    const reports: Promise<void>[] = [];
    for (const msg of event.messages) {
      const delivery = msg?.role === "custom" && msg.customType === TYPE ? msg.details?.vqro?.delivery : undefined;
      if (!delivery) continue;
      if (unread.has(delivery)) inContext.push(delivery);
      const claim = claims.get(delivery);
      if (!claim) continue;
      const entry = ctx.sessionManager.getEntries().find((e: any) => e.type === "custom_message" && e.details?.vqro?.delivery === delivery);
      if (!entry?.id) continue; // not durable yet: evidence: entry needs the session entry; try at the next request
      claims.delete(delivery);
      unread.set(delivery, claim);
      inContext.push(delivery);
      const sent = report(delivery, claim, "inserted");
      inserting.set(delivery, sent);
      reports.push(sent);
    }
    void Promise.all(reports);
  });

  // read: an assistant answer to a request containing it finished normally.
  pi.on("message_end", async (event: any) => {
    const stop = event.message?.stopReason;
    if (event.message?.role !== "assistant" || !inContext.length || stop === "aborted" || stop === "error") return;
    for (const delivery of inContext.splice(0)) {
      const claim = unread.get(delivery);
      if (!claim) continue;
      unread.delete(delivery);
      // In order: read only after its inserted report (the inbox refuses a report that does not advance).
      const before = inserting.get(delivery) ?? Promise.resolve();
      inserting.delete(delivery);
      void before.then(() => report(delivery, claim, "read"));
    }
  });

  pi.on("agent_settled", async (_event: any, ctx: any) => {
    current = ctx;
    void status("idle", "agent_settled");
    woken = pending > 0; // something may have arrived while busy
    void pullIdle();
  });

  pi.on("session_shutdown", async () => {
    stopped = true;
    if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = undefined; }
    if (recoveryTimer) { clearTimeout(recoveryTimer); recoveryTimer = undefined; }
    const c = conn;
    conn = undefined;
    c?.destroy();
  });
}
