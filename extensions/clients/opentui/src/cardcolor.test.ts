import { expect, test } from "bun:test";
import { cardFromBackground, cardSource } from "./cardcolor";

// The client's parser accepts these (theme.ts parseColor); `default` is handled by cardSource itself.
const readable = (v: any) => typeof v === "string" && /^#[0-9a-f]{6}$|^(cyan|gray)$/i.test(v.trim());

test("card: the theme's own, `default` meaning the terminal's background", () => {
  expect(cardSource({ card: "#101010", surface: "#202020" }, readable)).toEqual({ kind: "token", token: "card" });
  expect(cardSource({ card: "default" }, readable)).toEqual({ kind: "default" });
  expect(cardSource({ card: " Default ", surface: "#202020" }, readable)).toEqual({ kind: "default" });
});

test("card: an old theme follows its surface, `default` included", () => {
  expect(cardSource({ surface: "#202020" }, readable)).toEqual({ kind: "token", token: "surface" });
  expect(cardSource({ surface: "default" }, readable)).toEqual({ kind: "default" });
  // An unreadable card counts as missing.
  expect(cardSource({ card: "chartreuse", surface: "#202020" }, readable)).toEqual({ kind: "token", token: "surface" });
});

test("card: derived from a declared background, else built in", () => {
  expect(cardSource({ background: "#000000" }, readable)).toEqual({ kind: "derived", color: "#0d0d0d" });
  expect(cardSource({ background: "#ffffff" }, readable)).toEqual({ kind: "derived", color: "#f2f2f2" });
  expect(cardSource({ background: "#000000", surface: "#303030" }, readable)).toEqual({ kind: "token", token: "surface" });
  for (const p of [{}, { accent: "cyan" }, { card: "chartreuse" }, { surface: "nonsense" }, { background: "default" }, { background: "cyan" }])
    expect(cardSource(p, readable), JSON.stringify(p)).toEqual({ kind: "builtin" });
  // The same values as the TUI (theme.rs card_from_background).
  expect(cardFromBackground("#1e1e2e")).toBe("#292938");
});
