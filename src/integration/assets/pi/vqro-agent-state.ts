// installed by vqro
// managed by vqro; reinstalling or updating the integration overwrites this file.
// add custom hooks/plugins beside this file instead of editing it.
// VQRO_INTEGRATION_ID=pi
// VQRO_INTEGRATION_VERSION=10
// @ts-nocheck

import net from "node:net";
import path from "node:path";

const VQRO_ENV = process.env.VQRO_ENV;
const socketPath = process.env.VQRO_SOCKET_PATH;
const socketEndpoint =
  process.platform === "win32" && socketPath ? `\\\\.\\pipe\\${socketPath}` : socketPath;
const paneId = process.env.VQRO_PANE_ID;
const source = "vqro:pi";

function enabled() {
  return VQRO_ENV === "1" && !!socketPath && !!paneId;
}

function sendRequestAttempt(request: unknown, timeoutMs: number): Promise<boolean> {
  if (!enabled()) {
    return Promise.resolve(true);
  }

  return new Promise((resolve) => {
    let done = false;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const finish = (delivered: boolean) => {
      if (done) return;
      done = true;
      if (timeout) {
        clearTimeout(timeout);
      }
      socket.destroy();
      resolve(delivered);
    };

    const socket = net.createConnection(socketEndpoint!);
    socket.on("error", () => finish(false));
    socket.on("connect", () => socket.write(`${JSON.stringify(request)}\n`));
    socket.on("data", () => finish(true));
    socket.on("end", () => finish(false));
    timeout = setTimeout(() => finish(false), timeoutMs);
    timeout.unref?.();
  });
}

async function sendRequest(request: unknown): Promise<void> {
  if (await sendRequestAttempt(request, 500)) {
    return;
  }
  await sendRequestAttempt(request, 1500);
}

type AgentState = "working" | "blocked" | "idle";

type QueuedState = {
  state: AgentState;
  message?: string;
  seq: number;
};

let reportSeq = Date.now() * 1000;
let currentAgentSessionId: string | undefined;
let currentAgentSessionPath: string | undefined;

function nextReportSeq(): number {
  reportSeq += 1;
  return reportSeq;
}

function updateSessionRef(ctx: any): void {
  try {
    const file = ctx?.sessionManager?.getSessionFile?.();
    currentAgentSessionPath =
      typeof file === "string" &&
      (path.posix.isAbsolute(file) || path.win32.isAbsolute(file))
        ? file
        : undefined;
  } catch {
    currentAgentSessionPath = undefined;
  }

  try {
    const id = ctx?.sessionManager?.getSessionId?.();
    currentAgentSessionId = typeof id === "string" && id.length > 0 ? id : undefined;
  } catch {
    currentAgentSessionId = undefined;
  }
}

