// The OpenTUI trial client: renders the workspace through the shared client model (model.ts). Base profile
// (layout, panels, menus, prompts, recovery, safe-mode chooser) and `keys`; terminal views come from terminal.ts.
import { Renderable, RGBA, TextAttributes, type CliRenderer, type KeyEvent, type MouseEvent, type OptimizedBuffer, type PasteEvent } from "@opentui/core";
import * as m from "./model";
import { Peer, RpcError, type Json } from "./rpc";
import { Palette } from "./theme";
import { Terminals } from "./terminal";

export const FEATURES = [...m.BASE_FEATURES, "icon_names", "current_when", "follow", "list_detail", "embedded_views", "markdown", "pin", "composer", "multiline", "search", "facets", "sort", "columns", "message", "card"];
export const PROFILES = ["base", "keys", "terminal"];
const KINDS = ["panel", "terminal"];

export interface Rect { x: number; y: number; w: number; h: number }
interface Leaf { view: string; rect: Rect; inner: Rect; path: number[] }
interface PState { sel: number; top: number; page: number; filter: string; open: Record<string, boolean>; chip: number; followEnd: number | null; manual: boolean; detailFocus: boolean; detailFor: string; composers: Map<string, m.Composer>; editing: string | null; search: string; searchHit: number; seenPages: string[]; facets: Record<string, string>; sort: number | null }
type Overlay =
  | { kind: "prefix"; sel: number }
  | { kind: "palette"; query: string; sel: number }
  | { kind: "menu"; title: string; items: Json[]; ctx: Json; sel: number }
  | { kind: "input"; prompt: string; text: string; item: Json; ctx: Json; multi?: m.Editor }
  | { kind: "pick"; item: Json; ctx: Json; sel: number }
  | { kind: "popup"; view: string; width: number; ctx: Json }
  | { kind: "filter"; key: string; search?: string };  // search: typing a search in that view (feature search)
type Hit = { rect: Rect; act: (e: MouseEvent) => void };

/** How long a popup waits for its view's entries event (see App.openPopup). */
const POPUP_WAIT_MS = 2000;
const width = (s: string) => Bun.stringWidth(s);
/** Cut a string to at most `w` cells. */
export function fit(s: string, w: number): string {
  if (w <= 0) return "";
  if (width(s) <= w) return s;
  let out = "", used = 0;
  for (const ch of s) { const cw = width(ch); if (used + cw > w - 1) break; out += ch; used += cw; }
  return out + "…";
}
const clean = (s: any) => String(s ?? "").replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "").replace(/\n/g, " ");
const isObj = (v: Json) => v !== null && typeof v === "object" && !Array.isArray(v);

/** An OpenTUI key event as a vqro key name (core-protocol.md, Keys), or null. */
export function keyName(k: KeyEvent): string | null {
  const named: Record<string, string> = {
    up: "Up", down: "Down", left: "Left", right: "Right", home: "Home", end: "End", pageup: "PageUp", pagedown: "PageDown",
    insert: "Insert", delete: "Delete", return: "Enter", enter: "Enter", kpenter: "Enter", escape: "Esc",
    backspace: "Backspace", space: "Space",
  };
  let base: string;
  if (k.name === "linefeed") return "C-j";  // Ctrl-J: a new line in composers and multi-line prompts
  if (k.name === "tab") base = k.shift ? "BackTab" : "Tab";
  else if (named[k.name]) base = named[k.name];
  else if (/^f([1-9]|1[0-2])$/.test(k.name)) base = k.name.toUpperCase();
  else if (!k.ctrl && !k.meta && [...k.sequence].length === 1 && k.sequence >= " ") base = k.sequence;
  else if ([...k.name].length === 1) base = k.shift ? k.name.toUpperCase() : k.name;
  else return null;
  const shiftMod = k.shift && base.length > 1 && base !== "BackTab";
  return m.normalize((k.ctrl ? "C-" : "") + (k.meta || k.option ? "M-" : "") + (shiftMod ? "S-" : "") + base);
}

export class App {
  ws = new m.Workspace();
  win = new m.Window();
  peer!: Peer;
  pal = new Palette(null, false);
  focus = 0;
  zoomed: string | null = null;
  overlay: Overlay | null = null;
  under: Overlay | null = null;      // a popup under a menu or prompt opened from it
  pstate = new Map<string, PState>();
  safeSelected = new Set<string>();
  message = "";
  toast: { text: string; style: string | null; until: number } | null = null;
  leaves: Leaf[] = [];
  /** The focused view at the last draw and the one before it (`client.focus {dir: "last"}`). */
  focusNow: string | null = null;
  /** Until this time (ms), keys with a `repeat` binding run without the prefix (binding `repeat`). */
  repeatUntil = 0;
  armRepeat(b: Json) { if (b?.repeat === true && !this.overlay) this.repeatUntil = Date.now() + 600; }
  /** What each resource reported when its tab was last shown (m.seenOf), for tab marks. */
  seen = new Map<string, Json>();
  /** A tab's mark (m.tabMark); showing a tab remembers what its resources report now. */
  tabMark(tab: Json, shown: boolean): string | null {
    const views: string[] = [];
    const walk = (n: Json) => { if (!isObj(n)) return; if (typeof n.view === "string") views.push(n.view); (n.children ?? []).forEach(walk); (n.tabs ?? []).forEach((t: Json) => walk(t?.node)); };
    walk(tab?.node);
    const rids = views.map((v) => this.ws.entries.get(`view/${v}`)?.resource).filter((r): r is string => typeof r === "string" && this.ws.resources.has(r));
    for (const r of rids) {
      if (shown || !this.seen.has(r)) this.seen.set(r, m.seenOf(this.ws.resources.get(r)));
      if (shown) this.ws.reached.delete(r);
    }
    return shown ? null : m.tabMark(this.ws.entries.get(m.TABS_ENTRY), rids.map((r) => this.ws.resources.get(r)), rids.map((r) => this.seen.get(r)),
      rids.map((r) => this.ws.reached.get(r) ?? null));
  }
  focusLast: string | null = null;
  hits: Hit[] = [];
  listed = new Map<string, { rows: m.Row[]; hits: { y: number; x: number; w: number; i: number; chip?: number }[]; area?: Rect }>();
  terminals: Terminals;
  quit = false;
  private resyncing = false;

  constructor(public renderer: CliRenderer, public screen: Screen, private socketPath: string) {
    this.terminals = new Terminals(this);
  }

  // ---- protocol ---------------------------------------------------------------------------------------------------

  async start() {
    this.peer = await Peer.connect(this.socketPath, (method, params) => this.onNotify(method, params));
    this.peer.onClose = () => { this.message = "core disconnected, reconnecting…"; this.terminals.reset(); this.redraw(); void this.reconnect(); };
    await this.peer.call("hello", { client: "opentui", version: "0.1.0", profiles: PROFILES, features: FEATURES });
    await this.resync();
  }

  /** Reconnect after the core restarts: a fresh snapshot, never replayed input (terminals reattach). */
  private async reconnect() {
    for (;;) {
      await Bun.sleep(1000);
      if (this.quit) return;
      try { await this.start(); this.message = "reconnected"; this.redraw(); return; } catch { /* the core is not back yet */ }
    }
  }

  async resync() {
    if (this.resyncing) return;
    this.resyncing = true;
    try {
      const snap = await this.peer.call("workspace.snapshot", {}, 10_000);
      const was = this.ws.load(snap);
      this.settlePendingPopup();
      this.win.settle(this.ws);
      this.safeModeChanged(this.ws.safeMode, was);
      try { await this.peer.call("events.subscribe", { from: snap.seq, epoch: snap.epoch, partial_resources: true }); }
      catch (e) { if (!(e instanceof RpcError && e.kind === "conflict")) throw e; }
    } catch (e) { this.message = `snapshot failed: ${(e as Error).message}`; }
    finally { this.resyncing = false; this.redraw(); }
  }

  /** A popup a command reply asked for. The reply and the view's entries event travel separately, so an unknown view
   *  waits for the next events (up to POPUP_WAIT_MS, or until the user does something) instead of being refused. */
  pendingPopup: { view: string; width: number; ctx: Json; until: number } | null = null;
  openPopup(view: string, width: number, ctx: Json) {
    const kind = this.viewKind(view);
    if (kind === "terminal") this.message = "popup refused: terminals never pop up";
    else if (!kind) { this.message = `opening ${view}…`; this.pendingPopup = { view, width, ctx, until: Date.now() + POPUP_WAIT_MS }; }
    else {
      this.pendingPopup = null;
      // Asked again for the popup already shown (a late reply): keep the user's filter and place in it.
      if (!(this.overlay?.kind === "popup" && this.overlay.view === view)) { this.pstate.delete("@popup"); this.under = null; }
      this.overlay = { kind: "popup", view, width, ctx };
      if (this.message.startsWith("opening ")) this.message = "";
    }
  }
  settlePendingPopup() {
    const p = this.pendingPopup;
    if (!p) return;
    this.pendingPopup = null;
    if (this.viewKind(p.view)) this.openPopup(p.view, p.width, p.ctx);
    else if (Date.now() >= p.until) this.message = `popup: no view ${p.view}`;
    else this.pendingPopup = p;
  }

  onNotify(method: string, params: Json) {
    if (method === "missed") { void this.resync(); return; }
    if (method.startsWith("stream.")) { this.terminals.onStream(method, params); return; }
    if (method !== "event") return;
    for (const c of this.ws.apply(params)) {
      switch (c.type) {
        case "resync": void this.resync(); return;
        case "entries": {
          this.settlePendingPopup();
          const s = this.win.settle(this.ws);
          if (typeof s === "object" && "Closed" in s) this.message = `workspace ${s.Closed.was} was closed`;
          if (!c.author.startsWith("ext:") && !c.author.startsWith("client:")) this.message = `workspace changed by ${c.author}`;
          break;
        }
        case "resource": this.terminals.onResource(c.id, c.available, c.live, c.restored); break;
        case "failed": this.message = `extension ${c.name} failed: ${c.reason}`; break;
        case "safe_mode": this.safeModeChanged(c.on, c.was); break;
        case "notify": this.toast = { text: c.text, style: c.style, until: Date.now() + 4000 }; setTimeout(() => this.redraw(), 4100); break;
        case "backend": this.message = `backend ${c.name} disconnected: ${c.error}`; break;
      }
    }
    this.redraw();
  }

  safeModeChanged(on: boolean, was: boolean) {
    if (on && !was) { this.safeSelected = new Set(this.ws.safeModeAllow); this.pstate.delete("@popup"); this.overlay = { kind: "popup", view: m.SAFE_MODE_VIEW, width: 76, ctx: {} }; }
    if (!on && this.overlay?.kind === "popup" && this.overlay.view === m.SAFE_MODE_VIEW) this.overlay = null;
  }

  redraw() { this.renderer.requestRender(); }

  // ---- views and context ------------------------------------------------------------------------------------------

  layoutRoot(): Json | null { const l = this.win.shown(this.ws); return l ? this.ws.entries.get(`layout/${l}`) ?? null : null; }

  shownView(view: string): string | null {
    if (view === m.SAFE_MODE_VIEW) return view;
    return m.shownView(view, (id) => m.merged(id, this.ws.entries, this.ws.live), (v) => m.renders(v, KINDS, FEATURES))[0];
  }

  viewKind(view: string): string {
    if (view === m.SAFE_MODE_VIEW) return "panel";
    const shown = this.shownView(view) ?? view;
    return this.ws.entries.get(`view/${shown}`)?.kind ?? "";
  }

  panelValue(view: string): Json {
    if (view === m.SAFE_MODE_VIEW) return m.safeChooser(this.ws, this.safeSelected);
    const shown = this.shownView(view) ?? view;
    if (!this.ws.entries.has(`view/${shown}`)) return {};
    return m.markCurrent(m.compose(shown, this.ws.entries, this.ws.live, FEATURES)[0], this.win.shown(this.ws));
  }

