// Terminal views (profile `terminal`) through OpenTUI's EmbeddedTerminalRenderable (Ghostty VT): attach with
// backend history, feed `stream.data`, send typed bytes as ordered `stream.input`, follow pane size with
// `stream.resize`, and on a gap or close discard the emulator and reattach for a fresh repaint (core-protocol.md,
// Resources). A viewer never answers terminal queries: the emulator's `response` bytes are dropped.
import { EmbeddedTerminalRenderable, type KeyEvent, type MouseEvent } from "@opentui/core";
import type { App, Rect } from "./app";

const CHUNK = 64 * 1024;
const MAX_PASTE = 4 * 1024 * 1024;
const b64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");

interface Term {
  view: string;
  resource: string;
  r: EmbeddedTerminalRenderable;
  stream: number | null;
  attaching: boolean;
  closed: string | null;
  retryAt: number;
  size: [number, number];
  /** Ordered input: each `stream.input` is answered before the next is sent. */
  queue: Promise<void>;
}

export class Terminals {
  private terms = new Map<string, Term>();
  private byStream = new Map<number, Term>();
  private early = new Map<number, Uint8Array[]>();

  constructor(private app: App) {}

  /** Place one emulator per visible terminal pane; remove the rest. Called on every frame. */
  layout(leaves: { view: string; inner: Rect }[], _overlay: boolean) {
    const a = this.app;
    const seen = new Set<string>();
    for (const leaf of leaves) {
      const shown = a.shownView(leaf.view) ?? leaf.view;
      const v = a.ws.entries.get(`view/${shown}`) ?? {};
      const rid = typeof v.resource === "string" ? v.resource : "";
      seen.add(leaf.view);
      let t = this.terms.get(leaf.view);
      if (t && t.resource !== rid) { this.drop(t); t = undefined; }
      const cols = Math.max(1, leaf.inner.w), rows = Math.max(1, leaf.inner.h);
      if (!t) t = this.create(leaf.view, rid, cols, rows);
      t.r.position = "absolute";
      t.r.left = leaf.inner.x; t.r.top = leaf.inner.y; t.r.width = cols; t.r.height = rows;
      t.r.visible = true;
      if (t.size[0] !== cols || t.size[1] !== rows) {
        t.size = [cols, rows];
        if (t.stream !== null) a.peer.tell("stream.resize", { stream: t.stream, cols, rows });
      }
      const res = a.ws.resources.get(rid);
      const live = res?.state === "live" && res?.available !== false;
      if (live && t.stream === null && !t.attaching && Date.now() >= t.retryAt) void this.attach(t);
    }
    for (const [view, t] of this.terms) if (!seen.has(view)) this.drop(t);
  }

  private create(view: string, resource: string, cols: number, rows: number): Term {
    const t = { view, resource, stream: null, attaching: false, closed: null, retryAt: 0, size: [cols, rows], queue: Promise.resolve() } as unknown as Term;
    this.emulator(t);
    this.terms.set(view, t);
    return t;
  }

  /** A new emulator for this view: on creation and before every attach (a fresh repaint never continues an old
   *  parser state across streams). */
  private emulator(t: Term) {
    const a = this.app;
    if (t.r) { a.renderer.root.remove(t.r); t.r.destroyRecursively?.(); }
    const [cols, rows] = t.size;
    t.r = new EmbeddedTerminalRenderable(a.renderer, { cols, rows, width: cols, height: rows, position: "absolute", maxScrollback: 5000, zIndex: 1 });
    t.r.onData = (data, source) => { if (source === "input") this.send(t, data); };  // never answer queries as a viewer
    t.r.onScreenChange = () => a.redraw();
    a.renderer.root.add(t.r);
  }

  private drop(t: Term) {
    if (t.stream !== null) { this.app.peer.tell("stream.close", { stream: t.stream }); this.byStream.delete(t.stream); }
    this.app.renderer.root.remove(t.r);
    t.r.destroyRecursively?.();
    this.terms.delete(t.view);
  }

  private async attach(t: Term) {
    const a = this.app;
    t.attaching = true;
    this.emulator(t);
    try {
      const reply = await a.peer.call("resource.attach", { resource: t.resource, cols: t.size[0], rows: t.size[1], history: 2000 });
      if (!this.terms.has(t.view) || this.terms.get(t.view) !== t) { a.peer.tell("stream.close", { stream: reply.stream }); return; }
      t.stream = reply.stream;
      t.closed = null;
      this.byStream.set(reply.stream, t);
      for (const chunk of this.early.get(reply.stream) ?? []) t.r.write(chunk);
      this.early.delete(reply.stream);
    } catch (e) {
      t.closed = (e as Error).message;
      t.retryAt = Date.now() + 1000;
      setTimeout(() => a.redraw(), 1050);
    } finally {
      t.attaching = false;
      a.redraw();
    }
  }

