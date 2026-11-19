"""Default replaceable coordination-v0 extension (stdlib, public primitives only).

No terminal typing, native replay, runtime supervision, or workspace persistence
fallback. Missing C1/C2 support is visible and fails closed; raw recovery survives.
"""
import copy
import threading

from vqro_ext import Extension, VqroError
from coordination_store import OperationalStore, StoreError
from coordination_engine import Coordinator, digest, SCOPE_CAPACITY, WINDOW
from coordination_policy import PolicyError, bounded_page, QuotaLimits
from dataclasses import asdict

COMMANDS = (
    "inbox.window", "inbox.send", "inbox.edit", "inbox.withdraw", "inbox.take", "inbox.report",
    "inbox.reconcile", "inbox.peek", "inbox.resolve", "inbox.list", "inbox.get", "inbox.op",
    "inbox.archive", "handover", "scope", "control", "names", "grants", "membership.set", "status",
)


def callback_context(params):
    """Only core callback attribution, never fields in params.args or a label."""
    principal, actor, author = params.get("principal"), params.get("actor"), params.get("author")
    if not isinstance(principal, str) or not isinstance(actor, str) or not isinstance(author, str):
        return {"principal": "undeclared", "actor": "undeclared", "author": params.get("caller"), "kind": "tool"}
    return {"principal": principal, "actor": actor, "author": author,
            "initiator": params.get("initiator"), "label": params.get("label"), "kind": "tool"}