  viewTitle(view: string): string {
    const v = this.ws.entries.get(`view/${view}`) ?? {};
    if (typeof v.title === "string") return v.title;
    const r = this.ws.resources.get(v.resource);
    return r?.name ?? (r ? m.commandLabel(r) : view);
  }

  tabTitle(path: number[], index: number): string | null {
    let n = this.layoutRoot();
    for (const i of path) n = n?.children ? n.children[i] : n?.tabs ? n.tabs[i]?.node : null;
    return n?.tabs?.[index]?.title ?? null;
  }

  ctxLeaf(i: number): Json {
    const leaf = this.leaves[i];
    if (!leaf) return this.win.invocationContext(this.ws, {});
    const v = this.ws.entries.get(`view/${leaf.view}`) ?? {};
    const r = typeof v.resource === "string" ? this.ws.resources.get(v.resource) : null;
    const w = this.win.context(this.ws);
    const ctx: Json = { layout: w.layout, layout_version: w.layout_version, path: leaf.path, view: leaf.view, kind: v.kind ?? null,
      resource: v.resource ?? null, resource_kind: r?.kind ?? null, live: r?.state === "live", title: this.viewTitle(leaf.view), tab_title: null,
      zoomed: this.zoomed !== null };
    let n = this.layoutRoot();
    for (let d = 0; d < leaf.path.length && n; d++) {
      if (n.tabs) { ctx.tab_path = leaf.path.slice(0, d); ctx.tab_index = leaf.path[d]; ctx.tab_title = n.tabs[leaf.path[d]]?.title ?? null; }
      n = n.children ? n.children[leaf.path[d]] : n.tabs ? n.tabs[leaf.path[d]]?.node : null;
    }
    const shown = this.shownView(leaf.view);
    if (shown && shown !== leaf.view) ctx.shown_view = shown;
    // A panel's live `menu_context` joins as `menu.<key>` (core-protocol.md, menus).
    const extra = (shown ?? leaf.view) && this.ws.entries.get(`view/${shown ?? leaf.view}`)?.kind === "panel" ? this.panelValue(leaf.view)?.menu_context : null;
    if (extra && typeof extra === "object" && !Array.isArray(extra))
      for (const [k, x] of Object.entries(extra)) if (x === null || typeof x !== "object") ctx[`menu.${k}`] = x;
    return ctx;
  }

  // ---- commands ---------------------------------------------------------------------------------------------------

  /** Run an item (menu entry, binding, row): prompt, pick or options first when it asks. */
  activate(item: Json, ctx: Json) {
    if (Array.isArray(item.options)) {
      this.openOver({ kind: "menu", title: item.text ?? "", ctx, sel: 0, items: item.options.map((o: Json) => ({
        label: (o.value === item.value ? "● " : "  ") + (o.label ?? String(o.value)), command: item.command,
        args: { ...(item.args ?? {}), value: o.value }, input: o.input, prefill: o.prefill ?? item.prefill })) });
      return;
    }
    if (typeof item.input === "string") {
      let text = "";
      if (typeof item.prefill === "string") text = item.prefill;
      else if (isObj(item.prefill) && typeof ctx[item.prefill.context] === "string") text = ctx[item.prefill.context];
      this.openOver({ kind: "input", prompt: item.input, text, item, ctx, multi: item.multiline === true ? new m.Editor(text, item.max_bytes) : undefined });
      return;
    }
    if (item.pick === "resource") { this.openOver({ kind: "pick", item, ctx, sel: 0 }); return; }
    void this.run(item.command, item.args ?? {}, ctx);
  }

  openOver(o: Overlay) {
    if (this.overlay?.kind === "popup") this.under = this.overlay;
    this.overlay = o;
  }

  closeOverlay() { this.overlay = this.under; this.under = null; }

  async run(name: string, args: Json, ctx: Json): Promise<boolean> {
    if (!name) return false;
    if (name.startsWith("client.")) { this.clientCommand(name, args ?? {}, ctx); this.redraw(); return true; }
    this.message = `running ${name}…`;
    this.redraw();
    try {
      const reply = await this.peer.call("command.invoke", { name, args: { ...(isObj(args) ? args : {}), context: this.win.invocationContext(this.ws, ctx) } });
      const hints = reply?.client;
      const layout = m.replyLayout(name, reply);
      if (layout) this.win.show(this.ws, layout);
      if (isObj(hints)) {
        if (hints.close === true && this.overlay?.kind === "popup") this.overlay = null;
        if (isObj(hints.popup) && typeof hints.popup.view === "string") this.openPopup(hints.popup.view, hints.popup.width ?? 60, ctx);
        if (isObj(hints.focus) && typeof hints.focus.view === "string") {
          this.zoomed = null;
          setTimeout(() => { const i = this.leaves.findIndex((l) => l.view === hints.focus.view); if (i >= 0) this.focus = i; this.redraw(); }, 50);
        }
      }
      if (this.message === `running ${name}…`) this.message = `${name}: done`;
    } catch (e) {
      this.message = `${name}: ${(e as Error).message}`;
      this.redraw();
      return false;
    }
    this.redraw();
    return true;
  }

  clientCommand(name: string, args: Json, ctx: Json) {
    switch (name) {
      case "client.prefix": this.overlay = { kind: "prefix", sel: 0 }; break;
      case "client.palette": this.overlay = { kind: "palette", query: "", sel: 0 }; break;
      case "client.menu": {
        const place = args.where ?? "pane";
        const items = this.ws.menuEntries(place, ctx);
        if (!items.length) this.message = `no ${place} menu items (are their extensions on?)`;
        else this.openOver({ kind: "menu", title: place, items, ctx, sel: 0 });
        break;
      }
      case "client.focus": {
        const n = this.leaves.length || 1;
        if (args.dir === "last") {
          const i = this.leaves.findIndex((l) => l.view === this.focusLast);
          if (i < 0) { this.message = "no other pane was focused here"; break; }
          this.focus = i;
        } else this.focus = (this.focus + (args.dir === "prev" ? n - 1 : 1)) % n;
        this.zoomed = null;
        break;
      }
      case "client.tab": this.tabTo(args.to, ctx); break;
      case "client.zoom": this.zoomed = this.zoomed ? null : this.leaves[this.focus]?.view ?? null; break;
      case "client.layout": { const [ok, r] = this.win.clientLayout(this.ws, args); if (!ok) this.message = String(r); break; }
      case "client.quit": this.quit = true; this.renderer.destroy(); process.exit(0);
      case "client.safe_mode":
        if (this.ws.safeMode) { this.pstate.delete("@popup"); this.overlay = { kind: "popup", view: m.SAFE_MODE_VIEW, width: 76, ctx: {} }; }
        else void this.run("vqro.safe_mode", { on: true }, ctx);
        break;
      case "client.safe_mode.toggle": if (!this.safeSelected.delete(args.name)) this.safeSelected.add(args.name); break;
      case "client.safe_mode.apply": void (async () => {
        for (const key of this.ws.entries.keys()) {
          if (!key.startsWith("extension/")) continue;
          const n = key.slice(10);
          try { await this.peer.call("command.invoke", { name: "vqro.safe_mode.allow", args: { name: n, on: this.safeSelected.has(n) } }); }
          catch (e) { this.message = (e as Error).message; this.redraw(); return; }
        }
        this.message = "Safe-mode choices applied; enabled declarations were not changed";
        this.redraw();
      })(); break;
      case "client.safe_mode.leave": void this.run("vqro.safe_mode", { on: false }, ctx); break;
      case "client.panel": {
        const outerKey = ctx.panel;
        if (!outerKey) break;
        const outer = this.state(outerKey);
        // `pane`: "list" or "detail"; by default the focused one.
        let value = this.panelValue(ctx.panel_view ?? outerKey), key = outerKey, st = outer;
        const wantDetail = args.pane === "detail" || (args.pane !== "list" && outer.detailFocus);
        const det = wantDetail && listDetailOn(value, outer.page) ? this.selectedDetail(outerKey, value) : null;
        if (det) { key = `${outerKey}#detail`; st = this.state(key); value = det.value; }
        if (args.action === "filter") this.overlay = { kind: "filter", key };
        else if (args.action === "search") { st.search = ""; st.searchHit = 0; this.overlay = { kind: "filter", key, search: ctx.panel_view ?? key }; }
        else if (args.action === "search_next" || args.action === "search_prev") this.searchStep(key, ctx.panel_view ?? key, args.action === "search_next" ? 1 : -1);
        else if (args.action === "clear_filter") st.filter = "";
        else if (args.action === "facet") { if (typeof args.key === "string") chooseFacet(value, st, args.key, typeof args.value === "string" ? args.value : null); }
        else if (args.action === "sort") { const i = (m.declared(value, st.page, "sorts") ?? []).findIndex((s: Json) => s?.key === args.key); st.sort = i >= 0 ? i : null; }
        else if (args.action === "expand_all" || args.action === "collapse_all") {
          for (const r of m.rows(value, st.page, "", {}, FEATURES)) if (r.has_children) st.open[r.id] = args.action === "expand_all";
        } else if (args.action === "activate") this.panelKey(outerKey, ctx.panel_view ?? outerKey, "Enter", ctx);
        else {  // navigation for pane-scoped keys: down/up/first/last/end, next/prev matching row, toggle
          const rows = rowsOf(value, st);
          const sel = rows.map((r, i) => (m.selectable(r.item) ? i : -1)).filter((i) => i >= 0);
          st.manual = false;
          switch (args.action) {
            case "down": st.sel = sel.find((i) => i > st.sel) ?? st.sel; break;
            case "up": st.sel = [...sel].reverse().find((i) => i < st.sel) ?? st.sel; break;
            case "first": st.sel = sel[0] ?? 0; break;
            case "last": st.sel = sel[sel.length - 1] ?? 0; break;
            case "end": { const listed = sel.filter((i) => !rows[i].pinned && !rows[i].bottom); st.sel = listed[listed.length - 1] ?? st.sel; st.followEnd = null; break; }
            case "next": case "prev": { const i = m.findRow(rows, st.sel, args.action === "next", args.match); if (i !== null) st.sel = i; break; }
            case "toggle": { const r = rows[st.sel]; if (r?.has_children) st.open[r.id] = !r.expanded; break; }
            default: this.message = `client.panel: unknown action ${args.action}`;
          }
        }
        break;
      }
      case "client.composer": this.composerCommand(args, ctx); break;
      case "client.copy": {
        // `text`, or the selected row's `copy` (else its text), through the terminal (OSC 52); the terminal reports
        // nothing back, so the notice says what was sent.
        const str = (x: any): x is string => typeof x === "string";
        let t: string | null = str(args.text) ? args.text : null;
        if (t === null) {
          const key = ctx.panel ?? this.leaves[this.focus]?.view;
          const st = key ? this.pstate.get(key) : undefined;
          const item = key && st ? rowsOf(this.panelValue(ctx.panel_view ?? key), st)[st.sel]?.item : undefined;
          t = str(item?.copy) ? item.copy : str(item?.text) ? item.text : null;
        }
        if (t === null) { this.message = "copy: nothing selected"; break; }
        // OpenTUI writes OSC 52 only when the terminal reported support; say which happened.
        this.message = this.renderer.copyToClipboardOSC52(t)
          ? `copy: sent ${[...t].length} characters to the terminal clipboard (OSC 52; some terminals ignore it)`
          : "copy: this terminal did not report clipboard support (OSC 52); nothing sent";
        break;
      }
      case "client.none": break;
      default: this.message = `${name}: not supported by this client`;
    }
  }

  tabTo(to: Json, ctx: Json) {
    const path: number[] = ctx.tab_path ?? [];
    const name = this.win.shown(this.ws);
    const root = this.layoutRoot();
    if (!name || !root) return;
    const copy = structuredClone(root);
    let n = copy;
    for (const i of path) n = n.children ? n.children[i] : n.tabs[i].node;
    if (!n?.tabs) return;
    const count = n.tabs.length, cur = n.active ?? 0;
    const next = to === "next" ? (cur + 1) % count : to === "prev" ? (cur + count - 1) % count : Number(to) - 1;
    if (!(next >= 0 && next < count) || next === cur) return;
    n.active = next;
    void this.peer.call("workspace.apply", { note: "select tab", ops: [{ op: "set", key: `layout/${name}`, value: copy, expect: this.ws.versions.get(`layout/${name}`) }] })
      .catch((e) => { this.message = (e as Error).message; this.redraw(); });
  }

