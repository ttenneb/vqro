// Semantic palette (D18): `doc/settings/theme.palette` tokens over a built-in default; safe mode ignores themes.
import { RGBA, TextAttributes } from "@opentui/core";
import { cardSource } from "./cardcolor";
import { ANSI, ROLES, roleValue } from "./roles";

/** A theme colour value as RGBA: ANSI name, #RRGGBB, 256-colour index (number or numeric string), or `default`. */
export function parseColor(v: any): RGBA | null {
  if (typeof v === "number") return Number.isInteger(v) && v >= 0 && v < 256 ? RGBA.fromIndex(v) : null;
  if (typeof v !== "string") return null;
  const s = v.trim();
  if (s.toLowerCase() === "default") return null;
  if (/^#[0-9a-f]{6}$/i.test(s)) return RGBA.fromHex(s);
  if (/^\d+$/.test(s)) return +s < 256 ? RGBA.fromIndex(+s) : null;
  const name = s.toLowerCase().replace(/[-_ ]/g, "");
  return name in ANSI ? RGBA.fromIndex(ANSI[name]) : null;
}

const DEFAULTS: Record<string, any> = {
  text: "default", muted: "gray", dim: "darkgray", accent: "cyan", ok: "green", warn: "yellow", error: "red",
  prompt: "magenta", border: "darkgray", border_focus: "cyan", selection: null, selection_text: null,
  badge: "darkgray", badge_text: "black", surface: "#282830", card: "#202026", tab_active: "blue", tab_active_text: "white", tab_bar: 235,
};

export class Palette {
  private colors: Record<string, RGBA | null> = {};
  /** Tokens whose colour is the terminal's own background (`default`); only `card` uses this today. */
  private terminalBg = new Set<string>();
  readonly fg = RGBA.defaultForeground();
  readonly bg = RGBA.defaultBackground();

  constructor(theme: any, safeMode: boolean) {
    const palette = !safeMode && theme && typeof theme.palette === "object" ? theme.palette : {};
    for (const [token, value] of Object.entries(DEFAULTS)) {
      const mine = token in palette ? parseColor(palette[token]) : null;
      this.colors[token] = mine ?? (value === null ? null : parseColor(value));
    }
    // Meaning roles: the theme's own, its fallback role, else built in (roles.ts).
    for (const role of Object.keys(ROLES)) this.colors[role] = parseColor(roleValue(palette, role, (v) => parseColor(v) !== null || v === "default"));
    // The card: the theme's own, its surface, one derived from its background, else built in (cardcolor.ts).
    const card = cardSource(palette, (v) => parseColor(v) !== null);
    if (card.kind === "default") this.terminalBg.add("card");
    else if (card.kind === "token") this.colors.card = parseColor(palette[card.token]);
    else if (card.kind === "derived") this.colors.card = parseColor(card.color);
  }

  /** A background colour: the terminal's own for a token the theme set to `default`, else the token's colour. */
  background(token: string): RGBA { return this.terminalBg.has(token) ? this.bg : this.color(token); }

  color(token: string): RGBA { return this.colors[token] ?? this.fg; }
  maybe(token: string): RGBA | null { return this.colors[token] ?? null; }

  /** A semantic style name (`bold`, `plain`, `dim`, `muted`, `warn`, `error`, `ok`, `accent`, or a meaning role such
   * as `heading` or `link`): colour and attributes. `link` is underlined. Unknown names are plain text. */
  style(name: string | null | undefined): [RGBA, number] {
    switch (name) {
      case "bold": return [this.color("text"), TextAttributes.BOLD];
      case "link": return [this.color("link"), TextAttributes.UNDERLINE];
      case "dim": case "muted": case "accent": case "ok": case "warn": case "error": return [this.color(name), TextAttributes.NONE];
      default: return name && name in ROLES ? [this.color(name), TextAttributes.NONE] : [this.color("text"), TextAttributes.NONE];
    }
  }
}