function withSessionRef(params: Record<string, unknown>): Record<string, unknown> {
  if (currentAgentSessionPath) {
    return { ...params, agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { ...params, agent_session_id: currentAgentSessionId };
  }
  return params;
}

function currentSessionRef(): Record<string, unknown> | undefined {
  if (currentAgentSessionPath) {
    return { agent_session_path: currentAgentSessionPath };
  }
  if (currentAgentSessionId) {
    return { agent_session_id: currentAgentSessionId };
  }
  return undefined;
}

function reportSession(sessionStartSource?: string): Promise<void> {
  const sessionRef = currentSessionRef();
  if (!sessionRef) {
    return Promise.resolve();
  }

  return sendRequest({
    id: `${source}:session:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent_session",
    params: {
      pane_id: paneId,
      source,
      agent: "pi",
      seq: nextReportSeq(),
      session_start_source: sessionStartSource,
      ...sessionRef,
    },
  });
}

function sendState(state: AgentState, message?: string, seq = nextReportSeq()): Promise<void> {
  return sendRequest({
    id: `${source}:${Date.now()}:${Math.random().toString(36).slice(2)}`,
    method: "pane.report_agent",
    params: withSessionRef({
      pane_id: paneId,
      source,
      agent: "pi",
      state,
      message,
      seq,
    }),
  });
}

let sendInFlight = false;
let queuedState: QueuedState | undefined;

function queueState(state: AgentState, message?: string): void {
  queuedState = { state, message, seq: nextReportSeq() };
  if (!sendInFlight) {
    void drainStateQueue();
  }
}

async function drainStateQueue(): Promise<void> {
  if (sendInFlight) {
    return;
  }

  sendInFlight = true;
  try {
    while (queuedState) {
      const next = queuedState;
      queuedState = undefined;
      await sendState(next.state, next.message, next.seq);
    }
  } finally {
    sendInFlight = false;
    if (queuedState) {
      void drainStateQueue();
    }
  }
}

export default function (pi) {
  if (!enabled()) {
    return;
  }

  let agentActive = false;
  let blockedCount = 0;
  let blockedMessage: string | undefined;
  let lastState: AgentState | undefined;
  let lastMessage: string | undefined;
  let rootSession = false;
  let editorUi: any;
  let editorPoll: ReturnType<typeof setInterval> | undefined;
  let inputBoxHasText: boolean | undefined;

  // The host refuses to type into an editor that already holds the human's
  // unsent text, so it needs to know whether this one does. Only the fact is
  // reported (never the content), and only when it flips: one request per
  // change of emptiness, polled because Pi changes the editor on its own too
  // (a restored draft, a submit).
  function readInputBoxHasText(): boolean | undefined {
    try {
      const text = editorUi?.getEditorText?.();
      return typeof text === "string" ? text.trim().length > 0 : undefined;
    } catch {
      return undefined;
    }
  }

  function reportInputBox(hasText: boolean): void {
    void sendRequest({
      id: `${source}:input-box:${Date.now()}:${Math.random().toString(36).slice(2)}`,
      method: "pane.report_metadata",
      params: {
        pane_id: paneId,
        source,
        seq: nextReportSeq(),
        tokens: { input_box: hasText ? "has_text" : "empty" },
      },
    });
  }

  function checkInputBox(force = false): void {
    if (!rootSession) {
      return;
    }
    const next = readInputBoxHasText();
    if (next === undefined || (!force && next === inputBoxHasText)) {
      return;
    }
    inputBoxHasText = next;
    reportInputBox(next);
  }

  function watchEditor(ctx: any): void {
    if (editorPoll) {
      clearInterval(editorPoll);
    }
    editorUi = ctx?.ui;
    // Every tick re-reports, not only a flip: the host discards a report older
    // than the human's latest key, and a flip-only report would leave it on its
    // fallback heuristic after any keystroke until the next flip, which may
    // never come. One small request per second per pane is the whole cost.
    editorPoll = setInterval(() => {
      if (!rootSession) {
        return;
      }
      const next = readInputBoxHasText();
      if (next === undefined) {
        return;
      }
      inputBoxHasText = next;
      reportInputBox(next);
    }, 1000);
    editorPoll.unref?.();
  }

  function desiredState() {
    if (blockedCount > 0) {
      return { state: "blocked" as const, message: blockedMessage };
    }
    if (agentActive) {
      return { state: "working" as const, message: undefined };
    }
    return { state: "idle" as const, message: undefined };
  }

  function publishState(force = false) {
    const next = desiredState();
    if (!force && next.state === lastState && next.message === lastMessage) {
      return;
    }
    lastState = next.state;
    lastMessage = next.message;
    queueState(next.state, next.message);
  }

  pi.events.on("vqro:blocked", (data) => {
    if (!rootSession) {
      return;
    }
    if (!data?.active) {
      blockedCount = Math.max(0, blockedCount - 1);
      if (blockedCount === 0) {
        blockedMessage = undefined;
      }
      publishState();
      return;
    }

    blockedCount += 1;
    blockedMessage = data.label;
    publishState();
  });

  pi.on("session_start", async (event, ctx) => {
    // TUI only: RPC/JSON/print modes are headless (no PTY vqro can display),
    // and RPC still reports hasUI=true, so mode is the reliable gate.
    if (ctx?.mode !== "tui") {
      return;
    }
    rootSession = true;
    updateSessionRef(ctx);
    await reportSession(event?.reason);
    // A reload can replace this extension mid-run without emitting another agent_start.
    agentActive = ctx?.isIdle?.() === false;
    publishState(true);
    watchEditor(ctx);
    // Report the empty/typed baseline even when it has not flipped yet, so the
    // host knows this editor reports at all.
    checkInputBox(true);
  });

  pi.on("agent_start", (_event, ctx) => {
    if (!rootSession) {
      return;
    }
    updateSessionRef(ctx);
    void reportSession();
    agentActive = true;
    publishState();
    checkInputBox();
  });

  pi.on("agent_settled", (_event, ctx) => {
    if (!rootSession || ctx?.isIdle?.() !== true) {
      return;
    }

    agentActive = false;
    publishState();
    checkInputBox();
  });

  pi.on("session_shutdown", () => {
    // Local cleanup only, and no report: a shut-down session must stop
    // reporting whether the editor has text. The host drops the fact with the
    // agent process either way.
    rootSession = false;
    inputBoxHasText = undefined;
    editorUi = undefined;
    if (editorPoll) {
      clearInterval(editorPoll);
    }
    editorPoll = undefined;
  });
}