  // ---- keys -------------------------------------------------------------------------------------------------------

  prefixKey(): string { return m.normalize(String(this.ws.entries.get("doc/settings/tui")?.prefix ?? "")) ?? "C-g"; }
  prefix(): string { const p = this.prefixKey(); return /^(C-|M-)/.test(p) || /^F\d+$/.test(p) ? p : "C-g"; }

  onKey(k: KeyEvent) {
    this.settlePendingPopup();
    const name = keyName(k);
    if (!name) return;
    const o = this.overlay;
    const isPrefix = name === this.prefix();
    // The prefix always wins (recovery): it cancels a prompt and reaches the prefix menu even while typing.
    if (isPrefix && o?.kind !== "prefix") { if (o?.kind === "input") this.closeOverlay(); this.overlay = { kind: "prefix", sel: 0 }; this.redraw(); return; }
    if (o) { this.overlayKey(o, name, k); this.redraw(); return; }
    // After a `repeat` binding: its key (or another `repeat` key) runs again without the prefix for a moment.
    const until = this.repeatUntil;
    this.repeatUntil = 0;
    if (!isPrefix && Date.now() < until) {
      const b = this.ws.bindingFor(name, this.ctxLeaf(this.focus));
      if (b?.repeat === true) { this.activate(b, this.ctxLeaf(this.focus)); this.armRepeat(b); this.redraw(); return; }
    }
    const leaf = this.leaves[this.focus];
    if (leaf && this.viewKind(leaf.view) === "terminal") { this.terminals.key(leaf.view, k); return; }
    if (leaf && this.viewKind(leaf.view) === "panel") {
      // Pane- and composer-scoped bindings (D29): keys without the prefix, before the panel's own keys.
      const ctx = this.ctxLeaf(this.focus);
      const typing = this.pstate.get(leaf.view)?.editing != null;
      const b = this.ws.scopedBinding(name, typing ? "composer" : "pane", ctx);
      if (b) this.activate(b, { ...ctx, panel: leaf.view, panel_view: leaf.view });
      else this.panelKey(leaf.view, leaf.view, name, ctx);
    }
    this.redraw();
  }

  onPaste(e: PasteEvent) {
    const leaf = this.leaves[this.focus];
    if (!this.overlay && leaf && this.viewKind(leaf.view) === "terminal") this.terminals.paste(leaf.view, e.bytes);
    else if (this.overlay?.kind === "input") {
      const text = new TextDecoder().decode(e.bytes);
      if (this.overlay.multi) { if (this.overlay.multi.insert(text) === "refused") this.message = "paste refused: over the prompt's size limit"; }
      else this.overlay.text += text.replace(/\n/g, " ");
      this.redraw();
    } else if (!this.overlay && leaf) {
      const st = this.pstate.get(leaf.view);
      const c = st?.editing ? st.composers.get(st.editing) : undefined;
      if (c && c.editor.insert(new TextDecoder().decode(e.bytes)) === "refused") this.message = "paste refused: over the composer's size limit";
      this.redraw();
    }
  }

  overlayKey(o: Overlay, name: string, k: KeyEvent) {
    const list = (n: number, sel: number) => name === "Up" ? Math.max(0, sel - 1) : name === "Down" ? Math.min(n - 1, sel + 1) : sel;
    switch (o.kind) {
      case "prefix": {
        const rows = m.prefixRows(this.ws, this.ctxLeaf(this.focus));
        if (name === this.prefix()) {  // twice: to the program
          this.overlay = null;
          const leaf = this.leaves[this.focus];
          if (leaf && this.viewKind(leaf.view) === "terminal") this.terminals.key(leaf.view, k);
          return;
        }
        const b = this.ws.bindingFor(name, this.ctxLeaf(this.focus));
        if (b) { this.overlay = null; this.activate(b, this.ctxLeaf(this.focus)); this.armRepeat(b); return; }
        if (name === "Up" || name === "Down") { o.sel = list(rows.length, o.sel); return; }
        if (name === "Enter") { const r = rows[o.sel]; this.overlay = null; if (r) this.activate(r.binding, this.ctxLeaf(this.focus)); return; }
        this.overlay = null;
        if (name !== "Esc") this.message = `no binding for ${this.prefix()} ${name}`;
        return;
      }
      case "palette": {
        const names = this.paletteItems(o.query);
        if (name === "Esc") this.overlay = null;
        else if (name === "Enter") { this.overlay = null; if (names[o.sel]) void this.run(names[o.sel], {}, this.ctxLeaf(this.focus)); }
        else if (name === "Up" || name === "Down") o.sel = list(names.length, o.sel);
        else if (name === "Backspace") { o.query = o.query.slice(0, -1); o.sel = 0; }
        else if ([...k.sequence].length === 1 && k.sequence >= " ") { o.query += k.sequence; o.sel = 0; }
        return;
      }
      case "menu": {
        if (name === "Esc") this.closeOverlay();
        else if (name === "Up" || name === "Down") o.sel = list(o.items.length, o.sel);
        else if (name === "Enter") { const it = o.items[o.sel]; this.closeOverlay(); if (it) this.activate({ ...it, options: undefined }, o.ctx); }
        return;
      }
      case "pick": {
        const res = [...this.ws.resources.values()];
        if (name === "Esc") this.closeOverlay();
        else if (name === "Up" || name === "Down") o.sel = list(res.length, o.sel);
        else if (name === "Enter") { this.closeOverlay(); const r = res[o.sel]; if (r) void this.run(o.item.command, { ...(o.item.args ?? {}), picked: r.id }, o.ctx); }
        return;
      }
      case "input": {
        if (o.multi) {  // a multi-line prompt (feature multiline): the shared editor
          const outcome = o.multi.key(name === "Space" ? " " : name);
          if (outcome === "submit") { this.closeOverlay(); void this.run(o.item.command, { ...(o.item.args ?? {}), text: o.multi.string() }, o.ctx); }
          else if (outcome === "leave") this.closeOverlay();
          else if (outcome === "refused") this.message = "too long: the prompt's size limit";
          return;
        }
        if (name === "Esc") this.closeOverlay();
        else if (name === "Enter") { this.closeOverlay(); void this.run(o.item.command, { ...(o.item.args ?? {}), text: o.text }, o.ctx); }
        else if (name === "Backspace") o.text = o.text.slice(0, -1);
        else if ([...k.sequence].length === 1 && k.sequence >= " ") o.text += k.sequence;
        return;
      }
      case "filter": {
        const st = this.pstate.get(o.key);
        if (!st) { this.overlay = this.under; return; }
        if (o.search !== undefined) {  // a search: each change jumps to the first match; Enter keeps it for n/N
          if (name === "Esc") { st.search = ""; this.message = ""; this.overlay = this.under; this.under = null; return; }
          if (name === "Enter") { this.overlay = this.under; this.under = null; return; }
          if (name === "Backspace") st.search = st.search.slice(0, -1);
          else if ([...k.sequence].length === 1 && k.sequence >= " ") st.search += k.sequence;
          else return;
          st.searchHit = 0;
          this.searchStep(o.key, o.search, 0);
          return;
        }
        if (name === "Esc") { st.filter = ""; this.overlay = this.under; this.under = null; }
        else if (name === "Enter") { this.overlay = this.under; this.under = null; }
        else if (name === "Backspace") st.filter = st.filter.slice(0, -1);
        else if ([...k.sequence].length === 1 && k.sequence >= " ") { st.filter += k.sequence; st.sel = 0; st.top = 0; }
        return;
      }
      case "popup": {
        if (name === "Esc") { this.overlay = null; return; }
        this.panelKey("@popup", o.view, name, { ...o.ctx, panel: "@popup", panel_view: o.view });
        return;
      }
    }
  }

  paletteItems(query: string): string[] {
    const q = query.toLowerCase();
    return [...this.ws.commands.values()].filter((c) => c.available === true && (c.name.toLowerCase().includes(q) || String(c.title ?? "").toLowerCase().includes(q)))
      .map((c) => c.name).sort();
  }

  state(key: string): PState {
    let st = this.pstate.get(key);
    if (!st) { st = { sel: 0, top: 0, page: 0, filter: "", open: {}, chip: 0, followEnd: null, manual: false, detailFocus: false, detailFor: "", composers: new Map(), editing: null, search: "", searchHit: 0, seenPages: [], facets: {}, sort: null }; this.pstate.set(key, st); }
    return st;
  }

  panelKey(key: string, view: string, name: string, ctx: Json) {
    const value = this.panelValue(view);
    const st = this.state(key);
    // list_detail (core-protocol.md, Optional list with detail): Tab switches list/detail; keys go to the focused one.
    const detail = listDetailOn(value, st.page) ? this.selectedDetail(key, value) : null;
    if (!st.editing && listDetailOn(value, st.page) && (name === "Tab" || name === "BackTab")) { st.detailFocus = !st.detailFocus && detail !== null; return; }
    // The focused pane is a full panel: its own pages (the list's when the detail has none), filter, facets, sort.
    const detailPages = Array.isArray(detail?.value?.pages) && detail.value.pages.length > 0;
    if (detail && st.detailFocus && !st.editing && !((name === "[" || name === "]") && !detailPages)) { this.valueKey(`${key}#detail`, detail.value, name, ctx); return; }
    if (!detail) st.detailFocus = false;
    this.valueKey(key, value, name, ctx);
  }

  /** The selected row's `detail_panel` (and its id) when the list_detail enhancement is on; a new row resets the
   *  detail's selection and scroll. */
  /** Show search match `searchHit + delta` (wrapping): open the rows around it and select it. */
  searchStep(key: string, view: string, delta: number, value: Json = this.panelValue(view)) {
    const st = this.state(key);
    const hits = m.search(value, st.search, st.page, st.filter, FEATURES, st.facets, st.sort);
    if (!hits.length) { this.message = st.search ? `/${st.search}: no match` : ""; return; }
    st.searchHit = (((st.searchHit + delta) % hits.length) + hits.length) % hits.length;
    const [id, open] = hits[st.searchHit];
    for (const a of open) st.open[a] = true;
    const at = rowsOf(value, st).findIndex((r) => r.id === id);
    if (at >= 0) { st.sel = at; st.manual = false; }
    this.message = `/${st.search}  ${st.searchHit + 1} of ${hits.length}  n/N  Esc`;
  }

  selectedDetail(key: string, value: Json): { id: string; value: Json } | null {
    const st = this.state(key);
    const rows = rowsOf(value, st);
    const at = rows[st.sel] && m.selectable(rows[st.sel].item) ? st.sel : rows.findIndex((r) => m.selectable(r.item));
    const row = rows[at];
    if (!row || !isObj(row.item.detail_panel)) return null;
    if (st.detailFor !== row.id) { st.detailFor = row.id; this.pstate.delete(`${key}#detail`); }
    return { id: row.id, value: row.item.detail_panel };
  }

  composerOf(st: PState, row: m.Row): m.Composer {
    let c = st.composers.get(row.id);
    if (!c) { c = new m.Composer(row.item); st.composers.set(row.id, c); }
    return c;
  }

  /** Send a composer's draft; it is cleared only after a successful reply (on an error it stays). */
  sendComposer(key: string, row: m.Row, c: m.Composer, ctx: Json) {
    if (typeof row.item.command !== "string") { this.message = "composer has no command"; return; }
    const args = c.submitArgs(row.item), sent = c.editor.string();
    void this.run(row.item.command, args, ctx).then((ok) => { if (ok && c.editor.string() === sent) { c.editor = new m.Editor("", row.item.max_bytes, row.item.submit); this.redraw(); } });
  }

