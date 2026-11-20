// The TypeScript client model against the shared base-profile fixtures (D29), the same files the Rust model
// (tests/client_conformance.rs) and the Python model (tests/test_client_conformance.py) pass. `bun test`.
import { expect, test } from "bun:test";
import { join } from "path";
import * as m from "./model";

const dir = join(import.meta.dir, "../../../../tests/fixtures/client");
const fixture = async (name: string) => JSON.parse(await Bun.file(join(dir, name)).text());
const workspace = (snap: any) => { const ws = new m.Workspace(); ws.load(snap); return ws; };

test("panels", async () => {
  const doc = await fixture("panels.json");
  expect(doc.base_features).toEqual(m.BASE_FEATURES);
  for (const c of doc.cases) {
    const s = c.state, p = c.panel;
    const got = m.rows(p, s.page ?? 0, s.filter ?? "", s.open ?? {}, c.features ?? m.BASE_FEATURES).map((r) => ({
      id: r.id, depth: r.depth, text: r.item.text ?? null, type: m.kind(r.item), has_children: r.has_children, expanded: r.expanded,
      pinned: r.pinned, selectable: m.selectable(r.item), bottom: r.bottom, chips: "chips" in r.item ? r.item.chips.map((x: any) => x.text ?? null) : null,
    }));
    const enhancement = p.list_detail && typeof p.list_detail === "object" ? m.missingFor(p.list_detail, m.BASE_FEATURES).length === 0 : null;
    expect({ rows: got, missing: m.missingFor(p, m.BASE_FEATURES), list_detail: enhancement }, c.name).toEqual(c.expect);
  }
});

test("views", async () => {
  const doc = await fixture("views.json");
  const entries = new Map(Object.entries(doc.entries)), live = new Map(Object.entries(doc.live));
  for (const c of doc.cases) {
    const [shown, chain] = m.shownView(c.view, (v) => m.merged(v, entries, live), (v) => m.renders(v, doc.kinds, m.BASE_FEATURES));
    expect({ shown, chain }, c.name).toEqual(c.expect);
  }
});

test("contributions", async () => {
  for (const c of (await fixture("contributions.json")).cases) {
    const ws = workspace(c.snapshot);
    const menus = Object.fromEntries(Object.entries(c.places).map(([place, ctx]) => [place, ws.menuEntries(place, ctx).map((e) => e.label ?? null)]));
    const cands = ws.keyCandidates(c.ctx);
    const keys = Object.fromEntries([...m.resolve(cands)].map(([k, i]) => [k, cands[i][1].command === "client.none" ? null : cands[i][0]]));
    const gestures = Object.fromEntries(c.gesture_queries.map(([key, place]: [string, string]) => [`${key} ${place}`, ws.gesture(key, place)?.command ?? null]));
    expect({ menus, keys, gestures }, c.name).toEqual(c.expect);
  }
});

test("recovery", async () => {
  for (const c of (await fixture("recovery.json")).cases) {
    const ws = workspace(c.snapshot);
    const prefix = m.prefixRows(ws, c.ctx).map((r) => ({ key: r.key, label: r.label, command: r.binding.command ?? null, args: r.binding.args ?? null }));
    const chooser = ws.safeMode ? m.safeChooser(ws, new Set(c.selected ?? [])) : null;
    expect({ prefix, safe_mode_chooser: chooser }, c.name).toEqual(c.expect);
  }
});

test("window", async () => {
  for (const c of (await fixture("window.json")).cases) {
    const ws = workspace(c.snapshot), win = new m.Window(), got: any[] = [];
    for (const step of c.steps) {
      let out: any;
      switch (step.do) {
        case "start": out = { settled: win.settle(ws) }; break;
        case "event": ws.apply(step.event); out = { settled: win.settle(ws) }; break;
        case "show": case "client.layout": {
          const [ok, r] = step.do === "show" ? win.show(ws, step.layout) : win.clientLayout(ws, step.args);
          out = ok ? { ok: r } : { error: r };
          break;
        }
        case "reply": { const hinted = m.replyLayout(step.command, step.reply); if (hinted) win.show(ws, hinted); out = { reply_layout: hinted }; break; }
        case "invoke": out = { ctx: win.invocationContext(ws, step.ctx) }; break;
        case "context": out = { context: win.context(ws) }; break;
        case "mark": out = { panel: m.markCurrent(step.panel, win.shown(ws)) }; break;
        default: throw new Error(`unknown step ${step.do}`);
      }
      out.shown = win.shown(ws);
      got.push(out);
    }
    expect(got, c.name).toEqual(c.expect);
  }
});

test("key names follow the spec", () => {
  for (const [raw, want] of [["C-space", "C-Space"], ["Ctrl-Shift-Left", "C-S-Left"], ["C-Q", "C-q"], ["S-a", null], ["Hyper-x", null],
    ["f12", "F12"], ["F13", null], ["C--", "C--"], ["M-x", "M-x"], [" ", "Space"], ["BackTab", "BackTab"]] as [string, string | null][]) {
    expect(m.normalize(raw), raw).toBe(want);
  }
});

test("embedded views are read-only, cycle-bounded and never attach terminals", () => {
  const entries = new Map<string, any>([
    ["view/root", { kind: "panel", items: [{ type: "view", view: "source", readonly: true }] }],
    ["view/source", { kind: "panel", items: [] }],
    ["view/term", { kind: "terminal", resource: "r1" }],
    ["view/odd", { kind: "x.odd", fallback: "plain" }],
    ["view/plain", { kind: "panel", items: [{ text: "via fallback" }] }]]);
  const live = new Map<string, any>([["source", { items: [
    { text: "native", command: "forbidden", args: {}, children: [{ text: "nested", command: "bad" }] },
    { type: "view", view: "root" }, { type: "view", view: "term", text: "no terminals here" }, { type: "view", view: "odd" }] }]]);
  const [panel, deps] = m.compose("root", entries, live, [...m.BASE_FEATURES, "embedded_views"]);
  expect(panel.items[0].text).toBe("native");
  expect(panel.items[0].command).toBeUndefined();
  expect(panel.items[0].children[0].command).toBeUndefined();
  expect(panel.items[1].text).toBe("Referenced panel unavailable or unsupported.");  // the cycle back to root
  expect(panel.items[2].text).toBe("no terminals here");
  expect(panel.items[3].text).toBe("via fallback");
  expect([...deps].sort()).toEqual(["odd", "plain", "root", "source", "term"]);
});

test("markdown", async () => {
  for (const c of (await fixture("markdown.json")).cases) expect(m.markdownLines(c.text, c.width), c.name).toEqual(c.expect);
});
