// LF-delimited JSON-RPC over the core's opaque local endpoint (D22): a Unix socket path here. Never parsed; passed
// to the platform's connect call. Calls are bounded by a deadline; nothing is resent automatically (D10).

export type Json = any;
export type Notify = (method: string, params: Json) => void;

const MAX_RECORD = 16 * 1024 * 1024;

export class RpcError extends Error {
  constructor(message: string, public kind: string, public data?: Json) { super(message); }
}

export class Peer {
  private next = 1;
  private waiting = new Map<number, { resolve: (v: Json) => void; reject: (e: Error) => void; timer: Timer }>();
  private buffer = "";
  private socket: any;
  closed = false;
  onClose: () => void = () => {};

  private constructor(private notify: Notify) {}

  static async connect(path: string, notify: Notify): Promise<Peer> {
    const peer = new Peer(notify);
    const decoder = new TextDecoder();
    peer.socket = await Bun.connect({
      unix: path,
      socket: {
        data(_s, chunk) { peer.feed(decoder.decode(chunk, { stream: true })); },
        close() { peer.close(); },
        error() { peer.close(); },
      },
    });
    return peer;
  }

  private feed(text: string) {
    this.buffer += text;
    if (this.buffer.length > MAX_RECORD && !this.buffer.includes("\n")) { this.close(); return; }
    let at: number;
    while ((at = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, at);
      this.buffer = this.buffer.slice(at + 1);
      if (!line.trim()) continue;
      let msg: Json;
      try { msg = JSON.parse(line); } catch { continue; }
      if (msg.method !== undefined && msg.id === undefined) { this.notify(msg.method, msg.params ?? {}); continue; }
      if (msg.method !== undefined) {  // a request: clients serve none
        this.send({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "clients serve no requests", data: { kind: "not_found" } } });
        continue;
      }
      const w = this.waiting.get(msg.id);
      if (!w) continue;
      this.waiting.delete(msg.id);
      clearTimeout(w.timer);
      if (msg.error) w.reject(new RpcError(msg.error.message ?? "error", msg.error.data?.kind ?? "error", msg.error.data));
      else w.resolve(msg.result);
    }
  }

  private send(msg: Json) {
    if (this.closed) throw new RpcError("disconnected", "disconnected");
    this.socket.write(JSON.stringify(msg) + "\n");
  }

  call(method: string, params: Json = {}, timeoutMs = 60_000): Promise<Json> {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id);
        reject(new RpcError("response deadline; outcome indeterminate, never resent", "timeout"));
      }, timeoutMs);
      this.waiting.set(id, { resolve, reject, timer });
      try { this.send({ jsonrpc: "2.0", id, method, params }); } catch (e) { clearTimeout(timer); this.waiting.delete(id); reject(e); }
    });
  }

  /** A notification (no reply): stream input, credit, resize, close. */
  tell(method: string, params: Json) {
    if (!this.closed) this.send({ jsonrpc: "2.0", method, params });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    try { this.socket?.end(); } catch {}
    for (const [, w] of this.waiting) { clearTimeout(w.timer); w.reject(new RpcError("disconnected", "disconnected")); }
    this.waiting.clear();
    this.onClose();
  }
}