class CoordinationService:
    def __init__(self, ext):
        self.ext = ext
        self.engine = None
        self.unavailable = None
        self.read_only_rows = None
        self.pending_wakes = {}
        self.wake_lock = threading.Lock()
        self.wake_seq = 0
        self.event = threading.Event()
        self.old_handler = ext.vqro.handler
        ext.vqro.handler = self.handle
        self.store = OperationalStore(self.rpc)
        try:
            self.read_only_rows = self.store.snapshot()
        except StoreError:
            pass  # No invented empty state when a domain is absent/unreadable.
        try:
            self.store.register()
            self.engine = Coordinator(self.store, config=self.config, resources=self.resources,
                                      providers=self.providers, core_epoch=ext.epoch,
                                      invoke=self.invoke, config_apply=self.config_apply, wake=self.wake)
        except (StoreError, PolicyError, VqroError) as error:
            # No fallback authorizes old ordinary documents as new execution state.
            self.unavailable = {"kind": getattr(error, "kind", "unavailable"), "message": str(error)[:1024]}
            ext.log("coordination unavailable (no input invoked):", self.unavailable)
        for name in COMMANDS:
            ext.command("coord." + name, "Coordination: " + name)(lambda args: None)
        ext.on_event(self.on_event)

    def rpc(self, method, params=None):
        params = dict(params or {})
        context = getattr(self.ext.vqro, "_command_context", None)
        proof = getattr(context, "invocation", None)
        if proof is not None and method in ("operational.apply", "operational.register", "workspace.apply", "resource.invoke", "command.invoke"):
            params["invocation"] = proof
            # Compatible with the pre-C2 SDK: do not also inject the old field
            # on_behalf_of=<proof>, which C2 interprets as a principal delegation.
            context.invocation = None
        try:
            return self.ext.vqro.call(method, params)
        finally:
            if context is not None:
                context.invocation = proof

    def config(self):
        with self.ext.cache_lock:
            return copy.deepcopy(self.ext.entries)

    def resources(self):
        with self.ext.cache_lock:
            resources = copy.deepcopy(self.ext.resources)
            entries = copy.deepcopy(self.ext.entries)
        for key, row in entries.items():
            if key.startswith("doc/agents/terminals/"):
                agent = key[len("doc/agents/terminals/"):]
                if agent in resources and isinstance(row.get("value"), dict):
                    marker = row["value"]
                    native = resources[agent].setdefault("data", {}).setdefault("agent", {})
                    if "pull" in marker:
                        native.setdefault("pull", marker["pull"])
        return resources

    def providers(self):
        with self.ext.cache_lock:
            return copy.deepcopy(self.ext.providers)

    def invoke(self, target, operation, args, deadline_ms):
        return self.rpc("resource.invoke", {"resource": target, "op": operation, "args": args, "deadline_ms": deadline_ms})

    def config_apply(self, ops, context):
        # The live core callback proof supplies attribution; the effective principal
        # in `context` is recorded evidence, not a body-selected delegation.
        reply = self.rpc("workspace.apply", {"ops": ops, "note": "coord: configuration"})
        self.ext.fresh()
        return reply

    def wake(self, agent, count):
        with self.wake_lock:
            self.pending_wakes[agent] = count
            self.wake_seq += 1
        self.event.set()

    def on_event(self, event_type, data):
        if event_type in ("resource", "resource.removed", "entries", "provider", "missed", "operational"):
            self.event.set()

    def handle(self, method, params):
        if method != "command.invoke" or not str(params.get("name", "")).startswith("coord."):
            return self.old_handler(method, params)
        command = params["name"][len("coord."):]
        if command not in COMMANDS:
            raise VqroError("unknown coordination command", "not_found")
        args = params.get("args") or {}
        if not isinstance(args, dict):
            raise VqroError("coordination arguments must be an object", "invalid")
        if command == "status":
            return {"state": "unavailable" if self.engine is None else "ready", "reason": self.unavailable,
                    "physical_suspension": False, "window": self.engine.window() if self.engine else None,
                    "recovery_readable": self.read_only_rows is not None,
                    "limits": {"scope_targets": SCOPE_CAPACITY, "page_items": 64, "take_items": 16,
                               "page_bytes": 262144, "operation_window": WINDOW,
                               "domain_bytes": self.store.byte_limit, "domain_records": self.store.record_limit,
                               "control_reserve_percent": 10,
                               "quotas": asdict(QuotaLimits(domain_bytes=self.store.byte_limit, scope_capacity=SCOPE_CAPACITY))},
                    "identity_cache": {"owner": "core", "capacity": None, "incomplete": True}}
        if self.engine is None and command in ("inbox.get", "inbox.list", "inbox.peek") and self.read_only_rows is not None:
            return self.read_only(command, args)
        if self.engine is None:
            raise VqroError("coordination unavailable; no input was invoked: " + str(self.unavailable), "unavailable")
        context = callback_context(params)
        # Derive endpoint origin only from an explicit principal association in core
        # facts/configuration. Shared hosts are never guessed to be the first child.
        matches = []
        for agent, resource in self.resources().items():
            native = (resource.get("data") or {}).get("agent") or {}
            if native.get("principal") == context["principal"]:
                matches.append(agent)
        if len(matches) == 1:
            context.update(agent=matches[0], kind="agent")
        return self.route(command, args, context)

    def read_only(self, command, args):
        records = []
        for key, row in self.read_only_rows.items():
            if not key.startswith("message/"):
                continue
            record = copy.deepcopy(row["value"])
            record.update(read_only=True, stale=True, revision=row["version"])
            if record["state"] == "claimed":
                record.update(state="unknown", reason="owner_unavailable")
            elif record["state"] in ("held", "recorded"):
                record.update(state="held", reason="paused")
            records.append(record)
        if command == "inbox.get":
            return next((record for record in records if record["delivery"] == args.get("delivery")),
                        {"state": "unknown", "read_only": True})
        if args.get("target"):
            records = [record for record in records if record["to"]["agent"] == args["target"].removeprefix("agent:")]
        if args.get("sender"):
            records = [record for record in records if record["from"]["principal"] == args["sender"]]
        return bounded_page(records, page=args.get("page"), revision=digest(self.read_only_rows), key=lambda record: (record["order"], record["delivery"]))

    def route(self, command, args, context):
        engine = self.engine
        if command == "inbox.window":
            return engine.window()
        if command == "inbox.op":
            return engine.operation(context["principal"], args.get("op"))
        if command == "inbox.get":
            return engine.get(args.get("delivery"))
        if command in ("inbox.peek", "inbox.list"):
            return engine.listing(args)
        if command == "scope" and args.get("action") in ("inspect", "preview"):
            return engine.scope_preview(args.get("scope", "workspace"))
        if command == "scope" and args.get("action") == "result":
            return engine.scope_result(args.get("receipt"), args.get("page"))
        if command == "inbox.archive":
            return engine.archive(args, context)
        actions = {"inbox.send": engine.send, "inbox.edit": engine.edit, "inbox.take": engine.take,
                   "inbox.report": engine.report, "inbox.reconcile": engine.reconcile, "inbox.resolve": engine.resolve,
                   "handover": engine.handover, "scope": engine.scope, "control": engine.control}
        if command == "inbox.withdraw":
            result = engine.edit(args, context, withdraw=True)
        elif command in ("names", "grants", "membership.set"):
            result = engine.config_action("membership" if command == "membership.set" else command, args, context)
        else:
            result = actions[command](args, context)
        self.event.set()
        return result

    def work(self):
        while True:
            self.event.wait(1.0)
            self.event.clear()
            if self.engine is None:
                continue
            try:
                self.engine.lifecycle_sweep(forget=self.forget)
                for _ in range(16):
                    if self.engine.dispatch_one() is None:
                        break
                else:
                    self.event.set()  # bounded batch, then yield to controls/observations
                with self.wake_lock:
                    pending, self.pending_wakes = self.pending_wakes, {}
                    sequence = self.wake_seq
                with self.engine.lock:
                    pending = {agent: count for agent, count in pending.items() if agent in self.engine.permits}
                    control = self.store.get("control")["value"] if pending else None
                if pending:
                    # A count hint is fenced by its epoch/revision, never authority
                    # to wake/insert; the plugin must still make a gated take.
                    self.ext.publish("coord-status", {"wake": {"seq": sequence, "pending": pending},
                                                      "control_epoch": self.engine.control_epoch,
                                                      "control_revision": control["revision"],
                                                      "physical_suspension": False})
            except (StoreError, PolicyError, VqroError) as error:
                self.ext.log("coordination attention:", getattr(error, "kind", "unknown"), str(error)[:512])

    def forget(self, agent, incarnation):
        # C4 is cache-only. A failed/missing API never becomes terminal.kill.
        try:
            self.rpc("resource.forget_cached", {"resource": agent, "incarnation": incarnation})
        except VqroError as error:
            self.ext.log("unavailable cache retained:", agent, error.kind)

    def run(self):
        self.ext.contribute({"view/coord-status": {"kind": "document", "title": "Coordination"}})
        threading.Thread(target=self.work, daemon=True, name="coord-dispatch").start()
        self.ext.run()


def main():
    ext = Extension("coord", event_types=["entries", "resource", "resource.removed", "provider", "missed", "operational"])
    CoordinationService(ext).run()


if __name__ == "__main__":
    main()
