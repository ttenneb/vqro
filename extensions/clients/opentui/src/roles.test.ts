// The role fallback against the shared fixture (the Rust client passes the same file: src/tui/theme.rs).
import { expect, test } from "bun:test";
import { join } from "path";
import { BUILTIN, ROLES, isColor, roleValue } from "./roles";

const file = join(import.meta.dir, "../../../../tests/fixtures/client/roles.json");

test("roles and built-ins match the fixture", async () => {
  const f = JSON.parse(await Bun.file(file).text());
  expect(ROLES).toEqual(f.roles);
  expect(BUILTIN).toEqual(f.builtin);
});

test("colour values this client reads", () => {
  for (const v of ["cyan", "Dark-Gray", "#1a2B3c", 208, "208", "default"]) expect([v, isColor(v)]).toEqual([v, true]);
  for (const v of [256, "#12345", "chartreuse", { r: 1 }, null]) expect([v, isColor(v)]).toEqual([v, false]);
});

test("each case resolves as the fixture says", async () => {
  const f = JSON.parse(await Bun.file(file).text());
  for (const c of f.cases) {
    for (const [role, want] of Object.entries(c.expect)) {
      expect([c.name, role, roleValue(c.palette, role)]).toEqual([c.name, role, want]);
    }
  }
});
