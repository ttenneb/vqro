// Meaning roles (core-protocol.md, Themes): which colour value each role gets. Pure, so `bun test` checks it against
// tests/fixtures/client/roles.json without OpenTUI's packages; theme.ts parses the result.

/** The 16 ANSI colour names (also accepted as `dark-gray`, `Light Blue`, ...). */
export const ANSI: Record<string, number> = {
  black: 0, red: 1, green: 2, yellow: 3, blue: 4, magenta: 5, cyan: 6, gray: 7, grey: 7, darkgray: 8, darkgrey: 8,
  lightred: 9, lightgreen: 10, lightyellow: 11, lightblue: 12, lightmagenta: 13, lightcyan: 14, white: 15,
};

/** Whether this client reads a theme colour value: ANSI name, #RRGGBB, 256-colour index, or `default`. */
export function isColor(v: any): boolean {
  if (typeof v === "number") return Number.isInteger(v) && v >= 0 && v < 256;
  if (typeof v !== "string") return false;
  const s = v.trim();
  return s.toLowerCase() === "default" || /^#[0-9a-f]{6}$/i.test(s) || /^\d+$/.test(s) && +s < 256
    || s.toLowerCase().replace(/[-_ ]/g, "") in ANSI;
}

/** Each role and the role it follows when a theme leaves it out. */
export const ROLES: Record<string, string> = {
  heading: "accent", control: "accent", link: "accent", info: "accent",
  you: "accent", agent: "muted", other_agent: "accent", changed: "warn",
};

/** The built-in role colours (the 16 ANSI colours, like the rest of the default palette). */
export const BUILTIN: Record<string, string> = {
  heading: "cyan", control: "lightblue", link: "lightblue", info: "lightcyan",
  you: "lightyellow", agent: "lightmagenta", other_agent: "lightgreen", changed: "yellow",
};

/** A role's colour value: the theme's own, else the theme's fallback role, else built in. `readable` says
 * whether this client can use a value (an unreadable one counts as missing). */
export function roleValue(palette: Record<string, any>, role: string, readable: (v: any) => boolean = isColor): any {
  if (role in palette && readable(palette[role])) return palette[role];
  const fallback = ROLES[role];
  if (fallback in palette && readable(palette[fallback])) return palette[fallback];
  return BUILTIN[role];
}