  /** `client.composer {action: set {panel, row, arg, value} | focus | send {set?}}` (client-local). */
  composerCommand(args: Json, ctx: Json) {
    const key = args.panel ?? ctx.panel ?? this.leaves[this.focus]?.view;
    if (!key) return;
    const view = ctx.panel_view ?? (key === "@popup" && this.overlay?.kind === "popup" ? this.overlay.view : key);
    const st = this.state(key);
    const rows = rowsOf(this.panelValue(view), st);
    const at = rows.findIndex((r) => m.kind(r.item) === "composer" && (args.row == null || args.row === r.id));
    if (at < 0) { this.message = "no composer here"; return; }
    const c = this.composerOf(st, rows[at]);
    if (isObj(args.set)) Object.assign(c.values, args.set);
    if (args.action === "set") c.values[args.arg] = args.value;
    else if (args.action === "send") { if (c.editor.string().trim()) this.sendComposer(key, rows[at], c, ctx); else this.message = "nothing to send"; }
    else { st.sel = at; st.editing = rows[at].id; st.chip = 0; }
  }

  /** Composer `autofocus`: when a pane gains focus, typing goes straight into its composer. */
  private autofocused: string | null = null;
  autofocus() {
    const view = this.leaves[this.focus]?.view;
    if (!view || view === this.autofocused) return;
    this.autofocused = view;
    if (this.viewKind(view) !== "panel") return;
    const st = this.state(view);
    const rows = rowsOf(this.panelValue(view), st);
    const at = rows.findIndex((r) => m.kind(r.item) === "composer" && r.item.autofocus === true);
    if (at >= 0) { st.sel = at; st.editing = rows[at].id; }
  }

  valueKey(key: string, value: Json, name: string, ctx: Json) {
    const st = this.state(key);
    const rows = rowsOf(value, st);
    const sel = (i: number) => rows.findIndex((r, j) => j >= i && m.selectable(r.item));
    const step = (delta: number) => {
      let i = st.sel;
      for (let n = Math.abs(delta); n > 0;) { const j = i + Math.sign(delta); if (j < 0 || j >= rows.length) break; i = j; if (m.selectable(rows[i].item)) n--; }
      if (rows[i] && m.selectable(rows[i].item)) st.sel = i;
    };
    st.manual = false;
    // Composer (feature composer): while typing, keys edit its draft (the prefix was handled before).
    if (st.editing) {
      const r = rows.find((x) => x.id === st.editing && m.kind(x.item) === "composer");
      if (r) {
        const c = this.composerOf(st, r);
        if (name === "Tab") { st.editing = null; st.chip = 1; return; }
        const outcome = c.editor.key(name === "Space" ? " " : name);
        if (outcome === "submit") this.sendComposer(key, r, c, ctx);
        else if (outcome === "leave") st.editing = null;
        else if (outcome === "refused") this.message = "too long: the composer's size limit";
        return;
      }
      st.editing = null;
    }
    // An active search: n/N step through matches, Esc ends it (feature search).
    if (st.search && (name === "n" || name === "N" || name === "Esc")) {
      if (name === "Esc") { st.search = ""; this.message = ""; }
      else this.searchStep(key, ctx.panel_view ?? key, name === "n" ? 1 : -1, value);
      return;
    }
    const row = rows[st.sel];
    if (row && m.kind(row.item) === "composer") {
      const c = this.composerOf(st, row);
      const buttons = composerButtons(row.item);
      if (name === "Left") { st.chip = Math.max(0, st.chip - 1); return; }
      if (name === "Right") { st.chip = Math.min(buttons.length, st.chip + 1); return; }
      if ((name === "Enter" || name === "Space" || name === "i") && st.chip === 0) { st.editing = row.id; return; }
      if (name === "Enter" || name === "Space") {
        const b = buttons[st.chip - 1];
        if (!b) return;
        if (b.send) { if (c.editor.string().trim()) this.sendComposer(key, row, c, ctx); else this.message = "nothing to send"; }
        else if (b.value !== undefined) c.values[b.arg] = b.value;
        else {  // a select control: pick a value for this composer only (client-local until Send)
          const ctl = m.composerControls(row.item).find((x) => x.arg === b.arg);
          this.openOver({ kind: "menu", title: ctl?.text ?? b.arg, ctx, sel: 0, items: (ctl?.options ?? []).map((o: Json) => ({
            label: (o.value === c.values[b.arg] ? "● " : "  ") + (o.label ?? String(o.value)), command: "client.composer",
            args: { action: "set", panel: key, row: row.id, arg: b.arg, value: o.value } })) });
        }
        return;
      }
    }
    switch (name) {
      case "Up": case "k": step(-1); break;
      case "Down": case "j": step(1); break;
      case "PageUp": step(-10); break;
      case "PageDown": step(10); break;
      case "Home": st.sel = Math.max(0, sel(0)); break;
      case "End": { for (let i = rows.length - 1; i >= 0; i--) if (m.selectable(rows[i].item)) { st.sel = i; break; } break; }
      case "Left":
        if (row && m.kind(row.item) === "chips") st.chip = Math.max(0, st.chip - 1);
        else if (row?.has_children && row.expanded) st.open[row.id] = false;
        else if (row && row.parent !== null) st.sel = row.parent;
        break;
      case "Right":
        if (row && m.kind(row.item) === "chips") st.chip = Math.min((row.item.chips?.length ?? 1) - 1, st.chip + 1);
        else if (row?.has_children && !row.expanded) st.open[row.id] = true;
        break;
      case "[": st.page = Math.max(0, st.page - 1); st.sel = 0; st.top = 0; break;
      case "]": st.page = Math.min(Math.max(0, (value.pages?.length ?? 1) - 1), st.page + 1); st.sel = 0; st.top = 0; break;
      case "/":  // `/` searches panels that ask for it (`search: true`), else filters
        if (value.search === true) { st.search = ""; st.searchHit = 0; this.openOver({ kind: "filter", key, search: ctx.panel_view ?? key }); }
        else this.openOver({ kind: "filter", key });
        break;
      case "f": if (m.declared(value, st.page, "facets")?.length) cycleFacet(value, st); break;
      case "s": if (m.declared(value, st.page, "sorts")?.length) st.sort = cycleSort(value, st); break;
      case "m": if (row && Array.isArray(row.item.menu)) this.openOver({ kind: "menu", title: row.item.text ?? "", items: row.item.menu, ctx, sel: 0 }); break;
      case "Enter": case "Space": {
        if (!row || !m.selectable(row.item)) break;
        const item = m.kind(row.item) === "chips" ? row.item.chips?.[st.chip] : row.item;
        if (item && typeof item.command === "string" && item.disabled !== true) this.activate(item, ctx);
        else if (row.has_children) st.open[row.id] = !row.expanded;
        break;
      }
    }
  }

  // ---- mouse ------------------------------------------------------------------------------------------------------

  /** The terminal view a press started in keeps its drag and release, even outside its pane. */
  private mouseOwner: string | null = null;

  onMouse(e: MouseEvent) {
    // Terminals: the program gets the mouse when it asked for it (its own encoding); otherwise wheel is scrollback.
    const owner = this.mouseOwner && (e.type === "drag" || e.type === "move" || e.type === "up" || e.type === "drag-end") ? this.mouseOwner : null;
    const at = this.overlay ? -1 : this.leaves.findIndex((l) => inside(l.inner, e.x, e.y));
    const view = owner ?? (at >= 0 ? this.leaves[at].view : null);
    if (view && this.viewKind(view) === "terminal") {
      if (e.type === "down") { this.focus = at; this.mouseOwner = view; }
      if (e.type === "up" || e.type === "drag-end") this.mouseOwner = null;
      if (this.terminals.mouse(view, e) || e.type !== "down") { this.redraw(); return; }
    } else if (e.type === "scroll") {
      // Panels: the list or, under the pointer, its detail scrolls by hand (independently).
      const key = this.overlay?.kind === "popup" ? "@popup" : at >= 0 ? this.leaves[at].view : null;
      if (key) {
        const detail = this.listed.get(`${key}#detail`);
        const st = this.state(detail?.area && inside(detail.area, e.x, e.y) ? `${key}#detail` : key);
        st.manual = true;
        st.top = Math.max(0, st.top + (e.scroll?.direction === "up" ? -3 : 3));
      }
      this.redraw();
      return;
    }
    if (e.type !== "down") return;
    for (let i = this.hits.length - 1; i >= 0; i--) if (inside(this.hits[i].rect, e.x, e.y)) { this.hits[i].act(e); this.redraw(); return; }
    if (this.overlay && this.overlay.kind !== "input") { this.overlay = null; this.redraw(); }
  }
}

/** Menus, prompts, the prefix menu and popups, drawn above terminal panes; it also receives the mouse. */
export class Top extends Renderable {
  app!: App;
  protected renderSelf(buf: OptimizedBuffer) { if (this.app) this.app.screen.overlays(buf, this.width, this.height); }
}

const inside = (r: Rect, x: number, y: number) => x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h;

/** The whole window: tab bars, panes, panels, status bar and overlays; terminals are separate renderables on top. */
export class Screen extends Renderable {
  app!: App;
  protected renderSelf(buf: OptimizedBuffer) {
    const a = this.app;
    if (!a) return;
    a.pal = new Palette(a.ws.entries.get("doc/settings/theme"), a.ws.safeMode);
    a.hits = [];
    const W = this.width, H = this.height;
    buf.fillRect(0, 0, W, H, a.pal.bg);
    const body: Rect = { x: 0, y: 0, w: W, h: Math.max(0, H - 1) };
    const leaves: Leaf[] = [];
    const root = a.layoutRoot();
    if (!root) text(buf, 1, 1, "no workspace yet: Ctrl-g R resets the layout", a.pal.color("muted"));
    else if (a.zoomed) leaves.push({ view: a.zoomed, rect: body, inner: inset(body), path: a.leaves.find((l) => l.view === a.zoomed)?.path ?? [] });
    else this.walk(buf, root, body, [], leaves);
    a.leaves = leaves;
    if (a.focus >= leaves.length) a.focus = Math.max(0, leaves.length - 1);
    const now = leaves[a.focus]?.view ?? null;
    if (now !== null && now !== a.focusNow) { a.focusLast = a.focusNow; a.focusNow = now; }
    a.autofocus();
    const seen = new Set<string>();
    leaves.forEach((leaf, i) => { seen.add(leaf.view); this.pane(buf, leaf, i); });
    a.terminals.layout(leaves.filter((l) => a.viewKind(l.view) === "terminal"), a.overlay !== null && a.overlay.kind !== "input" && a.overlay.kind !== "filter");
    this.status(buf, { x: 0, y: H - 1, w: W, h: 1 });
  }

