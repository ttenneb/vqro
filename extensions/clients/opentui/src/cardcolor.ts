// Where a theme's card colour comes from (core-protocol.md, Themes: `card`, `background`). Kept apart from theme.ts
// so it is tested without the OpenTUI packages.

const isDefault = (v: any) => typeof v === "string" && v.trim().toLowerCase() === "default";

/** A card for a theme that declares only its base background (`#RRGGBB`): 5% toward white on a dark background,
 *  toward black on a light one. Null when the background isn't an RGB value. */
export function cardFromBackground(v: any): string | null {
  if (typeof v !== "string" || !/^#[0-9a-f]{6}$/i.test(v.trim())) return null;
  const s = v.trim();
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(s.slice(i, i + 2), 16));
  // Integer weights and half-up rounding match Rust, including threshold ties.
  const light = 2126 * r + 7152 * g + 722 * b >= 1_275_000;
  const step = (c: number) => Math.floor((19 * c + (light ? 0 : 255) + 10) / 20);
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