  /** The connection ended: its streams are gone; emulators reattach (fresh repaint) once the core is back. */
  reset() {
    this.byStream.clear();
    this.early.clear();
    for (const t of this.terms.values()) { t.stream = null; t.attaching = false; t.closed = "core disconnected"; t.retryAt = Date.now() + 1000; }
  }

  onStream(method: string, params: any) {
    const sid = params.stream;
    const t = this.byStream.get(sid);
    if (method === "stream.data") {
      const bytes = new Uint8Array(Buffer.from(String(params.data ?? ""), "base64"));
      if (t) t.r.write(bytes);
      else if ([...this.terms.values()].some((x) => x.attaching)) {  // output can race the attach reply
        const list = this.early.get(sid) ?? [];
        if (list.reduce((n, c) => n + c.length, 0) + bytes.length <= 256 * 1024) { list.push(bytes); this.early.set(sid, list); }
        else { this.early.delete(sid); this.app.peer.tell("stream.close", { stream: sid }); }
      } else this.app.peer.tell("stream.close", { stream: sid });
    } else if (method === "stream.closed" || method === "stream.gap") {
      this.early.delete(sid);
      if (method === "stream.gap") this.app.peer.tell("stream.close", { stream: sid });
      if (t) {
        this.byStream.delete(sid);
        t.stream = null;
        t.closed = params.reason?.reason ?? params.reason ?? "closed";
        t.retryAt = Date.now() + 1000;
        setTimeout(() => this.app.redraw(), 1050);
      }
    } else if (method === "stream.error") {
      this.app.message = `stream input rejected (not replayed): ${params.error?.message ?? "error"}`;
    }
    this.app.redraw();
  }

  onResource(id: string, available: boolean, live: boolean, restored: boolean) {
    for (const t of this.terms.values()) {
      if (t.resource !== id) continue;
      if (!available && t.stream !== null) {
        this.app.peer.tell("stream.close", { stream: t.stream });
        this.byStream.delete(t.stream);
        t.stream = null; t.closed = "resource unavailable"; t.retryAt = Date.now() + 1000;
      } else if (available && live && restored) t.retryAt = Date.now();
    }
  }

  private send(t: Term, data: Uint8Array) {
    if (t.stream === null || !data.length) return;
    const stream = t.stream;
    t.queue = t.queue.then(async () => {
      for (let at = 0; at < data.length; at += CHUNK) {
        const chunk = data.subarray(at, at + CHUNK);
        for (let tries = 0; ; tries++) {
          try { await this.app.peer.call("stream.input", { stream, data: b64(chunk) }, 30_000); break; }
          catch (e: any) {
            if (e?.kind === "overloaded" && tries < 300) { await Bun.sleep(100); continue; }  // refused, no effect: offer again
            this.app.message = `input stopped after ${at} of ${data.length} bytes: ${e?.message ?? e}; nothing resent`;
            this.app.redraw();
            return;
          }
        }
      }
    });
  }

  key(view: string, k: KeyEvent) {
    const t = this.terms.get(view);
    if (t) this.send(t, t.r.encodeKey(k));  // the child's current keyboard mode (kitty, modifyOtherKeys)
  }

  paste(view: string, bytes: Uint8Array) {
    const t = this.terms.get(view);
    if (!t) return;
    const wrapped = t.r.encodePaste(bytes);
    if (wrapped.length > MAX_PASTE) { this.app.message = `paste of ${wrapped.length} bytes refused (limit 4 MiB); nothing sent`; this.app.redraw(); return; }
    this.send(t, wrapped);
  }

  scroll(view: string, e: MouseEvent) {
    const t = this.terms.get(view);
    if (t) t.r.processMouseEvent(e);
  }

  title(view: string, fallback: string): string {
    const a = this.app;
    const t = this.terms.get(view);
    const v = a.ws.entries.get(`view/${a.shownView(view) ?? view}`) ?? {};
    const r = a.ws.resources.get(v.resource);
    const name = v.title ?? r?.name ?? r?.argv?.[0] ?? fallback;
    const state = !r ? " (gone)" : r.available === false ? " (unavailable)" : r.state === "finished" ? " (finished)" : t?.closed ? ` (${t.closed}; reattaching)` : "";
    return `${name}${state}`;
  }
}