  private walk(buf: OptimizedBuffer, node: Json, r: Rect, path: number[], out: Leaf[]) {
    const a = this.app;
    if (r.w <= 0 || r.h <= 0 || !isObj(node)) return;
    if (typeof node.view === "string") { out.push({ view: node.view, rect: r, inner: inset(r), path }); return; }
    if (Array.isArray(node.children) && node.children.length) {
      const n = node.children.length;
      const weights: number[] = Array.isArray(node.sizes) && node.sizes.length === n ? node.sizes.map((s: Json) => Math.max(1, Number(s) || 1)) : Array(n).fill(1);
      const total = weights.reduce((x, y) => x + y, 0);
      const side = node.split !== "v";
      const span = side ? r.w : r.h;
      let at = 0;
      node.children.forEach((c: Json, i: number) => {
        const len = i === n - 1 ? span - at : Math.round((span * weights[i]) / total);
        const sub = side ? { x: r.x + at, y: r.y, w: len, h: r.h } : { x: r.x, y: r.y + at, w: r.w, h: len };
        at += len;
        this.walk(buf, c, sub, [...path, i], out);
      });
      return;
    }
    if (Array.isArray(node.tabs) && node.tabs.length) {
      const active = Math.min(node.active ?? 0, node.tabs.length - 1);
      buf.fillRect(r.x, r.y, r.w, 1, a.pal.color("tab_bar"));
      let x = r.x;
      // Labels and chip spacing from the shared model (doc/settings/tabs); colours from the palette.
      const style = m.tabStyle(a.ws.entries.get(m.TABS_ENTRY));
      node.tabs.forEach((t: Json, i: number) => {
        if (i > 0 && style.chips) x += 1;
        const only = typeof t?.node?.view === "string" ? a.ws.entries.get(`view/${t.node.view}`) : null;
        const shown = m.tabTitle(a.ws.entries.get(m.TABS_ENTRY), t?.title ?? "tab", only ? a.ws.resources.get(only.resource) ?? null : null);
        let label = m.tabLabel(style, i, clean(shown));
        const on = i === active;
        const bg = on ? a.pal.color("tab_active") : style.chips ? a.pal.color("surface") : a.pal.color("tab_bar");
        const mark = a.tabMark(t, on);
        const glyph = mark ? iconText(a, { name: mark, text: ({ blocked: "⚠", error: "✗", done: "✓" } as Json)[mark] ?? "•" }) : "";
        if (glyph) label = label.replace(/ $/, "");
        const ended = m.tabState(t) !== null || t.title === "unknown command";  // programs ended (tabs extension hint): dim, title unchanged
        text(buf, x, r.y, fit(label, r.x + r.w - x), on ? a.pal.color("tab_active_text") : a.pal.color(ended ? "dim" : "muted"), bg,
          on ? (ended ? TextAttributes.DIM : TextAttributes.BOLD) : TextAttributes.NONE);
        if (glyph) {  // inside the chip after the name, in the mark's semantic colour
          const color = a.pal.color(({ blocked: "warn", error: "error", done: "ok" } as Json)[mark!] ?? "info");
          text(buf, x + width(label), r.y, fit(` ${glyph} `, r.x + r.w - x - width(label)), color, bg);
          label += ` ${glyph} `;
        }
        const rect = { x, y: r.y, w: width(label), h: 1 };
        a.hits.push({ rect, act: (e) => {
          const gesture = e.button === 2 ? a.ws.gesture("mouse.right", "tab") : e.button === 1 ? a.ws.gesture("mouse.middle", "tab") : null;
          const w = a.win.context(a.ws);
          if (gesture) a.activate(gesture, { layout: w.layout, layout_version: w.layout_version, tab_path: path, tab_index: i, tab_title: t?.title ?? null });
          else a.tabTo(i + 1, { tab_path: path });
        } });
        x += width(label);
      });
      if (style.chips) x += 1;
      if (path.length === 0 || !a.hits.some((h) => (h as any).tabbar)) {
        for (const item of a.ws.menuEntries("tabbar", a.win.invocationContext(a.ws, {}))) {
          const glyph = iconText(a, item.icon);
          const label = item.icon_after && glyph ? ` ${item.label ?? ""}${glyph} `  // compact "+◆" button
            : ` ${glyph}${item.label ? (glyph ? " " : "") + item.label : ""} `;
          if (x + width(label) > r.x + r.w) break;
          text(buf, x, r.y, label, a.pal.color("control"), a.pal.color("tab_bar"));
          const hit: any = { rect: { x, y: r.y, w: width(label), h: 1 }, act: () => a.activate(item, a.win.invocationContext(a.ws, { tab_path: path })), tabbar: true };
          a.hits.push(hit);
          x += width(label);
        }
      }
      this.walk(buf, node.tabs[active]?.node, { x: r.x, y: r.y + 1, w: r.w, h: r.h - 1 }, [...path, active], out);
    }
  }

  private pane(buf: OptimizedBuffer, leaf: Leaf, i: number) {
    const a = this.app;
    const focused = i === a.focus && !a.overlay;
    const border = focused ? a.pal.color("border_focus") : a.pal.color("border");
    box(buf, leaf.rect, border, a.pal.bg);
    const kind = a.viewKind(leaf.view);
    let title = a.viewTitle(leaf.view);
    if (kind === "panel") title = a.panelValue(leaf.view).title ?? title;
    if (kind === "terminal") title = a.terminals.title(leaf.view, title);
    text(buf, leaf.rect.x + 1, leaf.rect.y, fit(` ${clean(title)} `, leaf.rect.w - 2), title.startsWith("unknown command") ? a.pal.color("dim") : border);
    a.hits.push({ rect: leaf.rect, act: (e) => {
      a.focus = i;
      if (kind === "panel") this.click(leaf.view, leaf.view, e, a.ctxLeaf(i));
      const gesture = e.button === 2 ? a.ws.gesture("mouse.right", "pane") : e.button === 1 ? a.ws.gesture("mouse.middle", "pane") : null;
      if (gesture) a.activate(gesture, a.ctxLeaf(i));  // what a gesture means comes from bindings (D17)
    } });
    if (kind === "panel") drawPanel(a, buf, leaf.inner, leaf.view, leaf.view, focused);
    else if (kind !== "terminal") {
      const v = a.ws.entries.get(`view/${leaf.view}`);
      text(buf, leaf.inner.x, leaf.inner.y, fit(v ? `view ${leaf.view}: kind \`${v.kind}\` is not supported by this client` : `view ${leaf.view} is missing`, leaf.inner.w), a.pal.color("muted"));
    }
  }

  private click(key: string, _view: string, e: MouseEvent, ctx: Json) {
    const a = this.app;
    const detail = a.listed.get(`${key}#detail`);
    const outer = a.state(key);
    if (detail && detail.area && inside(detail.area, e.x, e.y)) {
      outer.detailFocus = true;
      const hit = detail.hits.find((h) => h.y === e.y && e.x >= h.x && e.x < h.x + h.w);
      const dv = a.selectedDetail(key, a.panelValue(_view));
      if (!hit || !dv) return;
      const st = a.state(`${key}#detail`);
      st.sel = hit.i;
      if (hit.chip !== undefined) st.chip = hit.chip;
      if (e.button === 0) a.valueKey(`${key}#detail`, dv.value, "Enter", ctx);
      return;
    }
    const listed = a.listed.get(key);
    if (listed?.area && inside(listed.area, e.x, e.y)) outer.detailFocus = false;
    const hit = listed?.hits.find((h) => h.y === e.y && e.x >= h.x && e.x < h.x + h.w);
    if (!hit) return;
    const st = a.state(key);
    st.sel = hit.i;
    const row = listed!.rows[hit.i];
    if (hit.chip !== undefined) st.chip = hit.chip;
    else if (m.kind(row.item) === "composer") st.chip = 0;  // the text area: click to type
    const toggle = m.kind(row.item) === "toggle" && e.x <= hit.x + 4;
    // A composer: a click on a control or Send acts on it, never as a key for the draft being typed (that would
    // send it); a click on the text while typing just keeps typing.
    if (m.kind(row.item) === "composer" && st.editing) {
      if (hit.chip === undefined) return;
      const typing = st.editing;
      st.editing = null;
      if (e.button === 0) a.panelKey(key, _view, "Enter", ctx);
      if (!st.editing) { st.editing = typing; st.chip = 0; }
      return;
    }
    if (hit.chip !== undefined || toggle || !row.item.detail_panel) {
      if (e.button === 2 && Array.isArray(row.item.menu)) a.openOver({ kind: "menu", title: row.item.text ?? "", items: row.item.menu, ctx, sel: 0 });
      else if (e.button === 0) a.panelKey(key, _view, "Enter", ctx);
    }
  }

  private status(buf: OptimizedBuffer, r: Rect) {
    const a = this.app;
    const p = a.pal;
    buf.fillRect(r.x, r.y, r.w, 1, p.bg);
    const open = a.overlay?.kind === "prefix";
    const badge = " vqro ▾ ";
    text(buf, r.x, r.y, badge, open ? p.color("badge_text") : p.color("accent"), open ? p.color("prompt") : p.bg, TextAttributes.BOLD);
    a.hits.push({ rect: { x: r.x, y: r.y, w: width(badge), h: 1 }, act: () => { a.overlay = a.overlay?.kind === "prefix" ? null : { kind: "prefix", sel: 0 }; } });
    let x = r.x + width(badge) + 1;
    if (a.overlay?.kind === "input" || a.overlay?.kind === "filter") {
      const o = a.overlay;
      const line = o.kind === "input" ? (o.multi ? `${o.prompt}: (multi-line, above)` : `${o.prompt}: ${o.text}▏`) : (o.search !== undefined ? `/${a.pstate.get(o.key)?.search ?? ""}▏  ${a.message}` : `/${a.pstate.get(o.key)?.filter ?? ""}▏`);
      text(buf, x, r.y, fit(line, r.w - x), p.color("prompt"));
      return;
    }
    const failed = [...a.ws.extensions.values()].filter((s) => s?.status === "failed").length;
    const notes: [string, string, () => void][] = [];
    if (a.ws.safeMode) notes.push(["safe mode", "warn", () => a.clientCommand("client.safe_mode", {}, {})]);
    if (failed) notes.push([`${failed} failed · recover`, "error", () => { a.overlay = { kind: "prefix", sel: 0 }; }]);
    if (a.ws.backendNotice) notes.push([a.ws.backendNotice, "warn", () => a.clientCommand("client.safe_mode", {}, {})]);
    for (const [label, style, act] of notes) {
      text(buf, x, r.y, fit(label, r.w - x), p.color(style));
      a.hits.push({ rect: { x, y: r.y, w: width(label), h: 1 }, act });
      x += width(label) + 2;
    }
    const wctx = a.win.context(a.ws);
    for (const item of a.ws.menuEntries("status", a.win.invocationContext(a.ws, {}))) {
      let label = item.label ?? "";
      let style = "muted";
      if (typeof item.view === "string") { const live = a.ws.live.get(item.view); label = live?.label ?? ""; style = live?.style ?? style; if (!label) continue; }
      if (typeof item.label_context === "string") label = String(wctx[item.label_context] ?? label);
      const glyph = iconText(a, item.icon);
      const shown = item.icon_after && glyph ? `${label}${glyph}` : `${glyph}${glyph && label ? " " : ""}${label}`;
      if (x + width(shown) >= r.w) break;
      text(buf, x, r.y, shown, p.color(style));
      a.hits.push({ rect: { x, y: r.y, w: width(shown), h: 1 }, act: () => a.activate(item, a.win.invocationContext(a.ws, {})) });
      x += width(shown) + 2;
    }
    const toast = a.toast && a.toast.until > Date.now() ? a.toast : null;
    const msg = toast ? toast.text : a.message;
    if (msg && x < r.w) text(buf, x, r.y, fit(clean(msg), r.w - x), p.color(toast?.style ?? "muted"));
  }

