// Semantic palette (D18): `doc/settings/theme.palette` tokens over a built-in default; safe mode ignores themes.
import { RGBA, TextAttributes } from "@opentui/core";

const ANSI: Record<string, number> = {
  black: 0, red: 1, green: 2, yellow: 3, blue: 4, magenta: 5, cyan: 6, gray: 7, grey: 7, darkgray: 8, darkgrey: 8,
  lightred: 9, lightgreen: 10, lightyellow: 11, lightblue: 12, lightmagenta: 13, lightcyan: 14, white: 15,
};

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
  badge: "darkgray", badge_text: "black", surface: "#282830", tab_active: "blue", tab_active_text: "white", tab_bar: 235,
};

export class Palette {
  private colors: Record<string, RGBA | null> = {};
  readonly fg = RGBA.defaultForeground();
  readonly bg = RGBA.defaultBackground();

  constructor(theme: any, safeMode: boolean) {
    const palette = !safeMode && theme && typeof theme.palette === "object" ? theme.palette : {};
    for (const [token, value] of Object.entries(DEFAULTS)) {
      const mine = token in palette ? parseColor(palette[token]) : null;
      this.colors[token] = mine ?? (value === null ? null : parseColor(value));
    }
  }

  color(token: string): RGBA { return this.colors[token] ?? this.fg; }
  maybe(token: string): RGBA | null { return this.colors[token] ?? null; }

  /** A semantic style name (`bold`, `dim`, `muted`, `warn`, `error`, `ok`, `accent`, `plain`): colour and attributes. */
  style(name: string | null | undefined): [RGBA, number] {
    switch (name) {
      case "bold": return [this.color("text"), TextAttributes.BOLD];
      case "dim": case "muted": case "accent": case "ok": case "warn": case "error": return [this.color(name), TextAttributes.NONE];
      default: return [this.color("text"), TextAttributes.NONE];
    }
  }
}
