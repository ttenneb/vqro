import { expect, test } from "bun:test";
import extension, { roleObservation } from "./pi-role-status";

test("Pi role observation is exact and unique, never final-provider attestation", () => {
  expect(roleObservation("dummy role", "prefix\ndummy role\nsuffix")).toBe("loaded-at-pi");
  expect(roleObservation("dummy role", "dummy role\ndummy role")).toBe("requested");
  expect(roleObservation("dummy role", "not loaded")).toBe("requested");
  expect(roleObservation(" ", " ")).toBe("requested");
  expect(roleObservation("x".repeat(2049), "x".repeat(2049))).toBe("requested");
  expect(roleObservation("dummy role", undefined)).toBe("requested");
});

test("Pi status uses public CLI metadata and stays off mixed Herdr/Vqro routing", async () => {
  const old = { ...process.env };
  try {
    Object.assign(process.env, { VQRO_ENV: "1", VQRO_SOCKET_PATH: "/private/vqro.sock", VQRO_BIN_PATH: "/private/vqro", VQRO_PANE_ID: "w1:p1" });
    delete process.env.HERDR_ENV;
    delete process.env.HERDR_SOCKET_PATH;
    delete process.env.HERDR_CLIENT_SOCKET_PATH;
    const calls: any[] = [];
    const handlers = new Map<string, Function>();
    const statuses: string[] = [];
    const pi: any = {
      registerFlag: () => {}, getFlag: () => "dummy role",
      on: (name: string, handler: Function) => handlers.set(name, handler),
      exec: async (binary: string, args: string[]) => { calls.push([binary, args]); return { code: 0 }; },
    };
    extension(pi);
    const ctx = { getSystemPrompt: () => "dummy role", ui: { setStatus: (_key: string, text: string) => statuses.push(text) } };
    handlers.get("session_start")!({}, ctx);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.length).toBe(1);
    expect(calls[0][0]).toBe("/private/vqro");
    expect(calls[0][1].slice(0, 5)).toEqual(["pane", "report-metadata", "w1:p1", "--source", "example.pi-role"]);
    expect(calls[0][1]).toContain("idle=role loaded at Pi hook");
    handlers.get("before_agent_start")!({ systemPrompt: "role removed" }, ctx);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls[1][1]).toContain("working=role requested");
    expect(statuses.at(-1)).toBe("role requested");
    process.env.HERDR_ENV = "1";
    const off = new Map<string, Function>();
    extension({ ...pi, on: (name: string, fn: Function) => off.set(name, fn) });
    off.get("session_start")!({}, ctx);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(calls.length).toBe(2);
  } finally {
    for (const key of Object.keys(process.env)) if (!(key in old)) delete process.env[key];
    Object.assign(process.env, old);
  }
});