  overlays(buf: OptimizedBuffer, W: number, H: number) {
    const a = this.app;
    const stack = [a.under, a.overlay].filter(Boolean) as Overlay[];
    for (const o of stack) {
      if (o.kind === "input" && o.multi) {  // a multi-line prompt: a box above the status bar
        const [lines, [cl, cc]] = o.multi.lines();
        const w = Math.max(10, Math.min(100, W - 4)), rowsN = Math.max(3, Math.min(10, lines.length));
        const r = { x: Math.floor((W - w) / 2), y: Math.max(0, H - rowsN - 3), w, h: rowsN + 2 };
        box(buf, r, a.pal.color("prompt"), a.pal.bg);
        text(buf, r.x + 1, r.y, fit(` ${clean(o.prompt)} `, w - 2), a.pal.color("prompt"));
        text(buf, r.x + 1, r.y + r.h - 1, fit(" Enter send · Alt-Enter new line · Escape cancel ", w - 2), a.pal.color("dim"));
        const first = Math.max(0, cl - (rowsN - 1));
        lines.slice(first, first + rowsN).forEach((l, j) => text(buf, r.x + 1, r.y + 1 + j, fit(l, w - 2), a.pal.color("text")));
        const before = [...(lines[cl] ?? "")].slice(0, cc).join("");
        text(buf, r.x + 1 + Math.min(width(before), w - 3), r.y + 1 + cl - first, [...(lines[cl] ?? "")][cc] ?? " ", a.pal.color("text"), undefined, TextAttributes.INVERSE);
        continue;
      }
      if (o.kind === "popup") {
        const value = a.panelValue(o.view);
        const w = Math.min(Math.max(30, o.width), W - 4);
        // A list_detail page wants room for both panes (as the TUI sizes it), whatever the filter leaves.
        const st = a.state("@popup");
        const n = listDetailOn(value, st.page) ? 28 + (value.pages ? 2 : 0) + 2 : rowsOf(value, st).length + (value.pages ? 2 : 0) + 3 + 3;
        const h = Math.min(Math.max(6, n), H - 3);
        const r = { x: Math.floor((W - w) / 2), y: Math.max(1, Math.floor((H - 1 - h) / 2)), w, h };
        box(buf, r, a.pal.color("border_focus"), a.pal.bg);
        text(buf, r.x + 1, r.y, fit(` ${clean(value.title ?? a.viewTitle(o.view))} `, w - 2), a.pal.color("border_focus"));
        a.hits.push({ rect: r, act: (e) => this.click("@popup", o.view, e, { ...o.ctx, panel: "@popup", panel_view: o.view }) });
        drawPanel(a, buf, inset(r), "@popup", o.view, a.overlay === o);
        continue;
      }
      let title = "", items: string[] = [], sel = 0, acts: (() => void)[] = [];
      if (o.kind === "prefix") {
        const rows = m.prefixRows(a.ws, a.ctxLeaf(a.focus));
        const kw = Math.max(2, ...rows.map((r) => width(r.key ?? "")));
        title = ` vqro (${a.prefix()} then a key, or click) `;
        items = rows.map((r) => ` ${(r.key ?? "·").padStart(kw)}  ${r.label}`);
        acts = rows.map((r) => () => { a.overlay = null; a.activate(r.binding, a.ctxLeaf(a.focus)); });
        sel = o.sel;
      } else if (o.kind === "palette") {
        const names = a.paletteItems(o.query);
        title = ` commands: ${o.query}▏ `;
        items = names.map((n) => ` ${n}  ${a.ws.commands.get(n)?.title ?? ""}`);
        acts = names.map((n) => () => { a.overlay = null; void a.run(n, {}, a.ctxLeaf(a.focus)); });
        sel = o.sel;
      } else if (o.kind === "menu") {
        title = ` ${clean(o.title)} `;
        items = o.items.map((it) => ` ${iconText(a, it.icon)}${iconText(a, it.icon) ? " " : ""}${clean(it.label ?? it.text ?? "")}`);
        acts = o.items.map((it) => () => { a.closeOverlay(); a.activate({ ...it, options: undefined }, o.ctx); });
        sel = o.sel;
      } else if (o.kind === "pick") {
        const res = [...a.ws.resources.values()];
        title = " pick a resource ";
        items = res.map((r) => ` ${r.id}  ${r.name ?? m.commandLabel(r)}  ${r.state ?? ""}`);
        acts = res.map((r) => () => { a.closeOverlay(); void a.run(o.item.command, { ...(o.item.args ?? {}), picked: r.id }, o.ctx); });
        sel = o.sel;
      } else continue;
      const w = Math.min(W - 4, Math.max(width(title) + 2, ...items.map((s) => width(s) + 2), 24));
      const h = Math.min(H - 3, items.length + 2);
      const r = { x: Math.floor((W - w) / 2), y: Math.max(1, Math.floor((H - 1 - h) / 2)), w, h };
      box(buf, r, a.pal.color("border_focus"), a.pal.bg);
      text(buf, r.x + 1, r.y, fit(title, w - 2), a.pal.color("prompt"));
      const top = Math.max(0, sel - (h - 3));
      items.slice(top, top + h - 2).forEach((s, j) => {
        const i = top + j, y = r.y + 1 + j;
        const on = i === sel;
        const [fg, bg] = on ? [a.pal.maybe("selection_text") ?? a.pal.bg, a.pal.maybe("selection") ?? a.pal.color("accent")] : [a.pal.color(o.kind === "pick" && s.includes("  unknown command  ") ? "dim" : "text"), a.pal.bg];
        buf.fillRect(r.x + 1, y, w - 2, 1, bg);
        text(buf, r.x + 1, y, fit(s, w - 2), fg, bg);
        a.hits.push({ rect: { x: r.x + 1, y, w: w - 2, h: 1 }, act: acts[i] });
      });
    }
  }
}

function iconText(a: App, icon: Json): string {
  if (typeof icon === "string") return icon;
  if (!isObj(icon)) return "";
  if (!a.ws.safeMode) {
    const glyphs = a.ws.entries.get("doc/settings/icons")?.glyphs ?? {};
    for (const n of Array.isArray(icon.name) ? icon.name : [icon.name]) {
      const g = typeof n === "string" ? glyphs[n] : null;
      if (typeof g === "string" && g && width(g) <= 2 && !/[\x00-\x1f]/.test(g)) return g;
    }
  }
  return typeof icon.text === "string" ? icon.text : "";
}

export function text(buf: OptimizedBuffer, x: number, y: number, s: string, fg: RGBA, bg?: RGBA, attr = TextAttributes.NONE) {
  if (s) buf.drawText(s, x, y, fg, bg, attr);
}

function inset(r: Rect): Rect { return { x: r.x + 1, y: r.y + 1, w: Math.max(0, r.w - 2), h: Math.max(0, r.h - 2) }; }

function box(buf: OptimizedBuffer, r: Rect, fg: RGBA, bg: RGBA) {
  if (r.w < 2 || r.h < 2) return;
  buf.fillRect(r.x, r.y, r.w, r.h, bg);
  const hz = "─".repeat(r.w - 2);
  text(buf, r.x, r.y, "┌" + hz + "┐", fg, bg);
  text(buf, r.x, r.y + r.h - 1, "└" + hz + "┘", fg, bg);
  for (let y = r.y + 1; y < r.y + r.h - 1; y++) { text(buf, r.x, y, "│", fg, bg); text(buf, r.x + r.w - 1, y, "│", fg, bg); }
}

/** A panel: page bar, pinned toolbar, rows with selection and scrolling, descriptions, badges, chips, toggles. */
function drawPanel(a: App, buf: OptimizedBuffer, area: Rect, key: string, view: string, focused: boolean) {
  drawValue(a, buf, area, key, a.panelValue(view), focused);
}

/** A pane's rows with its own page, filter, expansion, facets and sort. */
const rowsOf = (value: Json, st: PState) => m.rows(value, st.page, st.filter, st.open, FEATURES, st.facets, st.sort);

const listDetailOn = (value: Json, page: number): boolean => {
  const pg = Array.isArray(value.pages) && value.pages.length ? value.pages[Math.min(page, value.pages.length - 1)] : value;
  const cfg = pg?.list_detail ?? value.list_detail;
  return cfg === true || (isObj(cfg) && m.missingFor(cfg, FEATURES).length === 0);
};

/** A panel value: missing notice, page bar, filter line, then the list, or list and detail (list_detail). */
function drawValue(a: App, buf: OptimizedBuffer, area: Rect, key: string, value: Json, focused: boolean) {
  const p = a.pal;
  const missing = m.missingFor(value, FEATURES);
  if (missing.length) {
    text(buf, area.x, area.y, fit("This view needs a newer vqro UI", area.w), p.color("warn"));
    text(buf, area.x, area.y + 1, fit(`(missing: ${missing.join(", ")})`, area.w), p.color("dim"));
    return;
  }
  const st = a.state(key);
  let y = area.y;
  if (Array.isArray(value.pages) && value.pages.length) {
    // Keep showing the same page when pages are added or removed before it (another extension's page appears).
    const titles: string[] = value.pages.map((pg: Json) => String(pg?.title ?? "page"));
    if (titles.join("\u0000") !== st.seenPages.join("\u0000")) {
      const at = titles.indexOf(st.seenPages[st.page] ?? "\u0000");
      if (at >= 0) st.page = at;
      st.seenPages = titles;
    }
    st.page = Math.min(st.page, value.pages.length - 1);
    let x = area.x;
    value.pages.forEach((pg: Json, i: number) => {
      const label = ` ${clean(pg?.title ?? "page")} `;
      text(buf, x, y, fit(label, area.x + area.w - x), i === st.page ? p.color("accent") : p.color("muted"), undefined, i === st.page ? TextAttributes.BOLD | TextAttributes.UNDERLINE : 0);
      const page = i;
      a.hits.push({ rect: { x, y, w: width(label), h: 1 }, act: () => { st.page = page; st.sel = 0; st.top = 0; } });
      x += width(label) + 1;
    });
    y += 2;
  }
  if (st.filter) { text(buf, area.x, y, fit(`/${st.filter}`, area.w), p.color("prompt")); y += 1; }
  const body: Rect = { x: area.x, y, w: area.w, h: area.y + area.h - y };
  if (body.h <= 0) return;
  if (!listDetailOn(value, st.page)) { drawList(a, buf, body, key, value, focused, false); a.listed.delete(`${key}#detail`); return; }
  const wide = body.w >= 96;
  const rows = rowsOf(value, st);
  // The list's chrome above its rows: the facet/sort line(s), a column header and the rule below them.
  const facetH = facetLine(a, null, { ...body, h: 3 }, body.y, value, st);
  const headerH = (m.declared(value, st.page, "columns") ?? []).some((c: Json) => typeof c?.label === "string") ? 1 : 0;
  const chrome = facetH + headerH + (facetH + headerH > 0 ? 1 : 0);
  const listH = Math.max(1, Math.min(Math.floor(body.h / 2), rows.reduce((h, r) => h + rowHeight(r, body.w), 1) + chrome));
  const lw = Math.max(36, Math.floor((body.w * 2) / 5));
  const list: Rect = wide ? { ...body, w: lw } : { ...body, h: listH };
  const detail: Rect = wide ? { x: body.x + lw + 1, y: body.y, w: body.w - lw - 1, h: body.h } : { x: body.x, y: body.y + listH + 1, w: body.w, h: body.h - listH - 1 };
  drawList(a, buf, list, key, value, focused && !st.detailFocus, true);
  if (wide) for (let yy = body.y; yy < body.y + body.h; yy++) text(buf, detail.x - 1, yy, "│", p.color("border"));
  else if (detail.h > 0) text(buf, body.x, detail.y - 1, "─".repeat(body.w), p.color("border"));
  if (detail.h < 2) return;
  const selected = a.selectedDetail(key, value);
  if (!selected) { st.detailFocus = false; a.listed.delete(`${key}#detail`); text(buf, detail.x, detail.y, "Select an item for details", p.color("dim")); return; }
  text(buf, detail.x, detail.y, fit(clean(selected.value.title ?? "Details"), detail.w), p.color("heading"), undefined, TextAttributes.BOLD);
  const dbody = { x: detail.x, y: detail.y + 1, w: detail.w, h: detail.h - 1 };
  drawValue(a, buf, dbody, `${key}#detail`, selected.value, focused && st.detailFocus);
  const dl = a.listed.get(`${key}#detail`);
  if (dl) dl.area = dbody;
}

