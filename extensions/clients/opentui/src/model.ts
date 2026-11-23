// The renderer-independent client model in TypeScript: a third implementation of the `base` client contract (D29),
// written from the spec (docs/spec/core-protocol.md, "Clients and profiles", "Fallbacks") and checked against the
// shared fixtures tests/fixtures/client/*.json (src/conformance.test.ts). Nothing here imports OpenTUI.

export type Json = any;
export const BASE_FEATURES = ["items", "pages", "tree", "toolbar", "badges", "description", "menu", "input", "options", "select", "chips", "icon", "wrap"];
export const BUILTIN = "builtin:";
export const SAFE_MODE_VIEW = "@vqro-safe-mode";
export const ROW_TYPES = ["item", "toggle", "header", "separator", "text", "chips", "select"];
export const MAX_CHAIN = 4;
const NAMED = ["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "Tab", "BackTab", "Enter", "Esc", "Backspace", "Space"];

const isObj = (v: Json): v is Record<string, Json> => v !== null && typeof v === "object" && !Array.isArray(v);
const isStr = (v: Json): v is string => typeof v === "string";
const isInt = (v: Json): v is number => typeof v === "number" && Number.isInteger(v);
const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

// ---- key names --------------------------------------------------------------------------------------------------

/** Canonical key name (`C-space` -> `C-Space`, `Ctrl-a` -> `C-a`), or null if it isn't one. */
export function normalize(name: string): string | null {
  let s = name;
  for (const [word, short] of [["ctrl-", "C-"], ["control-", "C-"], ["alt-", "M-"], ["meta-", "M-"], ["option-", "M-"], ["shift-", "S-"]]) {
    for (;;) {
      const lower = s.toLowerCase();
      let i = lower.indexOf(word);
      while (i !== -1 && !((i === 0 || lower.slice(0, i).endsWith("-")) && lower.length > i + word.length)) i = lower.indexOf(word, i + 1);
      if (i === -1) break;
      s = s.slice(0, i) + short + s.slice(i + word.length);
    }
  }
  let ctrl = false, alt = false, shift = false, rest = s;
  while ([...rest].length > 2 && rest[1] === "-" && "CMS".includes(rest[0].toUpperCase())) {
    const m = rest[0].toUpperCase();
    ctrl ||= m === "C"; alt ||= m === "M"; shift ||= m === "S";
    rest = rest.slice(2);
  }
  let base: string;
  const chars = [...rest];
  if (chars.length === 1) {
    if (rest === " ") base = "Space";
    else if (shift) return null;
    else base = ctrl ? rest.toLowerCase() : rest;
  } else {
    const lower = rest.toLowerCase();
    const f = /^f(\d+)$/.exec(lower);
    if (f && +f[1] >= 1 && +f[1] <= 12) base = "F" + +f[1];
    else {
      const named = NAMED.find((n) => n.toLowerCase() === lower);
      if (!named) return null;
      base = named;
    }
  }
  return (ctrl ? "C-" : "") + (alt ? "M-" : "") + (shift ? "S-" : "") + base;
}

// ---- panels and element fallbacks --------------------------------------------------------------------------------

export const kind = (item: Json): string => (isStr(item?.type) ? item.type : "item");
/** A `message` row's look for a client rendering `features`: [on the card background, frame drawn]. `look: "card"`
 *  (feature `card`) sets the block on the palette's `card` with a padding line above and below and drops the frame
 *  unless `frame: true`; otherwise it is the framed block. */
export function messageLook(item: Json, features: string[]): [boolean, boolean] {
  if (kind(item) !== "message") return [false, false];
  const card = item.look === "card" && features.includes("card");
  return [card, !card || item.frame === true];
}
export const selectable = (item: Json): boolean =>
  (!["header", "separator", "text"].includes(kind(item)) || (kind(item) === "text" && (item.focusable === true || fold(item) !== null))) && item.disabled !== true;

/** `fold: N` on a text row or message block (feature fold): its first N lines show until unfolded. */
export function fold(item: Json): number | null {
  if (!["text", "message"].includes(kind(item)) || !Number.isInteger(item.fold) || item.fold < 0) return null;
  return Math.min(Math.max(item.fold, 1), 10000);
}

export function foldLabel(item: Json, total: number, open: boolean): string | null {
  const n = fold(item);
  if (n === null || total <= n) return null;
  return open ? "▾ less" : `▸ ${total - n} more line${total - n === 1 ? "" : "s"}`;
}

export function missingFor(value: Json, features: string[]): string[] {
  const req = Array.isArray(value?.requires) ? value.requires : [];
  return req.filter((f: Json) => isStr(f) && !features.includes(f) && !(f.startsWith("vqro.") && features.includes(f.slice(5))));
}

function matches(item: Json, f: string): boolean {
  const hay = ["text", "detail", "description", "summary", "title", "body"].map((k) => (item[k] == null ? "" : String(item[k]))).join(" ")
    + " " + (Array.isArray(item.tags) ? item.tags.filter(isStr).join(" ") : "");
  return hay.toLowerCase().includes(f);
}

/** `requires_absent`: content standing in for an enhancement, dropped by a client with any of those features. */
const superseded = (item: Json, features: string[]) =>
  Array.isArray(item.requires_absent) && item.requires_absent.some((f: unknown) => isStr(f) && features.includes(f));

const native = (item: Json, features: string[]) =>
  (ROW_TYPES.includes(kind(item)) || features.includes(kind(item))) && missingFor(item, features).length === 0;

function chips(item: Json, features: string[]): Json {
  if (kind(item) !== "chips" || !Array.isArray(item.chips)) return item;
  const shown: Json[] = [];
  for (let chip of item.chips) {
    // Each chip reached (the declared one, then each fallback) is checked for `requires_absent` first.
    for (let n = 0; n <= 4; n++) {
      if (isObj(chip) && superseded(chip, features)) { chip = null; break; }
      if (n === 4 || !isObj(chip) || missingFor(chip, features).length === 0) break;
      if (chip.fallback === "drop") { chip = null; break; }
      if (!isObj(chip.fallback)) break;
      chip = chip.fallback;
    }
    if (chip !== null) shown.push(chip);
  }
  return { ...item, chips: shown };
}

/** Element fallback: the [row, id] pairs shown for one item. */
export function expand(item: Json, rid: string, features: string[], depth = 0): [Json, string][] {
  const own = isStr(item.id) ? item.id : rid;
  if (superseded(item, features)) return [];
  if (depth >= 4 || native(item, features) || !("fallback" in item)) return [[chips(item, features), own]];
  const fb = item.fallback;
  if (fb === "drop") return [];
  if (isObj(fb)) return expand(fb, own, features, depth + 1);
  if (Array.isArray(fb)) return fb.flatMap((row, n) => (isObj(row) ? expand(row, fb.length === 1 ? own : `${own}~${n}`, features, depth + 1) : []));
  return [[chips(item, features), own]];
}

const shownRows = (items: Json[], prefix: string, features: string[]): [Json, string][] =>
  items.flatMap((item, i) => (isObj(item) ? expand(item, `${prefix}${i}`, features) : []));

/** A row's own match: the text filter (lower case) and every chosen facet. */
const rowOk = (item: Json, f: string, chosen: [string, string][]) => (!f || matches(item, f)) && chosen.every(([k, w]) => hasFacet(item, k, w));

function subtree(item: Json, rid: string, f: string, features: string[], chosen: [string, string][] = []): boolean {
  return rowOk(item, f, chosen) || shownRows(Array.isArray(item.children) ? item.children : [], rid + "/", features).some(([c, cid]) => subtree(c, cid, f, features, chosen));
}

// ---- facets, sort, columns (features `facets`, `sort`, `columns`) -----------------------------------------------

const pageValue = (value: Json, page = 0): Json =>
  Array.isArray(value.pages) && value.pages.length && isObj(value.pages[Math.min(page, value.pages.length - 1)]) ? value.pages[Math.min(page, value.pages.length - 1)] : value;

/** Where a panel declares `facets`, `sorts` or `columns`: on the current page, else on the panel. */
export function declared(value: Json, page: number, field: string): Json[] | null {
  const own = pageValue(value, page)[field];
  if (Array.isArray(own)) return own;
  return Array.isArray(value[field]) ? value[field] : null;
}

export function hasFacet(item: Json, key: string, want: string): boolean {
  const v = isObj(item.facets) ? item.facets[key] : undefined;
  return isStr(v) ? v === want : Array.isArray(v) && v.includes(want);
}

export function chosenFacets(value: Json, page: number, facets: Record<string, string> | undefined, features: string[]): [string, string][] {
  if (!features.includes("facets") || !facets) return [];
  const keys = (declared(value, page, "facets") ?? []).filter(isObj).map((f) => f.key);
  return Object.entries(facets).filter(([k, w]) => keys.includes(k) && isStr(w)).sort(([a], [b]) => cmp(a, b));
}

export function chosenSort(value: Json, page: number, sort: number | null | undefined, features: string[]): [string, boolean] | null {
  if (!features.includes("sort") || sort == null) return null;
  const s = (declared(value, page, "sorts") ?? [])[sort];
  return isObj(s) && isStr(s.key) ? [s.key, s.desc === true] : null;
}

const sortValue = (item: Json, key: string): number | string | null => {
  const v = isObj(item.sort) ? item.sort[key] : undefined;
  return typeof v === "number" || isStr(v) ? v : null;
};

/** Numbers before strings, strings case-insensitively; `desc` reverses keyed rows only (keyless rows stay last). */
function compareRows(a: Json, b: Json, key: string, desc: boolean): number {
  const x = sortValue(a, key), y = sortValue(b, key);
  if (x === null || y === null) return x === null ? (y === null ? 0 : 1) : -1;
  let o: number;
  if (typeof x === "number" && typeof y === "number") o = x - y;
  else if (typeof x === "number") o = -1;
  else if (typeof y === "number") o = 1;
  else o = cmp(x.toLowerCase(), (y as string).toLowerCase());
  return desc ? -o : o;
}

/** One sibling list, stably sorted within each run between header and separator rows (they stay put). */
function sortedRows(pairs: [Json, string][], by: [string, boolean] | null): [Json, string][] {
  if (!by) return pairs;
  const out: [Json, string][] = [];
  let run: [Json, string][] = [];
  const flush = () => { out.push(...run.slice().sort((a, b) => compareRows(a[0], b[0], by[0], by[1]))); run = []; };
  for (const p of pairs) {
    if (["header", "separator"].includes(kind(p[0]))) { flush(); out.push(p); } else run.push(p);
  }
  flush();
  return out;
}

export interface FacetCount { key: string; label: string; chosen: string | null; all: number; values: { value: string; label: string; count: number }[] }

/** The facet line: a count is the rows that would show if that value were chosen, given the text filter and the other
 *  choices; headers, separators, toolbar and pin: "bottom" rows don't count; rows without the key only under All. */
export function facetCounts(value: Json, page = 0, filter = "", facets: Record<string, string> = {}, features = BASE_FEATURES): FacetCount[] {
  const decl = features.includes("facets") ? declared(value, page, "facets") : null;
  if (!decl) return [];
  const f = filter.toLowerCase(), chosen = chosenFacets(value, page, facets, features), items: Json[] = [];
  const walk = (list: Json[], prefix: string) => {
    for (const [item, rid] of shownRows(list, prefix, features)) {
      if (!["header", "separator"].includes(kind(item))) items.push(item);
      walk(Array.isArray(item.children) ? item.children : [], rid + "/");
    }
  };
  walk(pageItems(value, page).filter((i) => !(features.includes("pin") && isObj(i) && i.pin === "bottom")), "");
  return decl.filter((d) => isObj(d) && isStr(d.key)).map((d) => {
    const key: string = d.key;
    const pool = items.filter((i) => rowOk(i, f, chosen.filter(([k]) => k !== key)));
    let values: [string, string][];
    if (Array.isArray(d.values)) values = d.values.filter((x: Json) => isObj(x) && isStr(x.value)).map((x: Json) => [x.value, x.label ?? x.value]);
    else {
      values = [];
      for (const i of items) {
        const v = isObj(i.facets) ? i.facets[key] : undefined;
        for (const s of isStr(v) ? [v] : Array.isArray(v) ? v.filter(isStr) : []) if (!values.some(([x]) => x === s)) values.push([s, s]);
      }
    }
    const pick = chosen.find(([k]) => k === key);
    return { key, label: d.label ?? key, chosen: pick ? pick[1] : null, all: pool.length,
      values: values.map(([v, l]) => ({ value: v, label: l, count: pool.filter((i) => hasFacet(i, key, v)).length })) };
  });
}

export function sortLabel(value: Json, page = 0, sort: number | null = null): string | null {
  const sorts = declared(value, page, "sorts");
  if (!sorts || !sorts.length) return null;
  const s = sort != null ? sorts[sort] : null;
  return isObj(s) ? (s.label ?? s.key) : "given order";
}

/** Column widths in `width` cells (one-cell gaps between shown columns): numbers fixed, absent the widest cell (capped
 *  at a third), "fill" shares what is left. While a column with `min_width` would get less, the shown column with the
 *  lowest `priority` is dropped (width 0); columns without `priority` are never dropped. */
export function columnWidths(spec: Json[], shown: Row[], width: number): number[] {
  if (!spec.length) return [];
  const on = spec.map(() => true);
  for (;;) {
    const w = widthsFor(spec, shown, width, on);
    const short = spec.some((c, i) => on[i] && isInt(c?.min_width) && w[i] < c.min_width);
    let victim = -1;
    spec.forEach((c, i) => {
      if (!on[i] || typeof c?.priority !== "number") return;
      if (victim < 0 || c.priority < spec[victim].priority || (c.priority === spec[victim].priority && i > victim)) victim = i;
    });
    if (!short || victim < 0) return w;
    on[victim] = false;
  }
}

function widthsFor(spec: Json[], shown: Row[], width: number, on: boolean[]): number[] {
  // Terminal cells, not characters: wide (CJK, emoji) text takes two each.
  const cell = (r: Row, i: number) => (Array.isArray(r.item.columns) && isStr(r.item.columns[i]) ? Bun.stringWidth(r.item.columns[i]) : 0);
  const w: (number | null)[] = spec.map((c, i) => {
    if (!on[i]) return 0;
    if (isInt(c?.width)) return c.width;
    if (c?.width === "fill") return null;
    return Math.max(1, Math.min(Math.floor(width / 3), Math.max(isStr(c?.label) ? Bun.stringWidth(c.label) : 0, ...shown.map((r) => cell(r, i)))));
  });
  const fills = w.filter((x) => x === null).length;
  const left = Math.max(0, width - w.reduce<number>((s, x) => s + (x ?? 0), 0) - (on.filter(Boolean).length - 1));
  let k = 0;
  return w.map((x) => (x === null ? Math.floor(left / Math.max(fills, 1)) + (k++ < left % Math.max(fills, 1) ? 1 : 0) : x));
}

export function pageItems(value: Json, page = 0): Json[] {
  if (Array.isArray(value.pages)) {
    if (!value.pages.length) return [];
    return value.pages[Math.min(page, value.pages.length - 1)]?.items ?? [];
  }
  return Array.isArray(value.items) ? value.items : [];
}

export type ColumnSort = [number, boolean] | null;

export function nextColumnSort(by: ColumnSort, column: number): ColumnSort {
  return by?.[0] === column ? (by[1] ? null : [column, true]) : [column, false];
}

export function activeColumnSort(spec: Json[], by: ColumnSort): ColumnSort {
  return by && spec[by[0]]?.sortable === true ? by : null;
}

function columnSorted(pairs: [Json, string][], by: ColumnSort): [Json, string][] {
  if (!by) return pairs;
  const data = (item: Json) => Array.isArray(item.columns) && item.table_header !== true && typeof item.pin !== "string" && !["header", "separator"].includes(kind(item));
  const value = (item: Json) => (Array.isArray(item.column_sort) ? item.column_sort : item.columns)?.[by[0]];
  const keyed = (v: Json) => typeof v === "number" || typeof v === "string";
  const numeric = (v: Json): number | null => {
    if (typeof v === "string" && !/^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/.test(v.trim())) return null;
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  };
  const values = pairs.filter(([item]) => data(item)).sort(([a], [b]) => {
    const x = value(a), y = value(b);
    if (!keyed(x) || !keyed(y)) return Number(keyed(y)) - Number(keyed(x));
    const nx = numeric(x), ny = numeric(y);
    const order = nx !== null && ny !== null ? (nx < ny ? -1 : nx > ny ? 1 : 0)
      : nx !== null ? -1 : ny !== null ? 1 : cmp(String(x).toLowerCase(), String(y).toLowerCase());
    return by[1] ? -order : order;
  });
  let i = 0;
  return pairs.map(pair => data(pair[0]) ? values[i++] : pair);
}

export interface Row { item: Json; depth: number; id: string; parent: number | null; has_children: boolean; expanded: boolean; pinned: boolean; bottom: boolean }

/** Visible rows: toolbar, page, element fallbacks, tree expansion and filter. */
export function rows(value: Json, page = 0, filter = "", open: Record<string, boolean> = {}, features = BASE_FEATURES,
  facets: Record<string, string> = {}, sort: number | null = null, columnSort: ColumnSort = null): Row[] {
  const f = filter.toLowerCase();
  const out: Row[] = [];
  const chosen = chosenFacets(value, page, facets, features), by = chosenSort(value, page, sort, features);
  const columnBy = features.includes("column_sort") ? activeColumnSort(declared(value, page, "columns") ?? [], columnSort) : null;
  const order = (pairs: [Json, string][]) => columnSorted(sortedRows(pairs, by), columnBy);
  const one = (item: Json, rid: string, depth: number, parent: number | null, f: string, bottom: boolean) => {
    const mine = bottom ? [] : chosen; // pin: "bottom" rows are never filtered
    const narrowed = !!f || mine.length > 0;
    if (narrowed && !subtree(item, rid, f, features, mine)) return;
    const children = Array.isArray(item.children) ? item.children : [];
    const expanded = children.length > 0 && (narrowed || (rid in open ? open[rid] : item.expanded === true));
    const { children: _, ...shown } = item;
    const me = out.length;
    out.push({ item: shown, depth, id: rid, parent, has_children: children.length > 0, expanded, pinned: false, bottom });
    if (expanded) for (const [c, cid] of (bottom ? sortedRows(shownRows(children, rid + "/", features), by) : order(shownRows(children, rid + "/", features)))) one(c, cid, depth + (kind(item) !== "header" ? 1 : 0), me, f, bottom);
  };
  shownRows(Array.isArray(value.toolbar) ? value.toolbar : [], "toolbar/", features).forEach(([item], n) =>
    out.push({ item, depth: 0, id: `toolbar/${n}`, parent: null, has_children: false, expanded: false, pinned: true, bottom: false }));
  // pin: "bottom" rows (feature pin) come last and are never filtered out; without the feature they stay in place.
  const pin = features.includes("pin");
  const top = shownRows(pageItems(value, page), "", features);
  for (const [item, rid] of order(top.filter(([item]) => !(pin && item.pin === "bottom")))) one(item, rid, 0, null, f, false);
  for (const [item, rid] of top) if (pin && item.pin === "bottom") one(item, rid, 0, null, "", true);
  return out;
}

/** Search (feature search): matching selectable rows on the page, collapsed ones included, in display order, as
 *  [id, ids to open]. Toolbar and bottom-pinned rows are not searched; the filter still applies. */
export function search(value: Json, query: string, page = 0, filter = "", features = BASE_FEATURES,
  facets: Record<string, string> = {}, sort: number | null = null, columnSort: ColumnSort = null): [string, string[]][] {
  const q = query.toLowerCase(), f = filter.toLowerCase();
  const out: [string, string[]][] = [];
  if (!q) return out;
  const chosen = chosenFacets(value, page, facets, features), by = chosenSort(value, page, sort, features);
  const columnBy = features.includes("column_sort") ? activeColumnSort(declared(value, page, "columns") ?? [], columnSort) : null;
  const walk = (items: [Json, string][], path: string[]) => {
    for (const [item, rid] of columnSorted(sortedRows(items, by), columnBy)) {
      if ((f || chosen.length) && !subtree(item, rid, f, features, chosen)) continue;
      if (matches(item, q) && selectable(item)) out.push([rid, [...path]]);
      if (Array.isArray(item.children) && item.children.length) walk(shownRows(item.children, rid + "/", features), [...path, rid]);
    }
  };
  const pin = features.includes("pin");
  walk(shownRows(pageItems(value, page), "", features).filter(([i]) => !(pin && i.pin === "bottom")), []);
  return out;
}

/** The next (or previous) selectable row after `from` whose item has every field of `matching`; null if none. */
export function findRow(rows: Row[], from: number, forward: boolean, matching: Json): number | null {
  const ok = (r: Row) => selectable(r.item) && (!isObj(matching) || Object.entries(matching).every(([k, v]) => JSON.stringify(r.item[k] ?? null) === JSON.stringify(v)));
  if (forward) { for (let i = from + 1; i < rows.length; i++) if (ok(rows[i])) return i; }
  else for (let i = Math.min(from, rows.length) - 1; i >= 0; i--) if (ok(rows[i])) return i;
  return null;
}

// ---- view-kind fallback -----------------------------------------------------------------------------------------

export function merged(view: string, entries: Map<string, Json>, live: Map<string, Json>): Json | null {
  const v = entries.get(`view/${view}`);
  if (!isObj(v)) return null;
  const l = live.get(view);
  return isObj(l) ? { ...v, ...l } : { ...v };
}

export const renders = (v: Json, kinds: string[], features: string[]) => kinds.includes(v.kind) && missingFor(v, features).length === 0;

export function shownView(view: string, value: (id: string) => Json | null, canRender: (v: Json) => boolean): [string | null, string[]] {
  const chain: string[] = [];
  let current = view;
  while (chain.length <= MAX_CHAIN && !chain.includes(current)) {
    chain.push(current);
    const v = value(current);
    if (v === null) return [null, chain];
    if (canRender(v)) return [current, chain];
    if (!isStr(v.fallback)) return [null, chain];
    current = v.fallback;
  }
  return [null, chain];
}

// ---- read-only panel references (feature embedded_views) -----------------------------------------------------

const READONLY_FIELDS = ["type", "text", "style", "description", "detail", "detail_style", "badges", "id", "expanded", "wrap", "focusable"];

function readonlyRow(row: Json): Json {
  if (!isObj(row)) return { type: "text", text: isStr(row) ? row : JSON.stringify(row) };
  const out: Json = {};
  for (const k of READONLY_FIELDS) if (k in row) out[k] = row[k];
  for (const k of ["text", "description", "detail"]) {
    if (isStr(out[k])) out[k] = [...out[k]].slice(0, 8192).map((c) => (/[\x00-\x08\x0b-\x1f\x7f]/.test(c) ? JSON.stringify(c).slice(1, -1) : c)).join("");
  }
  if (Array.isArray(row.badges)) out.badges = row.badges.filter(isObj).map((b: Json) => ({ text: b.text, style: b.style }));
  if (Array.isArray(row.children)) out.children = row.children.map(readonlyRow);
  return out;
}

/** A panel view with its `{type: "view"}` references replaced by the source panels' rows, read-only: commands,
 *  menus and inputs are not imported; cycles, terminals, missing or unsupported sources and depth beyond eight
 *  show the reference's `text`; at most 2048 rows. Sources that can't be rendered use their view-kind fallback.
 *  Returns the value and every view it depends on (core-protocol.md, Live view content). */
export function compose(view: string, entries: Map<string, Json>, live: Map<string, Json>, features: string[]): [Json, Set<string>] {
  const value = (v: string): Json => {
    const l = live.get(v);
    const base = entries.get(`view/${v}`) ?? (isObj(l) && Array.isArray(l.items) ? { kind: "panel" } : {});
    return isObj(l) ? { ...base, ...l } : { ...base };
  };
  const deps = new Set<string>(), seen = new Set<string>([view]);
  let remaining = 2048;
  const rowsOf = (rows: Json[], depth: number): Json[] => {
    const out: Json[] = [];
    for (const row of rows) {
      if (remaining === 0) { out.push({ type: "text", style: "warn", text: "Composed panel row limit reached." }); break; }
      remaining--;
      if (isObj(row) && row.type === "view") {
        const source = isStr(row.view) ? row.view : "";
        deps.add(source);
        let panel = value(source);
        for (let n = 0; n < MAX_CHAIN && !(panel.kind === "panel" && missingFor(panel, features).length === 0) && isStr(panel.fallback); n++) {
          deps.add(panel.fallback);
          panel = value(panel.fallback);
        }
        if (depth >= 8 || seen.has(source) || panel.kind !== "panel" || !Array.isArray(panel.items) || missingFor(panel, features).length) {
          out.push({ type: "text", style: "dim", text: isStr(row.text) ? row.text : "Referenced panel unavailable or unsupported." });
        } else {
          seen.add(source);
          out.push(...rowsOf(panel.items, depth + 1).map(readonlyRow));
          seen.delete(source);
        }
      } else if (isObj(row) && Array.isArray(row.children)) out.push({ ...row, children: rowsOf(row.children, depth + 1) });
      else out.push(row);
    }
    return out;
  };
  const result = value(view);
  if (Array.isArray(result.items)) result.items = rowsOf(result.items, 0);
  return [result, deps];
}

// ---- workspace cache and contributions --------------------------------------------------------------------------

/** A binding's scope: `prefix` (default), `pane` or `composer`. */
export const scope = (b: Json): string => (isStr(b.scope) ? b.scope : "prefix");

export const whenHolds = (entry: Json, ctx: Json) => !isObj(entry.when) || Object.entries(entry.when).every(([k, v]) => JSON.stringify(ctx?.[k] ?? null) === JSON.stringify(v));

export function rank(id: string, b: Json): [number, number, string] {
  return [-(isInt(b.priority) ? b.priority : 0), id.startsWith(BUILTIN) ? 0 : 1, id];
}
const rankLess = (a: [number, number, string], b: [number, number, string]) => a[0] - b[0] || a[1] - b[1] || cmp(a[2], b[2]);

export function resolve(cands: [string, Json][]): Map<string, number> {
  const won = new Map<string, number>();
  cands.forEach(([id, b], i) => {
    const key = isStr(b.key) ? normalize(b.key) : null;
    if (key === null) return;
    const j = won.get(key);
    if (j === undefined || rankLess(rank(id, b), rank(...cands[j])) < 0) won.set(key, i);
  });
  return won;
}

export function builtinBindings(): Json[] {
  return [
    { key: ":", command: "client.palette", label: "all commands", recovery: true },
    { key: "o", command: "client.focus", args: { dir: "next" }, label: "next pane" },
    { key: "O", command: "client.focus", args: { dir: "prev" }, label: "previous pane", hidden: true },
    { key: "u", command: "vqro.revert", label: "undo last change", recovery: true },
    { key: "R", command: "vqro.layout.reset", label: "reset layout", recovery: true },
    { key: "!", command: "client.safe_mode", label: "safe mode / recovery choices", recovery: true },
    { key: "q", command: "client.quit", label: "quit this UI", recovery: true },
    ...Array.from({ length: 9 }, (_, i) => ({ key: String(i + 1), command: "client.tab", args: { to: i + 1 }, label: `tab ${i + 1}`, hidden: true })),
  ];
}

export type Change =
  | { type: "resync" } | { type: "view"; view: string } | { type: "layout" } | { type: "entries"; author: string; txn: Json }
  | { type: "resource"; id: string; available: boolean; live: boolean; restored: boolean }
  | { type: "failed"; name: string; reason: string } | { type: "safe_mode"; on: boolean; was: boolean }
  | { type: "notify"; text: string; style: string | null } | { type: "backend"; name: string; error: string };

export class Workspace {
  epoch = ""; seq = 0;
  entries = new Map<string, Json>(); versions = new Map<string, number>();
  resources = new Map<string, Json>(); commands = new Map<string, Json>(); extensions = new Map<string, Json>();
  safeMode = false; safeModeAllow = new Set<string>(); providerExtensions = new Set<string>();
  live = new Map<string, Json>(); backendNotice: string | null = null;
  /** Finishes (done/error) resources reached since their tab was last shown (tab marks); kept across a reload. */
  reached = new Map<string, string>();

  /** Replace the cache with a snapshot; returns whether safe mode was on before. */
  load(snap: Json): boolean {
    const was = this.safeMode;
    this.epoch = snap.epoch ?? ""; this.seq = snap.seq ?? 0;
    this.entries = new Map(Object.entries(snap.entries ?? {}).map(([k, e]: [string, Json]) => [k, e?.value]));
    this.versions = new Map(Object.entries(snap.entries ?? {}).map(([k, e]: [string, Json]) => [k, e?.version ?? 0]));
    const before = this.resources;
    this.resources = new Map((snap.resources ?? []).filter((r: Json) => isStr(r?.id)).map((r: Json) => [r.id, r]));
    // Finishes stay latched across a reload for resources that remain; a snapshot can cover a finish whose event then
    // arrives as already seen, so compare with what was cached (a resource never seen before latches nothing).
    for (const id of [...this.reached.keys()]) if (!this.resources.has(id)) this.reached.delete(id);
    for (const [id, record] of this.resources) {
      const finish = before.has(id) ? reached(before.get(id), record) : null;
      if (finish) this.reached.set(id, this.reached.get(id) === "error" ? "error" : finish);
    }
    this.commands = new Map((snap.commands ?? []).filter((c: Json) => isStr(c?.name)).map((c: Json) => [c.name, c]));
    this.extensions = new Map(Object.entries(snap.extensions ?? {}));
    this.safeMode = snap.safe_mode === true;
    this.safeModeAllow = new Set(snap.safe_mode_allow ?? []);
    this.providerExtensions = new Set(snap.provider_extensions ?? []);
    this.live = new Map(Object.entries(snap.live ?? {}));
    this.backendNotice = isStr(snap.backend_notice) && snap.backend_notice ? snap.backend_notice : null;
    return was;
  }

  /** Apply one event record in order; reports what changed for the renderer. */
  apply(ev: Json): Change[] {
    if (ev.epoch !== this.epoch) return [{ type: "resync" }];
    if ((ev.seq ?? 0) <= this.seq) return [];
    this.seq = ev.seq;
    const d = ev.data ?? {};
    const out: Change[] = [];
    if ("backend_notice" in d) this.backendNotice = isStr(d.backend_notice) && d.backend_notice ? d.backend_notice : null;
    switch (ev.type) {
      case "entries":
        for (const c of d.changes ?? []) {
          if (!isStr(c.key)) continue;
          if (c.key.startsWith("view/")) out.push({ type: "view", view: c.key.slice(5) });
          if (c.key.startsWith("layout/") && !out.some((x) => x.type === "layout")) out.push({ type: "layout" });
          if (c.value == null) { this.entries.delete(c.key); this.versions.delete(c.key); }
          else { this.entries.set(c.key, c.value); this.versions.set(c.key, c.version ?? 0); }
        }
        out.push({ type: "entries", author: d.author ?? "?", txn: d.txn });
        break;
      case "resource": {
        if (!isStr(d.id)) break;
        let record = d;
        if (ev.partial) {
          const base = this.resources.get(d.id);
          if (!base) return [{ type: "resync" }];
          record = { ...base, ...d };
        }
        const available = record.available !== false;
        const restored = available && this.resources.get(d.id)?.available === false;
        const finish = reached(this.resources.get(d.id), record);
        if (finish) this.reached.set(d.id, this.reached.get(d.id) === "error" ? "error" : finish);
        this.resources.set(d.id, record);
        out.push({ type: "resource", id: d.id, available, live: record.state === "live", restored });
        break;
      }
      case "resource.removed": this.resources.delete(d.id); this.reached.delete(d.id); break;
      case "command":
        if (d.available === false) this.commands.delete(d.name); else if (isStr(d.name)) this.commands.set(d.name, d);
        break;
      case "extension": {
        if (!isStr(d.name)) break;
        const state = { ...(d.state ?? {}) };
        if (state.dependencies == null && this.extensions.has(d.name)) state.dependencies = this.extensions.get(d.name).dependencies;
        this.extensions.set(d.name, state);
        if (state.status === "failed") out.push({ type: "failed", name: d.name, reason: state.failure?.reason ?? state.last_exit ?? "?" });
        break;
      }
      case "safe_mode": {
        const was = this.safeMode;
        this.safeMode = d.on === true;
        this.safeModeAllow = new Set(d.allow ?? []);
        this.providerExtensions = new Set(d.provider_extensions ?? []);
        out.push({ type: "safe_mode", on: this.safeMode, was });
        break;
      }
      case "view.content":
        if (!isStr(d.view)) break;
        if (d.content == null) this.live.delete(d.view); else this.live.set(d.view, d.content);
        out.push({ type: "view", view: d.view });
        break;
      case "notify": out.push({ type: "notify", text: String(d.text ?? ""), style: isStr(d.style) ? d.style : null }); break;
      case "adapter": case "provider":
        if (d.state?.connected === false) out.push({ type: "backend", name: String(d.name ?? "?"), error: String(d.state?.error ?? "") });
        break;
    }
    return out;
  }

  visible(e: Json): boolean {
    if (isStr(e.owner)) {
      const key = `extension/${e.owner}`;
      const decl = this.entries.get(key);
      const on = this.entries.has(key) && !(isObj(decl) && decl.enabled === false);
      if (this.safeMode ? !this.safeModeAllow.has(e.owner) : !on) return false;
    }
    const cmd = isStr(e.command) ? e.command : "";
    return cmd.startsWith("client.") || this.commands.get(cmd)?.available === true;
  }

  private sorted(prefix: string): [string, Json][] {
    return [...this.entries].filter(([k, v]) => k.startsWith(prefix) && isObj(v)).sort((a, b) => cmp(a[0], b[0]));
  }

  menuEntries(place: string, ctx: Json): Json[] {
    return this.sorted("menu/").filter(([, v]) => v.where === place && this.visible(v) && whenHolds(v, ctx))
      .map(([k, v]) => [isInt(v.order) ? v.order : 50, k, v] as [number, string, Json])
      .sort((a, b) => a[0] - b[0] || cmp(a[1], b[1])).map(([, , v]) => v);
  }

  keyCandidates(ctx: Json): [string, Json][] {
    return [
      ...builtinBindings().map((b) => [BUILTIN + b.key, b] as [string, Json]),
      ...this.sorted("binding/").filter(([, v]) => scope(v) === "prefix" && !String(v.key ?? "").startsWith("mouse.") && this.visible(v) && whenHolds(v, ctx)),
    ];
  }

  /** The binding a key runs without the prefix: scope "pane" (not typing) or "composer" (typing); never in
   *  terminal panes. */
  scopedBinding(key: string, scopeName: string, ctx: Json): Json | null {
    if (ctx?.kind === "terminal") return null;
    let best: [[number, number, string], Json] | null = null;
    for (const [k, b] of this.sorted("binding/")) {
      if (scope(b) !== scopeName || !this.visible(b) || !whenHolds(b, ctx) || !isStr(b.key) || normalize(b.key) !== key) continue;
      const r = rank(k, b);
      if (!best || rankLess(r, best[0]) < 0) best = [r, b];
    }
    return best && best[1].command !== "client.none" ? best[1] : null;
  }

  bindingFor(key: string, ctx: Json): Json | null {
    const cands = this.keyCandidates(ctx);
    const i = resolve(cands).get(key);
    return i === undefined || cands[i][1].command === "client.none" ? null : cands[i][1];
  }

  gesture(key: string, place: string): Json | null {
    let best: [[number, number, string], Json] | null = null;
    for (const [k, b] of this.sorted("binding/")) {
      if (b.key !== key || (b.where ?? "pane") !== place || !this.visible(b)) continue;
      const r = rank(k, b);
      if (!best || rankLess(r, best[0]) < 0) best = [r, b];
    }
    return best && best[1].command !== "client.none" ? best[1] : null;
  }

  bindingLabel(b: Json): string {
    const cmd = isStr(b.command) ? b.command : "";
    if (cmd === "vqro.safe_mode" && this.safeMode) return "leave safe mode";
    return b.label || this.commands.get(cmd)?.title || cmd;
  }
}

// ---- recovery content -------------------------------------------------------------------------------------------

export interface PrefixRow { key: string | null; label: string; binding: Json }

/** The prefix menu: winning binding per key, failed extensions' restart rows, then unreachable recovery actions. */
export function prefixRows(ws: Workspace, ctx: Json): PrefixRow[] {
  const cands = ws.keyCandidates(ctx);
  const winners = resolve(cands);
  const out: PrefixRow[] = [];
  cands.forEach(([, b], i) => {
    const key = isStr(b.key) ? normalize(b.key) : null;
    if (key !== null && winners.get(key) === i && b.command !== "client.none" && b.hidden !== true) out.push({ key, label: ws.bindingLabel(b), binding: b });
  });
  const failed: PrefixRow[] = [];
  for (const [name, state] of [...ws.extensions].sort((a, b) => cmp(a[0], b[0]))) {
    if (state?.status !== "failed") continue;
    const key = `extension/${name}`;
    const decl = ws.entries.get(key);
    const allowed = ws.safeMode ? ws.safeModeAllow.has(name) : ws.entries.has(key) && !(isObj(decl) && decl.enabled === false);
    failed.push({ key: null, label: allowed ? `restart ${name} (failed)` : `clear failure: ${name} (stays stopped)`, binding: { command: "vqro.extension.restart", args: { name } } });
  }
  out.splice(Math.min(out.length, 1), 0, ...failed);
  for (const b of builtinBindings()) if (b.recovery && !out.some((r) => r.binding.command === b.command)) out.push({ key: null, label: ws.bindingLabel(b), binding: b });
  return out;
}

export function needsText(deps: Json): string {
  const out: string[] = [];
  for (const need of deps?.needs ?? []) {
    if (!isStr(need.cap) || need.cap.startsWith("ui/")) continue;
    const who = (need.providers ?? []).map((p: Json) => p?.name).filter(isStr);
    if (who.includes("vqro")) continue;
    const text = who.length ? who.join(" or ") : need.suggest ?? need.cap;
    if (!out.includes(text)) out.push(text);
  }
  return [...out.join(", ")].slice(0, 120).join("");
}

export function safeChooser(ws: Workspace, selected: Set<string>): Json {
  const declared = [...ws.entries].filter(([k]) => k.startsWith("extension/")).map(([k, v]) => [k.slice(10), v] as [string, Json]).sort((a, b) => cmp(a[0], b[0]));
  const items: Json[] = [{ type: "text", text: "Nothing starts automatically. Choose this session's recovery set; saved enabled settings are unchanged." }];
  for (const [provider, title] of [[true, "Providers / backends"], [false, "UI extensions"]] as [boolean, string][]) {
    items.push({ type: "header", text: title });
    let count = 0;
    for (const [name, raw] of declared) {
      const value = isObj(raw) ? raw : {};
      if ((value.kind === "provider" || ws.providerExtensions.has(name)) !== provider) continue;
      const argv = Array.isArray(value.argv) ? value.argv : [];
      let description = [...(isStr(value.description) ? value.description : isStr(argv[0]) ? argv[0] : "Declared extension")].slice(0, 160).join("");
      const state = ws.extensions.get(name);
      if (state != null) {
        if (state.status === "failed") description += " -- failed: " + (isStr(state.last_exit) ? state.last_exit : "unknown failure");
        const needs = needsText(state.dependencies);
        if (needs) description += " -- needs " + needs;
      }
      items.push({ id: `safe/${name}`, text: (selected.has(name) ? "[x] " : "[ ] ") + name, description, command: "client.safe_mode.toggle", args: { name } });
      count++;
    }
    if (!count) items.push({ type: "text", text: "(none declared)" });
  }
  items.push({ type: "separator" }, { id: "safe/apply", text: "Enable selected", command: "client.safe_mode.apply" }, { id: "safe/leave", text: "Leave safe mode", command: "client.safe_mode.leave" });
  return { title: "Safe mode recovery", items };
}

// ---- the window's layout -----------------------------------------------------------------------------------------

export function defaultLayout(ws: Workspace): string | null {
  const a = ws.entries.get("workspace/active");
  if (isObj(a) && isStr(a.layout) && ws.entries.has(`layout/${a.layout}`)) return a.layout;
  return ws.entries.has("layout/main") ? "main" : null;
}

export type Settled = "Same" | { Took: string } | { Closed: { was: string; now: string | null } };

/** Which layout this window shows: window state, never workspace state. */
export class Window {
  constructor(public pinned: string | null = null) {}
  private exists(ws: Workspace, name: string | null) { return isStr(name) && ws.entries.has(`layout/${name}`); }
  shown(ws: Workspace): string | null { return this.exists(ws, this.pinned) ? this.pinned : defaultLayout(ws); }
  settle(ws: Workspace): Settled {
    if (this.exists(ws, this.pinned)) return "Same";
    let now = defaultLayout(ws);
    if (!this.exists(ws, now)) now = null;
    if (this.pinned !== null) { const was = this.pinned; this.pinned = now; return { Closed: { was, now } }; }
    if (now === null) return "Same";
    this.pinned = now;
    return { Took: now };
  }
  show(ws: Workspace, name: string): [boolean, boolean | string] {
    if (!this.exists(ws, name)) return [false, `no layout ${name}`];
    const changed = this.pinned !== name;
    this.pinned = name;
    return [true, changed];
  }
  clientLayout(ws: Workspace, args: Json): [boolean, boolean | string] {
    let name = args?.name ?? args?.layout;
    if (!isStr(name)) { name = defaultLayout(ws); if (name === null) return [false, "client.layout: no layout to show"]; }
    return this.show(ws, name);
  }
  context(ws: Workspace): Json {
    const layout = this.shown(ws);
    const settings = layout ? ws.entries.get(`doc/layout/${layout}`) : null;
    const title = isObj(settings) && isStr(settings.title) ? settings.title : layout;
    return { layout, layout_version: layout ? ws.versions.get(`layout/${layout}`) ?? null : null, layout_title: title };
  }
  invocationContext(ws: Workspace, ctx: Json): Json {
    const out = isObj(ctx) ? { ...ctx } : {};
    if (out.layout == null) { const w = this.context(ws); out.layout = w.layout; out.layout_version = w.layout_version; }
    return out;
  }
}

export function replyLayout(command: string, reply: Json): string | null {
  const layout = reply?.client?.layout;
  if (isStr(layout)) return layout;
  return command === "vqro.layout.reset" ? "main" : null;
}

export function markCurrent(value: Json, layout: string | null): Json {
  const v = structuredClone(value);
  const walk = (items: Json) => {
    for (const row of Array.isArray(items) ? items : []) {
      if (!isObj(row)) continue;
      if (layout !== null && isObj(row.current_when) && row.current_when.layout === layout) {
        if (row.style == null) row.style = "accent";
        row.badges = [{ text: "current", style: "accent" }, ...(Array.isArray(row.badges) ? row.badges : [])];
      }
      if ("children" in row) walk(row.children);
    }
  };
  if (isObj(v)) { walk(v.items); walk(v.toolbar); for (const p of Array.isArray(v.pages) ? v.pages : []) if (isObj(p)) walk(p.items); }
  return v;
}

// ---- formatted text (format: "markdown", feature markdown) ------------------------------------------------------

export type Span = [string, string];
const cw = (c: string) => Bun.stringWidth(c);

type MdBlock = { kind: "heading" | "para"; text: string } | { kind: "item"; level: number; marker: string; text: string } | { kind: "code"; lines: string[] } | { kind: "blank" };

function mdParse(text: string): MdBlock[] {
  const blocks: MdBlock[] = [];
  let para: string | null = null, inItem = false;
  const lines = text.replace(/\r/g, "").replace(/\t/g, "    ").split("\n");
  const flush = () => { if (para !== null) { blocks.push({ kind: "para", text: para }); para = null; } };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.replace(/^ +/, "");
    if (line.trimStart().startsWith("```")) {
      flush();
      const code: string[] = [];
      for (i++; i < lines.length && !lines[i].trimStart().startsWith("```"); i++) code.push(lines[i]);
      blocks.push({ kind: "code", lines: code });
      inItem = false;
      continue;
    }
    if (!line.trim()) { flush(); if (blocks.length && blocks[blocks.length - 1].kind !== "blank") blocks.push({ kind: "blank" }); inItem = false; continue; }
    const hashes = line.length - line.replace(/^#+/, "").length;
    if (hashes >= 1 && hashes <= 3 && line[hashes] === " ") { flush(); blocks.push({ kind: "heading", text: line.slice(hashes).trim() }); inItem = false; continue; }
    const indent = line.length - trimmed.length;
    let marker: [string, number] | null = null;
    if (trimmed.startsWith("- ") || trimmed.startsWith("* ")) marker = ["•", 2];
    else { const mm = /^([0-9]{1,9})\. /.exec(trimmed); if (mm) marker = [mm[1] + ".", mm[1].length + 2]; }
    if (marker) { flush(); blocks.push({ kind: "item", level: Math.min(Math.floor(indent / 2), 4), marker: marker[0], text: trimmed.slice(marker[1]).trim() }); inItem = true; continue; }
    const last = blocks[blocks.length - 1];
    if (inItem && indent >= 2 && last?.kind === "item") { last.text += " " + line.trim(); continue; }
    inItem = false;
    para = para === null ? line.trim() : para + " " + line.trim();
  }
  flush();
  return blocks;
}

function mdInline(s: string, plain: string): Span[] {
  const out: Span[] = [];
  let buf = "", i = 0;
  while (i < s.length) {
    if (s[i] === "`") {
      const j = s.indexOf("`", i + 1);
      if (j !== -1) { if (buf) { out.push([buf, plain]); buf = ""; } out.push([s.slice(i + 1, j), "c"]); i = j + 1; continue; }
    }
    if (s.startsWith("**", i)) {
      const j = s.indexOf("**", i + 2);
      if (j > i + 2) { if (buf) { out.push([buf, plain]); buf = ""; } out.push([s.slice(i + 2, j), plain === "h" ? "h" : "b"]); i = j + 2; continue; }
    }
    const c = String.fromCodePoint(s.codePointAt(i)!);
    buf += c;
    i += c.length;
  }
  if (buf) out.push([buf, plain]);
  return out;
}

const mdMerge = (line: Span[]): Span[] => line.reduce<Span[]>((out, [t, s]) => {
  if (out.length && out[out.length - 1][1] === s) out[out.length - 1] = [out[out.length - 1][0] + t, s]; else out.push([t, s]);
  return out;
}, []);

function mdWrap(spans: Span[], width: number): Span[][] {
  const w = Math.max(1, width);
  const units: Span[][] = [];
  let space = true;
  const push = (word: string, style: string) => { if (space || !units.length) units.push([[word, style]]); else units[units.length - 1].push([word, style]); };
  for (const [text, style] of spans) {
    let word = "";
    for (const c of text) {
      if (/\s/u.test(c)) { if (word) push(word, style); word = ""; space = true; }
      else word += c;
    }
    if (word) { push(word, style); space = false; }
  }
  const out: Span[][] = [];
  let line: Span[] = [], used = 0;
  for (const unit of units) {
    const uw = unit.reduce((n, [t]) => n + cw(t), 0);
    if (used > 0 && used + 1 + uw > w) { out.push(line); line = []; used = 0; }
    if (used > 0) { const prev = line.length ? line[line.length - 1][1] : ""; line.push([" ", prev === unit[0][1] ? prev : ""]); used += 1; }
    for (const [text, style] of unit) for (const c of text) {
      const k = cw(c);
      if (used + k > w && used > 0) { out.push(line); line = []; used = 0; }
      line.push([c, style]);
      used += k;
    }
  }
  if (line.length || !out.length) out.push(line);
  return out.map(mdMerge);
}

function mdCut(s: string, w: number): string {
  let out = "", used = 0;
  for (const c of s) { if (used + cw(c) > Math.max(0, w - 1)) break; out += c; used += cw(c); }
  return out + "…";
}

/** Formatted text laid out at `width` cells: lines of [text, style] spans (styles: "" h b c pre). */
export function markdownLines(text: string, width: number): Span[][] {
  const w = Math.max(1, width);
  const out: Span[][] = [];
  let prevItem = false, gap = false;
  for (const b of mdParse(text)) {
    if (b.kind === "blank") { gap = true; continue; }
    const isItem = b.kind === "item";
    if (out.length && (gap || !(prevItem && isItem))) out.push([]);
    gap = false; prevItem = isItem;
    if (b.kind === "heading") out.push(...mdWrap(mdInline(b.text, "h"), w));
    else if (b.kind === "para") out.push(...mdWrap(mdInline(b.text, ""), w));
    else if (b.kind === "item") {
      const prefix = "  ".repeat(b.level) + b.marker + " ";
      const pw = cw(prefix);
      mdWrap(mdInline(b.text, ""), Math.max(1, w - pw)).forEach((l, n) => out.push(mdMerge([[n === 0 ? prefix : " ".repeat(pw), ""], ...l])));
    } else if (b.kind === "code") for (const l of b.lines) { const shown = "  " + l; out.push([[cw(shown) > w ? mdCut(shown, w) : shown, "pre"]]); }
  }
  return out;
}

// ---- composer and multi-line prompts (features composer, multiline) ---------------------------------------------

export const DEFAULT_MAX_BYTES = 16 * 1024, MAX_BYTES = 64 * 1024;
export type Outcome = "edit" | "submit" | "leave" | "refused" | "ignored";
const utf8 = (s: string) => new TextEncoder().encode(s).length;

/** A multi-line text buffer with a cursor (a character index); key handling shared by every client. */
export class Editor {
  text: string[];
  cursor: number;
  maxBytes: number;
  submitCtrl: boolean;
  constructor(text = "", maxBytes?: Json, submit?: Json) {
    this.text = [...text];
    this.cursor = this.text.length;
    this.maxBytes = isInt(maxBytes) ? Math.min(maxBytes, MAX_BYTES) : DEFAULT_MAX_BYTES;
    this.submitCtrl = submit === "C-Enter";
  }
  string() { return this.text.join(""); }
  insert(s: string): Outcome {
    const clean = [...s.replace(/\r\n/g, "\n").replace(/\r/g, "\n").replace(/\t/g, "    ")].filter((c) => c === "\n" || !/\p{Cc}/u.test(c));
    if (!clean.length) return "ignored";
    if (utf8(this.string()) + utf8(clean.join("")) > this.maxBytes) return "refused";
    this.text.splice(this.cursor, 0, ...clean);
    this.cursor += clean.length;
    return "edit";
  }
  private start(at: number) { while (at > 0 && this.text[at - 1] !== "\n") at--; return at; }
  private end(at: number) { while (at < this.text.length && this.text[at] !== "\n") at++; return at; }
  key(key: string): Outcome {
    if ((key === "Enter" && !this.submitCtrl) || (key === "C-Enter" && this.submitCtrl)) return this.string().trim() ? "submit" : "ignored";
    switch (key) {
      case "Enter": case "S-Enter": case "M-Enter": case "C-j": return this.insert("\n");
      case "Esc": return "leave";
      case "Backspace": if (!this.cursor) return "ignored"; this.cursor--; this.text.splice(this.cursor, 1); return "edit";
      case "Delete": if (this.cursor >= this.text.length) return "ignored"; this.text.splice(this.cursor, 1); return "edit";
      case "Left": if (!this.cursor) return "ignored"; this.cursor--; return "edit";
      case "Right": if (this.cursor >= this.text.length) return "ignored"; this.cursor++; return "edit";
      case "Home": this.cursor = this.start(this.cursor); return "edit";
      case "End": this.cursor = this.end(this.cursor); return "edit";
      case "Up": case "Down": {
        const start = this.start(this.cursor), column = this.cursor - start;
        let target: number;
        if (key === "Up") { if (!start) return "ignored"; target = this.start(start - 1); }
        else { const end = this.end(this.cursor); if (end >= this.text.length) return "ignored"; target = end + 1; }
        this.cursor = Math.min(target + column, this.end(target));
        return "edit";
      }
      case "Space": return this.insert(" ");
    }
    return [...key].length === 1 ? this.insert(key) : "ignored";
  }
  /** Lines and the cursor's [line, column], for drawing. */
  lines(): [string[], [number, number]] {
    const before = this.text.slice(0, this.cursor);
    return [this.string().split("\n"), [before.filter((c) => c === "\n").length, this.cursor - this.start(this.cursor)]];
  }
}

export const composerControls = (row: Json): Json[] =>
  (Array.isArray(row.controls) ? row.controls : []).filter((c: Json) => isObj(c) && isStr(c.arg) && (c.type === "select" || c.type === "chips"));

/** A composer row's client-local state: draft, cursor and control values, until Send. */
export class Composer {
  values: Record<string, Json> = {};
  editor: Editor;
  constructor(row: Json) {
    for (const c of composerControls(row)) {
      this.values[c.arg] = c.type === "chips" ? ((c.chips ?? []).find((x: Json) => isObj(x) && x.checked === true)?.value ?? null) : c.value ?? null;
    }
    this.editor = new Editor(isStr(row.prefill) ? row.prefill : "", row.max_bytes, row.submit);
  }
  submitArgs(row: Json): Json {
    const args: Json = isObj(row.args) ? { ...row.args } : {};
    for (const [k, v] of Object.entries(this.values)) if (v != null) args[k] = v;
    args.text = this.editor.string();
    return args;
  }
}

// ---- tab bar (doc/settings/tabs; ui_model::tabbar) ----------------------------------------------------------------

export const TABS_ENTRY = "doc/settings/tabs";
const SUB = "₀₁₂₃₄₅₆₇₈₉", SUP = "⁰¹²³⁴⁵⁶⁷⁸⁹";
export type TabStyle = { numbers: "subscript" | "superscript" | "plain" | "none"; before: boolean; chips: boolean };

/** The shared tabs setting: missing means subscript numbers after the name, in chips; an unknown `numbers` value
 * reads as plain, an unknown `position` as after. */
export function tabStyle(setting: Json): TabStyle {
  const style: TabStyle = { numbers: "subscript", before: false, chips: true };
  if (setting && typeof setting === "object" && !Array.isArray(setting)) {
    if (typeof setting.numbers === "string") style.numbers = ["subscript", "superscript", "none", "plain"].includes(setting.numbers) ? setting.numbers : "plain";
    style.before = setting.position === "before";
    if (typeof setting.chips === "boolean") style.chips = setting.chips;
  }
  return style;
}

export function tabLabel(style: TabStyle, index: number, title: string): string {
  const digits = String(index + 1);
  const number = style.numbers === "subscript" ? [...digits].map((d) => SUB[+d]).join("")
    : style.numbers === "superscript" ? [...digits].map((d) => SUP[+d]).join("") : style.numbers === "plain" ? digits : "";
  if (!number) return ` ${title} `;
  if (!style.before) return ` ${title}${style.numbers === "plain" ? " " : ""}${number} `;
  const joiner = style.numbers === "superscript" ? "" : style.numbers === "plain" && !style.chips ? ":" : " ";
  return ` ${number}${joiner}${title} `;
}

export function commandLabel(resource: Json): string {
  const argv = resource?.argv;
  return Array.isArray(argv) ? (argv[0] ?? "default shell") : "unknown command";
}

export const PROGRAM_TITLE_MAX = 24;

/** The name a tab shows (ui_model::tabbar::tab_title): with `program_titles` (default on) a tab still named after its
 * only terminal's command shows the program's own title; renamed tabs and named resources keep theirs. */
export function tabTitle(setting: Json, title: string, resource: Json): string {
  const on = isObj(setting) && typeof setting.program_titles === "boolean" ? setting.program_titles : true;
  if (!on || !isObj(resource)) return title;
  const program = typeof resource.title === "string" ? resource.title.trim() : "";
  const named = typeof resource.name === "string" && resource.name !== "";
  const command = commandLabel(resource).split("/").pop()!;
  if (!program || named || command === null || title !== command) return title;
  const chars = [...program];
  return chars.length > PROGRAM_TITLE_MAX ? chars.slice(0, PROGRAM_TITLE_MAX - 1).join("") + "…" : program;
}

/** A tab's lifecycle hint (ui_model::tabbar::tab_state): "ended" or "gone", drawn dim; unknown values: null. */
export function tabState(tab: Json): string | null {
  return isObj(tab) && (tab.state === "ended" || tab.state === "gone") ? tab.state : null;
}

export const TAB_MARKS = ["blocked", "error", "done", "activity"];

/** What a client remembers about a resource when its tab is shown (ui_model::tabbar::seen_of). */
export function seenOf(resource: Json): Json {
  return { title: resource?.title ?? null };
}

const stateOf = (r: Json): string | null => isObj(r?.status) && typeof r.status.state === "string" ? r.status.state : null;

/** The finish an update reaches (ui_model::tabbar::reached): done/error when the status state turns into it. */
export function reached(old: Json, next: Json): string | null {
  const state = stateOf(next);
  return (state === "done" || state === "error") && state !== stateOf(old) ? state : null;
}

/** The mark an inactive tab shows (ui_model::tabbar::tab_mark): blocked while an agent waits; error/done reached
 * since last shown (latched from events); activity when a program title changed since. Off unless `marks` is true. */
export function tabMark(setting: Json, now: Json[], seen: Json[], reachedSince: (string | null)[]): string | null {
  if (!(isObj(setting) && setting.marks === true)) return null;
  const found: string[] = [];
  now.forEach((r, i) => {
    const was = seen[i], finish = reachedSince[i] ?? null;
    if (stateOf(r) === "blocked") found.push("blocked");
    else if (finish === "error" || finish === "done") found.push(finish);
    else if (isObj(was) && typeof r?.title === "string" && r.title !== was.title) found.push("activity");
  });
  return found.length ? found.sort((a, b) => TAB_MARKS.indexOf(a) - TAB_MARKS.indexOf(b))[0] : null;
}

export function tabBar(setting: Json, titles: string[], active: number): Json {
  const style = tabStyle(setting);
  return { labels: titles.map((t, i) => tabLabel(style, i, t)), active, gap: style.chips ? 1 : 0, chips: style.chips };
}
