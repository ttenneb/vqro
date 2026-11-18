"""Fake public extension/core surface; never starts a program or connects a socket."""
import copy
import threading
from prim_ext import PrimError

OPS = ("prompt", "steer", "follow_up", "abort", "clear_queue", "answer")


class FakeCore:
    def __init__(self, name="agents"):
        self.name = name
        self.cache_lock = threading.RLock()
        self.entries, self.resources, self.handlers, self.live = {}, {}, {}, {}
        self.providers = {"other": {"name": "other", "capabilities": {"agent."+op: {} for op in OPS}}}
        self.calls, self.notices, self.transactions = [], [], []
        self.prim = self
        self.fail_agents, self.fail_tabs, self.fail_effect, self.make_agent = False, False, None, True
        self.contributed = False

    def fresh(self):
        pass

    def version(self, key):
        return self.entries.get(key, {}).get("version", 0)

    def value(self, key, default=None):
        return copy.deepcopy(self.entries.get(key, {}).get("value", default))

    def apply(self, ops, note):
        for op in ops:
            if "expect" in op and op["expect"] != self.version(op["key"]):
                raise PrimError("changed", "conflict")
        self.transactions.append(copy.deepcopy(ops))
        for op in ops:
            if op["op"] == "delete":
                self.entries.pop(op["key"], None)
            else:
                self.entries[op["key"]] = {"value": copy.deepcopy(op["value"]), "version": self.version(op["key"])+1}
        return {}

    def contribute(self, entries):
        wanted = {k: dict(v, owner=self.name) for k, v in entries.items()}
        ops = [{"op": "set", "key": k, "value": v} for k, v in wanted.items() if self.value(k) != v]
        if not self.contributed:
            ops += [{"op": "delete", "key": k} for k, e in self.entries.items()
                    if k.startswith(("menu/", "binding/", "view/")) and e["value"].get("owner") == self.name and k not in wanted]
        self.contributed = True
        if ops:
            self.apply(ops, "contribute")

    def retrying(self, fn):
        return fn()

    def command(self, name, title):
        def register(fn):
            self.handlers[name] = fn
            return fn
        return register

    def on_event(self, fn):
        self.listener = fn

    def notify(self, text, style=None):
        self.notices.append((text, style))

    def publish(self, view, content):
        self.live[view] = copy.deepcopy(content)
        return {}

    def invoke(self, name, args=None):
        self.calls.append((name, copy.deepcopy(args)))
        if name == "agents.show" and self.fail_agents or name == "tabs.show" and self.fail_tabs:
            raise PrimError("extension stopped", "unavailable")
        return {"ok": True}

    def add_agent(self):
        self.resources["r2"] = {"id": "r2", "provider": "other", "name": "Other agent", "kind": "agent",
                                "state": "live", "available": True, "host": "r1", "data": {"requests": []},
                                "status": {"state": "idle"}}

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "resource.spawn":
            if self.make_agent:
                self.add_agent()
            return {"outcome": "observed", "resource": {"id": "r1"}}
        if self.fail_effect:
            raise PrimError("native outcome unknown", self.fail_effect)
        return {"outcome": "dispatched" if params["op"] == "agent.answer" else "accepted", "effect": "e1"}
