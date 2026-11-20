// `vqro ui opentui`: the OpenTUI trial client (D29). Started with VQRO_SOCKET by the vqro launcher.
import { createCliRenderer } from "@opentui/core";
import { App, Screen, Top } from "./app";

const socket = process.env.VQRO_SOCKET;
if (!socket) { console.error("VQRO_SOCKET is not set: start this client with `vqro ui opentui`"); process.exit(2); }
const renderer = await createCliRenderer({ exitOnCtrlC: false, useMouse: true });
const screen = new Screen(renderer, { width: "100%", height: "100%", position: "absolute", left: 0, top: 0, zIndex: 0 });
const top = new Top(renderer, { width: "100%", height: "100%", position: "absolute", left: 0, top: 0, zIndex: 10 });
renderer.root.add(screen);
renderer.root.add(top);
const app = new App(renderer, screen, socket);
screen.app = app;
top.app = app;
top.onMouse = (e) => app.onMouse(e);
renderer.keyInput.on("keypress", (k) => app.onKey(k));
renderer.keyInput.on("paste", (e) => app.onPaste(e));
renderer.on("resize", () => app.redraw());
try {
  await app.start();
} catch (e) {
  renderer.destroy();
  console.error(`vqro opentui: cannot connect: ${(e as Error).message}`);
  process.exit(1);
}
