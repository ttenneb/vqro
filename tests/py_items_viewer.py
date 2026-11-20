#!/usr/bin/env python3
"""The Python twin of `vqro-items-viewer` for the item-stream gate (tests/e2e_item_streams.py --py-viewer).

One client connection through the stdlib SDK (`extensions/vqro_ext.py` stream routing) that attaches the item
stream of every agent of a provider and reconstructs each with `extensions/vqro_items.py`'s ItemView. A gap or
close reattaches from the view's `after`; attaches are windowed (at most 32 in flight). Writes the same `--out`
JSON as the Rust viewer (counters, per-agent epoch/seq/digest), so the gate compares both against the runtime.
Usage: py_items_viewer.py --name p1 --provider items --seconds N --stop-file F --out OUT.json
"""
import concurrent.futures
import json
import os
import pathlib
import queue
import sys
import threading
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from vqro_ext import Vqro, VqroError  # noqa: E402
from vqro_items import ItemView, ERRORS  # noqa: E402


def arg(name, default):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def digest(view):
    """FNV-1a over `kind/id:rev:value;` in key order, as the Rust SDK's `digest` (serde_json compact, sorted keys)."""
    h = 0xcbf29ce484222325
    for (kind, eid), (rev, value) in sorted(view.entities.items()):
        text = json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        for b in f"{kind}/{eid}:{rev}:{text};".encode():
            h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


class Agent:
    """One agent's view; its sink runs on the SDK reader thread and only feeds bytes or queues a reattach."""

    def __init__(self, rid, counters, reattach):
        self.rid, self.counters, self.reattach = rid, counters, reattach
        self.view, self.stream, self.lock = ItemView(), None, threading.Lock()

    def data(self, data):
        with self.lock:
            self.view.feed(data)
            self.counters["bytes"] += len(data)

    def error(self, error):
        pass

    def ended(self, kind, reason):
        reason = reason if isinstance(reason, dict) else {}
        self.counters["gaps" if kind == "gap" else "closed"] += 1
        self.counters["closed_slow"] += reason.get("reason") == "slow_consumer"
        self.counters["gaps_retention"] += reason.get("gap") == "retention" or reason.get("reason") == "retention"
        with self.lock:
            self.view.stream_ended()  # a partial line or unfinished snapshot never carries onto a new stream
            self.stream = None
        self.counters["reattach"] += 1
        self.reattach.put(self.rid)


def main():
    name, provider = arg("--name", "p1"), arg("--provider", "items")
    seconds, stop_file, out = float(arg("--seconds", "60")), arg("--stop-file", ""), arg("--out", "")
    vqro = Vqro(os.environ["VQRO_SOCKET"], lambda method, params: None)
    vqro.call("hello", {"client": name})
    resources = vqro.call("resource.list", {})["resources"]
    counters = dict.fromkeys(("bytes", "opens_reset", "opens_continue", "gaps", "closed", "closed_slow",
                              "gaps_retention", "reattach", "attach_errors", "rpc_bad_json"), 0)
    todo = queue.Queue()
    agents = {r["id"]: Agent(r["id"], counters, todo) for r in resources
              if r.get("kind") == "agent" and r.get("provider") == provider and r.get("available") is not False}
    for rid in agents:
        todo.put(rid)

    def registered(agent, reply):  # on the reader thread, before any of the stream's data is read
        if "stream" in reply:
            agent.stream = reply["stream"]
            vqro.register_stream(reply["stream"], agent)

    def attach(rid):
        agent = agents[rid]
        with agent.lock:
            after = agent.view.after()
        try:
            reply = vqro.call("resource.attach", {"resource": rid, "items": {"after": after} if after else {}},
                              on_result=lambda r: registered(agent, r))
            counters["opens_reset" if reply.get("initial") == "reset" else "opens_continue"] += 1
        except VqroError:
            counters["attach_errors"] += 1
            threading.Timer(0.5, todo.put, (rid,)).start()  # retry later, never in a burst

    deadline = time.monotonic() + seconds
    with concurrent.futures.ThreadPoolExecutor(32) as pool:  # windowed: under the core's 128 requests/connection
        while time.monotonic() < deadline and not (stop_file and os.path.exists(stop_file)):
            try:
                pool.submit(attach, todo.get(timeout=0.1))
            except queue.Empty:
                pass
    errors = dict.fromkeys(ERRORS, 0)
    views = {}
    for rid, agent in agents.items():
        with agent.lock:
            for k, v in agent.view.errors.items():
                errors[k] += v
            views[rid] = {"epoch": agent.view.epoch, "seq": agent.view.seq, "digest": digest(agent.view),
                          "entities": len(agent.view.entities), "attached": agent.stream is not None}
    result = {"viewer": name, "counters": {**counters, **errors}, "agents": views}
    print(json.dumps({"viewer": name, "final": True, "counters": result["counters"]}), flush=True)
    if out:
        pathlib.Path(out).write_text(json.dumps(result, indent=1))
    os._exit(0)  # the SDK reader thread would otherwise outlive us


if __name__ == "__main__":
    main()
