// Where a theme's card colour comes from (core-protocol.md, Themes: `card`, `background`). Kept apart from theme.ts
// so it is tested without the OpenTUI packages.

const isDefault = (v: any) => typeof v === "string" && v.trim().toLowerCase() === "default";

/** A card for a theme that declares only its base background (`#RRGGBB`): 5% toward white on a dark background,
 *  toward black on a light one. Null when the background isn't an RGB value. */
export function cardFromBackground(v: any): string | null {
  if (typeof v !== "string" || !/^#[0-9a-f]{6}$/i.test(v.trim())) return null;
  const s = v.trim();
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(s.slice(i, i + 2), 16));
  const light = 0.2126 * r + 0.7152 * g + 0.0722 * b >= 127.5;
  const step = (c: number) => Math.round(light ? c * 0.95 : c + (255 - c) * 0.05);
  return "#" + [r, g, b].map((c) => step(c).toString(16).padStart(2, "0")).join("");
}

export type CardSource = { kind: "default" } | { kind: "token"; token: "card" | "surface" } | { kind: "derived"; color: string } | { kind: "builtin" };

/** In order: the theme's readable `card` (`default` = the terminal's background), its readable `surface`, a card
 *  derived from its `background`, else the client's built-in card. `readable` is the client's colour parser. */
export function cardSource(palette: Record<string, any>, readable: (v: any) => boolean): CardSource {
  for (const token of ["card", "surface"] as const) {
    if (!(token in palette)) continue;
    if (isDefault(palette[token])) return { kind: "default" };
    if (readable(palette[token])) return { kind: "token", token };
  }
  const derived = cardFromBackground(palette.background);
  return derived ? { kind: "derived", color: derived } : { kind: "builtin" };
}