/** The scrolling list of a panel: pinned toolbar, rows, selection, scrollbar and (list_detail) a count footer. */
function drawList(a: App, buf: OptimizedBuffer, area: Rect, key: string, value: Json, focused: boolean, indicator: boolean) {
  const p = a.pal;
  const st = a.state(key);
  let y = area.y;
  let bottom = area.y + area.h;
  if (indicator && area.h > 1) {
    const rowsAll = rowsOf(value, st);
    const n = rowsAll.filter((r) => m.selectable(r.item) && !r.pinned).length;
    const total = st.filter || Object.keys(st.facets).length ? m.rows(value, st.page, "", st.open, FEATURES).filter((r) => m.selectable(r.item) && !r.pinned).length : n;
    bottom -= 1;
    const extra = (m.declared(value, st.page, "facets")?.length ? " · f show" : "") + (m.declared(value, st.page, "sorts")?.length ? " · s sort" : "");
    text(buf, area.x, bottom, fit(`${n} of ${total} · PgUp/PgDn${extra} · Tab: details`, area.w), p.color("dim"));
  }
  const rows = rowsOf(value, st);
  const listed = { rows, hits: [] as { y: number; x: number; w: number; i: number; chip?: number }[], area };
  a.listed.set(key, listed);
  if (!rows.length) {
    // The facet line stays, so a choice that leaves nothing (a facet with 0 items) can be undone with the mouse.
    y += y < bottom ? facetLine(a, buf, area, y, value, st) : 0;
    const msg = st.filter ? "(no matches)" : Object.keys(st.facets).length ? "Nothing matches this choice: pick All above" : "(empty)";
    text(buf, area.x, y, fit(msg, area.w), p.color("dim"));
    return;
  }
  // follow: stay at the end while the reader is there (core-protocol.md, Optional list with detail).
  // The end is the last selectable list row, not a toolbar or bottom-pinned row (a composer stays selected).
  let following = false;
  if (value.follow === true && !st.manual) {
    let end = -1;
    for (let i = rows.length - 1; i >= 0; i--) if (m.selectable(rows[i].item) && !rows[i].pinned && !rows[i].bottom) { end = i; break; }
    if (st.editing) { const at = rows.findIndex((r) => r.id === st.editing); if (at >= 0) st.sel = at; }  // typing: stay on the composer
    const inList = !!rows[st.sel] && !rows[st.sel].pinned && !rows[st.sel].bottom;
    if (end >= 0) {
      // First shown (and not typing): start at the end. Later: keep the end while the reader is there.
      const first = st.followEnd === null && !st.editing;
      if (first || (inList && st.followEnd !== null && st.sel >= st.followEnd)) st.sel = end;
      st.followEnd = end;
      following = !first && !inList;
    }
  }
  st.sel = Math.min(st.sel, rows.length - 1);
  if (!m.selectable(rows[st.sel].item)) { const j = rows.findIndex((r, i) => i >= st.sel && m.selectable(r.item)); st.sel = j >= 0 ? j : rows.findIndex((r) => m.selectable(r.item)); if (st.sel < 0) st.sel = 0; }
  const pinned = rows.filter((r) => r.pinned).length;
  const line = (i: number, yy: number, limit: number) => {
    const r = rows[i];
    const comp: [m.Composer, boolean] | null = m.kind(r.item) === "composer" ? [a.composerOf(st, r), focused && st.editing === r.id] : null;
    return drawRow(a, buf, { x: area.x, y: yy, w: area.w, h: limit - yy }, r, focused && i === st.sel, st.chip, (x, w, chip, hy) => listed.hits.push({ y: hy ?? yy, x, w, i, chip }), comp);
  };
  for (let i = 0; i < pinned && y < bottom; i++) y += line(i, y, bottom);
  const lineH = y < bottom ? facetLine(a, buf, area, y, value, st) : 0;
  y += lineH;
  const spec = m.declared(value, st.page, "columns");
  let header = false;
  if (spec?.length) {
    // One layout for every row: cells start after the widest prefix and leave room for badges and detail.
    const data = rows.filter((r) => Array.isArray(r.item.columns));
    const prefix = (r: m.Row) => r.depth * 2 + 2 + (m.kind(r.item) === "toggle" ? 4 : 0) + (r.item.icon ? 3 : 0);
    // Room for the detail and the first badge (further, rarer badges take room from that row's own cells).
    const suffix = (r: m.Row) => (typeof r.item.detail === "string" ? width(r.item.detail) + 1 : 0) + (Array.isArray(r.item.badges) && r.item.badges.length ? width(String(r.item.badges[0]?.text ?? "")) + 3 : 0) + 1;
    const start = Math.max(1, ...data.map(prefix)), reserve = Math.max(1, ...data.map(suffix));
    const widths = m.columnWidths(spec, data, Math.max(1, area.w - 1 - start - reserve));
    const aligns = spec.map((c: Json) => c?.align === "right");
    for (const r of data) r.item = { ...r.item, text: " ".repeat(Math.max(0, start - prefix(r))) + columnText(r.item.columns, widths, aligns) };
    if (spec.some((c: Json) => typeof c?.label === "string") && y < bottom) {
      text(buf, area.x + start, y, fit(columnText(spec.map((c: Json) => c?.label ?? ""), widths, aligns), Math.max(0, area.w - start)), p.color("muted"), undefined, TextAttributes.BOLD);
      y += 1; header = true;
    }
  }
  if ((pinned || lineH || header) && y < bottom) { text(buf, area.x, y, "─".repeat(area.w), p.color("border")); y += 1; }
  const heights = rows.map((r) => rowHeight(r, area.w));
  // pin: "bottom" rows (feature pin): below the list, never scrolled, at most half of what is left.
  let end = rows.findIndex((r) => r.bottom);
  if (end < 0) end = rows.length;
  if (end < rows.length && bottom - y > 2) {
    const wanted = heights.slice(end).reduce((n, h) => n + h, 1);
    const by = bottom - Math.min(wanted, Math.max(2, Math.floor((bottom - y) / 2)));
    text(buf, area.x, by, "─".repeat(area.w), p.color("border"));
    for (let i = end, yy = by + 1; i < rows.length && yy < bottom; i++) yy += line(i, yy, bottom);
    bottom = by;
  }
  const visible = bottom - y;
  if (following) {  // selection on a pinned row: keep the list at its end
    let top = end - pinned, used = 0;
    while (top > 0 && used + heights[pinned + top - 1] <= visible) { top--; used += heights[pinned + top]; }
    st.top = top;
  } else if (!st.manual && st.sel < end) {
    if (st.sel >= pinned && st.sel < st.top + pinned) st.top = st.sel - pinned;
    let used = 0;
    for (let i = st.top + pinned; i <= st.sel && i < end; i++) used += heights[i];
    while (st.sel >= pinned && used > visible && st.top + pinned < st.sel) { used -= heights[st.top + pinned]; st.top++; }
  }
  st.top = Math.max(0, Math.min(st.top, end - pinned - 1));
  for (let i = pinned + st.top; i < end && y < bottom; i++) y += line(i, y, bottom);
  if (end - pinned > visible) {
    const n = end - pinned;
    const thumb = Math.floor((st.top / Math.max(1, n)) * visible);
    for (let yy = 0; yy < visible; yy++) text(buf, area.x + area.w - 1, bottom - visible + yy, yy === thumb ? "┃" : "│", yy === thumb ? p.color("accent") : p.color("border"));
  }
}

/** `client.panel {action: "facet"}`: choose a declared facet's value (null: All). */
function chooseFacet(value: Json, st: PState, key: string, v: string | null) {
  if (!(m.declared(value, st.page, "facets") ?? []).some((f: Json) => f?.key === key)) return;
  if (v === null) delete st.facets[key]; else st.facets[key] = v;
  st.sel = 0; st.top = 0;
}

/** `f`: the first facet cycles All -> each value -> All. */
function cycleFacet(value: Json, st: PState) {
  const first = m.facetCounts(value, st.page, st.filter, st.facets, FEATURES)[0];
  if (!first) return;
  const at = first.chosen === null ? -1 : first.values.findIndex((v) => v.value === first.chosen);
  chooseFacet(value, st, first.key, first.values[at + 1]?.value ?? null);
}

/** `s`: given order -> each sort -> given order. */
function cycleSort(value: Json, st: PState): number | null {
  const n = m.declared(value, st.page, "sorts")?.length ?? 0;
  return st.sort === null ? (n > 0 ? 0 : null) : st.sort + 1 < n ? st.sort + 1 : null;
}

/** The facet/sort line (features facets, sort): `Show: All n · value n …  Sort: label ▾`, clickable, wrapping onto
 *  at most three lines. Returns its height. */
function facetLine(a: App, buf: OptimizedBuffer | null, area: Rect, y0: number, value: Json, st: PState): number {
  const p = a.pal;
  const counts = m.facetCounts(value, st.page, st.filter, st.facets, FEATURES);
  const label = m.sortLabel(value, st.page, st.sort);
  if (!counts.length && label === null) return 0;
  let x = area.x, y = y0;
  const limit = Math.min(y0 + 3, area.y + area.h);
  const put = (s: string, fg: RGBA, bg?: RGBA, act?: () => void) => {
    const w = width(s);
    if (x > area.x && x + w > area.x + area.w) { x = area.x; y += 1; }
    if (y >= limit) return false;
    if (buf) {  // null: measure only
      text(buf, x, y, fit(s, area.x + area.w - x), fg, bg);
      if (act) a.hits.push({ rect: { x, y, w, h: 1 }, act });
    }
    x += w;
    return true;
  };
  for (const c of counts) {
    put(`${clean(c.label)}: `, p.color("muted"));
    const chips: [string | null, string, number][] = [[null, "All", c.all], ...c.values.map((v): [string | null, string, number] => [v.value, v.label, v.count])];
    chips.forEach(([v, l, n], i) => {
      if (i > 0) put("·", p.color("dim"));
      const on = v === c.chosen;
      put(` ${clean(l)} ${n} `, on ? p.color("badge_text") : n === 0 ? p.color("dim") : p.color("text"), on ? p.color("accent") : undefined, () => chooseFacet(value, st, c.key, v));
    });
    put("  ", p.color("text"));
  }
  if (label !== null) { put("Sort: ", p.color("muted")); put(` ${clean(label)} ▾ `, p.color("control"), p.color("surface"), () => { st.sort = cycleSort(value, st); }); }
  return Math.min(y, limit - 1) - y0 + 1;
}

/** Columns (feature columns): the cells padded to one layout for the pane, starting after the widest row prefix. */
function columnText(cells: Json[], widths: number[], aligns: boolean[]): string {
  // Zero-width columns were dropped for width (min_width/priority).
  return widths.flatMap((w, i) => {
    if (w === 0) return [];
    const c = fit(clean(cells[i] ?? ""), w);
    const pad = " ".repeat(Math.max(0, w - width(c)));
    return [aligns[i] ? pad + c : c + pad];
  }).join(" ").trimEnd();
}

/** A message block's body lines (markdown flattened, else wrapped), inside its "┃ " margin. This client shows
 *  them whole: it doesn't claim feature `fold`. */
const messageBody = (it: Json, w: number): string[] => {
  const body = clean2(it.body ?? it.text);
  return it.format === "markdown" ? m.markdownLines(body, Math.max(1, w)).map((l) => l.map(([t]) => t).join("")) : wrapText(body, Math.max(1, w));
};

/** A message block's lines at width `w` (after indent): a card's padding lines (feature card), then the frame's top
 *  rule with the title (or, on an unframed card, the plain title), the body (inside "┃ " when framed), the closing rule. */
const messageLines = (it: Json, w: number): string[] => {
  const [card, framed] = m.messageLook(it, FEATURES);
  const inner = w - (card ? 2 : 0);
  const title = clean(it.title ?? it.text ?? "");
  const body = messageBody(it, inner - (framed ? 2 : 0));
  const lines = framed ? [`┏━ ${title} `, ...body.map((l) => `┃ ${l}`), "┗"] : [title, ...body];
  return card ? ["", ...lines, ""] : lines;
};

const rowHeight = (r: m.Row, w: number) =>
  m.kind(r.item) === "composer" ? composerHeight(r.item)
    : m.kind(r.item) === "message" ? messageLines(r.item, w - r.depth * 2).length
    : m.kind(r.item) === "text" && r.item.format === "markdown" ? Math.max(1, m.markdownLines(clean2(r.item.text), Math.max(1, w - r.depth * 2)).length)
    : m.kind(r.item) === "text" && r.item.wrap === true ? wrapText(clean2(r.item.text), Math.max(1, w - r.depth * 2)).length
    : m.kind(r.item) === "separator" ? 1 : 1 + (typeof r.item.description === "string" && r.item.description ? 1 : 0);

const clean2 = (s: any) => String(s ?? "").replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");

/** Word-wrap a paragraph (feature `wrap`); newlines start new lines, long words break. */
export function wrapText(text: string, w: number): string[] {
  const out: string[] = [];
  for (const para of text.split("\n")) {
    let line = "";
    for (const word of para.split(/\s+/).filter(Boolean)) {
      let piece = word;
      while (width(piece) > w) { if (line) { out.push(line); line = ""; } out.push(fit(piece, w + 1).replace(/…$/, "").slice(0, w)); piece = piece.slice(w); }
      if (!line) line = piece; else if (width(line) + 1 + width(piece) <= w) line += " " + piece; else { out.push(line); line = piece; }
    }
    out.push(line);
  }
  return out.length ? out : [""];
}

/** A composer's control-line buttons: each control's choices (a select is one button), then Send. */
function composerButtons(row: Json): { arg: string; value?: Json; send?: boolean; label: string; on: (v: Record<string, Json>) => boolean }[] {
  const out: { arg: string; value?: Json; send?: boolean; label: string; on: (v: Record<string, Json>) => boolean }[] = [];
  for (const c of m.composerControls(row)) {
    if (c.type === "chips") for (const ch of c.chips ?? []) out.push({ arg: c.arg, value: ch.value, label: String(ch.text ?? ch.value), on: (v) => v[c.arg] === ch.value });
    else out.push({ arg: c.arg, label: String(c.text ?? c.arg), on: () => false });
  }
  if (row.send_button !== false) out.push({ arg: "", send: true, label: "[Send]", on: () => false });  // send_button:false hides it
  return out;
}

const composerHeight = (it: Json) => Math.min(12, Math.max(1, Number.isInteger(it.rows) ? it.rows : 3)) + 1;

/** A composer: the draft (or placeholder) with a ▌ margin and the cursor, then controls, Send and a key hint. */
function drawComposer(a: App, buf: OptimizedBuffer, r: Rect, it: Json, selected: boolean, chipSel: number, comp: [m.Composer, boolean] | null, hit: (x: number, w: number, chip?: number, y?: number) => void): number {
  const p = a.pal;
  const [c, editing] = comp ?? [new m.Composer(it), false];
  const rowsN = Math.max(1, Math.min(r.h, composerHeight(it)) - 1);
  const [lines, [cl, cc]] = c.editor.lines();
  const first = Math.max(0, cl - (rowsN - 1));
  const empty = c.editor.text.length === 0;
  for (let dy = 0; dy < rowsN; dy++) {
    const y = r.y + dy;
    text(buf, r.x, y, "▌", editing ? p.color("prompt") : selected ? p.color("border_focus") : p.color("border"));
    if (empty && dy === 0) text(buf, r.x + 1, y, fit(clean(it.placeholder ?? "Message"), r.w - 2), p.color("dim"));
    else if (lines[first + dy] !== undefined) text(buf, r.x + 1, y, fit(lines[first + dy], r.w - 2), p.color("text"));
    if (editing && first + dy === cl) {
      const before = [...(lines[cl] ?? "")].slice(0, cc).join("");
      // On an empty draft the cursor sits on the placeholder's first letter, which stays readable.
      const under = empty ? [...String(it.placeholder ?? "Message")][0] ?? " " : [...(lines[cl] ?? "")][cc] ?? " ";
      text(buf, r.x + 1 + Math.min(width(before), r.w - 2), y, under, empty ? p.color("dim") : p.color("text"), undefined, TextAttributes.INVERSE);
    }
    hit(r.x, r.w, undefined, y);
  }
  if (rowsN >= r.h) return rowsN;
  const y = r.y + rowsN;
  let x = r.x + 1;
  composerButtons(it).forEach((b, j) => {
    const sel = c.values[b.arg];
    const ctl = m.composerControls(it).find((x) => x.arg === b.arg);
    const label = b.send ? b.label : b.value !== undefined ? `[${b.on(c.values) ? "● " : ""}${b.label}]`
      : `${b.label}: ${ctl?.options?.find((o: Json) => o.value === sel)?.label ?? sel ?? ""} ▾`;
    if (x + width(label) > r.x + r.w) return;
    const on = selected && !editing && chipSel === j + 1;
    text(buf, x, y, label, b.on(c.values) || b.send ? p.color("control") : p.color("muted"), undefined, (on ? TextAttributes.INVERSE : 0) | (b.send ? TextAttributes.BOLD : 0));
    hit(x, width(label), j + 1, y);
    x += width(label) + 1;
  });
  const n = lines.length;
  const hint = `${n} line${n === 1 ? "" : "s"} · ${it.submit === "C-Enter" ? "Ctrl-Enter send · Enter new line" : "Enter send · Alt-Enter new line"}`;
  if (x + width(hint) < r.x + r.w) text(buf, r.x + r.w - width(hint), y, hint, p.color("dim"));
  return rowsN + 1;
}

/** `hit(x, w, chip?, y?)` records a click target; `y` defaults to the row's first line (multi-line rows pass it). */
function drawRow(a: App, buf: OptimizedBuffer, r: Rect, row: m.Row, selected: boolean, chipSel: number, hit: (x: number, w: number, chip?: number, y?: number) => void, comp: [m.Composer, boolean] | null = null): number {
  const p = a.pal;
  const it = row.item;
  const k = m.kind(it);
  const indent = row.depth * 2;
  let x = r.x + indent;
  const end = r.x + r.w;
  if (k === "composer") return drawComposer(a, buf, r, it, selected, chipSel, comp, hit);
  if (k === "separator") { text(buf, r.x, r.y, "─".repeat(r.w), p.color("border")); return 1; }
  if (k === "message") {
    // A message from someone else (feature message): a heavy frame with its title, then the body. On a card
    // (feature card) the whole width takes the palette's `card`, one column stays clear each side, and the frame
    // is drawn only with `frame: true`.
    const [card, framed] = m.messageLook(it, FEATURES);
    const frame = p.color(it.style ?? "heading");
    const lines = messageLines(it, r.w - indent);
    const [first, last] = card ? [1, lines.length - 2] : [0, lines.length - 1];
    const bg = card ? p.background("card") : undefined;
    const w = r.w - indent - (card ? 2 : 0);
    const x0 = x + (card ? 1 : 0);
    lines.slice(0, r.h).forEach((l, j) => {
      if (bg) buf.fillRect(r.x, r.y + j, r.w, 1, bg);
      if (j < first || j > last) return;  // a card's padding line
      const rule = framed && (j === first || j === last);
      const fill = rule ? "━".repeat(Math.max(0, w - width(l))) : "";
      const attr = j === first ? TextAttributes.BOLD | (selected ? TextAttributes.INVERSE : 0) : 0;
      const fg = rule ? frame : j === first ? (it.style ? p.color(it.style) : p.color("text")) : p.color("text");
      text(buf, x0, r.y + j, fit(l + fill, w), fg, bg, attr);
      if (framed && j > first && j < last) text(buf, x0, r.y + j, "┃", frame, bg);
    });
    hit(r.x, r.w);
    return Math.min(lines.length, Math.max(1, r.h));
  }
  if (k === "text" && it.format === "markdown") {
    // Formatted text (feature markdown): the shared layout, styled through the palette.
    const lines = m.markdownLines(clean2(it.text), Math.max(1, r.w - indent));
    const [base] = p.style(it.style ?? null);
    const inverse = selected && !p.maybe("selection");
    lines.slice(0, r.h).forEach((l, j) => {
      if (selected) buf.fillRect(r.x, r.y + j, r.w, 1, p.maybe("selection") ?? p.bg);
      let xx = x;
      for (const [t, st] of l) {
        const fg = st === "h" ? p.color("heading") : st === "c" || st === "pre" ? p.color("muted") : selected && !inverse ? p.maybe("selection_text") ?? base : base;
        const attr = (st === "h" || st === "b" ? TextAttributes.BOLD : 0) | (inverse ? TextAttributes.INVERSE : 0);
        if (xx >= r.x + r.w) break;
        text(buf, xx, r.y + j, fit(t, r.x + r.w - xx), fg, undefined, attr);
        xx += width(t);
      }
    });
    hit(r.x, r.w);
    return Math.min(lines.length, Math.max(1, r.h));
  }
  if (k === "text" && it.wrap === true) {
    const lines = wrapText(clean2(it.text), Math.max(1, r.w - indent));
    const [fg, attr] = p.style(it.style ?? "muted");
    lines.slice(0, r.h).forEach((l, j) => {
      if (selected) buf.fillRect(r.x, r.y + j, r.w, 1, p.maybe("selection") ?? p.bg);
      const inverse = selected && !p.maybe("selection");
      text(buf, x, r.y + j, l, selected && !inverse ? p.maybe("selection_text") ?? fg : fg, undefined, inverse ? attr | TextAttributes.INVERSE : attr);
    });
    hit(r.x, r.w);
    return Math.min(lines.length, Math.max(1, r.h));
  }
  if (k === "chips") {
    (Array.isArray(it.chips) ? it.chips : []).forEach((c: Json, j: number) => {
      const label = ` ${iconText(a, c.icon)}${iconText(a, c.icon) && c.text ? " " : ""}${clean(c.text ?? "")}${Array.isArray(c.options) ? " ▾" : ""} `;
      if (x + width(label) > end) return;
      const on = selected && j === chipSel;
      const bg = on ? p.maybe("selection") ?? p.color("accent") : c.checked ? p.color("accent") : p.color("surface");
      const fg = on ? p.maybe("selection_text") ?? p.bg : c.checked ? p.color("badge_text") : p.style(c.style)[0];
      text(buf, x, r.y, label, fg, bg);
      hit(x, width(label), j);
      x += width(label) + 1;
    });
    return 1;
  }
  let [fg, attr] = p.style(it.style ?? (it.disabled ? "dim" : k === "text" ? "muted" : null));
  if (k === "header") { fg = p.color("heading"); attr = TextAttributes.BOLD; }
  let bg: RGBA | undefined;
  if (selected) {
    bg = p.maybe("selection") ?? undefined;
    if (bg) fg = p.maybe("selection_text") ?? fg; else attr |= TextAttributes.INVERSE;
  }
  if (selected) buf.fillRect(r.x, r.y, r.w, 1, bg ?? p.bg);
  let label = "";
  if (row.has_children && k !== "header") label += row.expanded ? "▾ " : "▸ ";
  if (k === "toggle") label += it.checked ? "[x] " : "[ ] ";
  const icon = iconText(a, it.icon);
  if (icon) label += icon + " ";
  label += clean(it.text);
  if (k === "select") label += `: ${clean(it.options?.find((o: Json) => o.value === it.value)?.label ?? it.value)} ▾`;
  else if (Array.isArray(it.options)) label += " ▾";
  const badges: Json[] = Array.isArray(it.badges) ? it.badges : [];
  const detail = typeof it.detail === "string" ? clean(it.detail) : "";
  const badgeWidth = badges.reduce((n, b) => n + width(clean(b.text)) + 3, 0);
  const room = Math.max(4, end - x - badgeWidth - (detail ? width(detail) + 2 : 0));
  text(buf, x, r.y, fit(label, room), fg, bg, attr);
  hit(r.x, r.w);
  let bx = Math.min(end, x + width(fit(label, room)) + 2);
  if (detail && bx + width(detail) <= end) { text(buf, bx, r.y, detail, p.style(it.detail_style ?? "dim")[0], bg); bx += width(detail) + 2; }
  for (const b of badges) {
    const t = ` ${clean(b.text)} `;
    if (bx + width(t) > end) break;
    text(buf, bx, r.y, t, p.color("badge_text"), b.style && b.style !== "dim" ? p.color(b.style) : p.color("badge"));
    bx += width(t) + 1;
  }
  if (typeof it.description === "string" && it.description && r.h > 1) {
    text(buf, r.x + indent + 2, r.y + 1, fit(clean(it.description), r.w - indent - 2), p.color("dim"));
    return 2;
  }
  return 1;
}
