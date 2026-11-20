"""Default replaceable coordination-v0 extension (stdlib, public primitives only).

No terminal typing, native replay, runtime supervision, or workspace persistence
fallback. Missing C1/C2 support is visible and fails closed; raw recovery survives.
"""
import copy
from collections import OrderedDict
import json
import threading
import time
from concurrent.futures import ThreadPoolExecutor

from vqro_ext import Extension, VqroError
from coordination_store import OperationalStore, StoreError
from coordination_engine import (Coordinator, digest, refusal_receipt, SCOPE_CAPACITY, WINDOW,
                                 CONTROL_ADMISSION_BYTES, CONTROL_ADMISSION_RECORDS, ARCHIVE_FUTURE_BYTES,
                                 ARCHIVE_ESCROW_BYTES, ARCHIVE_ESCROW_RECORDS, IDENTITY_SUMMARY_BYTES)
from coordination_policy import PolicyError, bounded_page, QuotaLimits
from coordination_streams import ItemReceiptStreams, ItemReceiptSink
from coordination_requests import pending_requests, request_key, native_facts
from agent_request_routes import request_identity_key
from dataclasses import asdict

COMMANDS = (
    "inbox.window", "inbox.send", "inbox.edit", "inbox.withdraw", "inbox.take", "inbox.report",
    "inbox.reconcile", "inbox.peek", "inbox.resolve", "inbox.list", "inbox.get", "inbox.op",
    "inbox.archive", "handover", "scope", "control", "names", "grants", "membership.set", "request_route", "status",
)


def callback_context(params):
    """Only core callback attribution, never fields in params.args or a label."""
    principal, actor, author = params.get("principal"), params.get("actor"), params.get("author")
    if not isinstance(principal, str) or not isinstance(actor, str) or not isinstance(author, str):
        return {"principal": "undeclared", "actor": "undeclared", "author": params.get("caller"), "kind": "tool"}
    return {"principal": principal, "actor": actor, "author": author,
            "initiator": params.get("initiator"), "label": params.get("label"), "kind": "tool"}


# Coordinator-approved historical measured envelope, not a native runtime deadline.
CONTROL_LATENCY_MS = 3000
CONTROL_LATENCY_CONDITIONS = {
    "kind": "conditional_measured_envelope", "historical": True,
    "runtime_guarantee": False, "current_release_verified": False, "verified_on_measured_pin_and_load": True,
    "rounding": "next whole second above observed maxima, including earlier 2724.1 ms; load-dependent",
    "measurement": {
        "case": "S1", "runs": ["b-6cqcuh5h", "b-uhym7ktz"], "core": "ac140ac7", "coord": "ac140ac7",
        "core_sha256": "d63096a959ba7288b83e7b256aae775af191d0364dd2aa6d2d262ce15fb407d4",
        "agents": 200, "terminals": 600, "streams": 600, "stalled_streams": 200,
        "measurement_phase_seconds": [82, 100], "cpu_count": 8, "nice": 10,
        "load_average": [7.5, 9.2], "native": "immediate fake provider; no model/runtime",
        "load": "steady inbox/layout traffic after three-viewer phase; full S1 burst/flood/reconnect also passed",
        "samples": {"interrupt": 80, "respond": 80}, "samples_per_run_per_kind": 40,
        "max_ms": {"interrupt": 2378.1, "respond": 1914.2},
        "boundary": "client command.invoke coord.control reply; op-id read and next-request native_set excluded",
        "harness": "ac140ac7 tests plus local uncommitted measurement phase; raw samples retained",
        "limitations": "shared-host load-dependent measured envelope, not a future SLO or runtime completion guarantee",
    },
    "earlier_s1_observation": {"core": "e026f6a", "coord": "a8968e9", "run": "b-hbof3nye",
                               "samples": {"interrupt": 3, "respond": 3}, "load_average": [15, 28],
                               "max_ms": {"interrupt": 2244.6, "respond": 2724.1}},
}


class CoordinationService:
    def __init__(self, ext, *, defer_bootstrap=False):
        self.ext = ext
        self.engine = None
        self.unavailable = None
        self.read_only_rows = None
        self.bootstrap_state = "pending"
        self.bootstrap_done = threading.Event()
        self._bootstrap_lock = threading.Lock()
        self._bootstrap_started = False
        self._bootstrap_thread = None
        self._public_registered = False
        self._defer_bootstrap = defer_bootstrap
        self.pending_wakes = {}
        self.wake_lock = threading.Lock()
        self.wake_seq = 0
        self.event = threading.Event()
        self.inventory_dirty = True
        self.dispatch_dirty = True
        self.request_cursor = 0
        self.request_views = set()
        self.next_request_scan = 0.0
        self.request_error = None
        self.request_baselines = OrderedDict()
        self.pool = ThreadPoolExecutor(max_workers=8, thread_name_prefix="coord-native")
        self.jobs = set()
        self.job_lock = threading.Lock()
        self.items = ItemReceiptStreams(self.rpc, self.receipts, wake=self.event.set)
        self.old_handler = ext.vqro.handler
        ext.vqro.handler = self.handle
        self.store = OperationalStore(self.rpc)
        # Install callbacks/listeners before subscription or any boot I/O. The
        # default synchronous path remains for existing component callers only;
        # production main defers until register() has received every public ACK.
        for name in COMMANDS:
            ext.command("coord." + name, "Coordination: " + name)(lambda args: None)
        ext.on_event(self.on_event)
        if not defer_bootstrap:
            self._bootstrap_started = True
            self._bootstrap()

    def register(self):
        """The registration/subscription half of Extension.run(), without blocking.

        The SDK currently has no register-only method. Keep its public sequence
        here so startup mutation cannot block the only thread that registers
        recovery commands. Notification callbacks are already installed above.
        """
        if self._public_registered:
            return
        for name, title in self.ext.titles.items():
            self.rpc("command.register", {"name": name, "title": title})
        sub = {"from": self.ext.seq, "epoch": self.ext.epoch, "partial_resources": True}
        if self.ext.event_types is not None:
            sub["types"] = list(self.ext.event_types)
        if self.ext.view_content is not None:
            sub["view_content"] = self.ext.view_content
        try:
            self.rpc("events.subscribe", sub)
        except VqroError as error:
            if error.kind != "conflict":
                raise
        self.ext.vqro.patient = False
        self._public_registered = True

    def start_bootstrap(self):
        """Start exactly one autonomous boot, never inside a command's proof scope."""
        with self._bootstrap_lock:
            if self._bootstrap_started:
                return self._bootstrap_thread
            if self._defer_bootstrap and not self._public_registered:
                raise VqroError("register coordination commands before bootstrap", "conflict")
            self._bootstrap_started = True
            self._bootstrap_thread = threading.Thread(target=self._bootstrap, daemon=True, name="coord-bootstrap")
            self._bootstrap_thread.start()
            return self._bootstrap_thread

    def _bootstrap(self):
        self.bootstrap_state = "reading"
        try:
            try:
                self.read_only_rows = self.store.snapshot()
            except StoreError:
                pass  # Unreadable/absent is unknown, not an invented empty queue.
            self.bootstrap_state = "registering"
            self.store.register()
            self.bootstrap_state = "recovering"
            engine = Coordinator(self.store, config=self.config, resources=self.resources,
                                 providers=self.providers, core_epoch=self.ext.epoch,
                                 invoke=self.invoke, config_apply=self.config_apply, wake=self.wake)
            # Do not expose a partially recovered engine or any of its permits.
            # Every Coordinator boot write has confirmed before this publication.
            engine.notice_principal = self.ext.info.get("principal")
            engine.notice_fence_capacity = self.identity_cache()['capacity']
            self.engine = engine
            self.bootstrap_state = "ready"
            self.inventory_dirty = self.dispatch_dirty = True
            self.event.set()
        except Exception as error:
            self.unavailable = {"kind": getattr(error, "kind", None) or "unavailable", "message": str(error)[:1024]}
            self.bootstrap_state = "failed"
            self.ext.log("coordination unavailable (no input invoked):", self.unavailable)
        finally:
            self.bootstrap_done.set()

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
            if method == "resource.attach" and "items" in params:
                sink = ItemReceiptSink(self.items)
                def attached(reply):
                    stream = reply.get("stream")
                    if type(stream) is int or isinstance(stream, str):
                        sink.stream = stream
                        self.ext.vqro.register_stream(stream, sink)
                # Stream notes bypass the RPC handler in the SDK. Register on
                # the reader thread before releasing/replaying any early bytes.
                return self.ext.vqro.call(method, params, on_result=attached)
            if method == "stream.close":
                self.ext.close_stream(params["stream"])
                return {}
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
        if operation == "agent.send":
            args = copy.deepcopy(args)
            origin = args.setdefault("origin", {})
            # Autonomous native workers use this connection, not a borrowed
            # callback principal. The core's hello result is authoritative.
            identity = getattr(self.ext, "info", {})
            origin.update(principal=identity.get("principal") or "undeclared",
                          actor=identity.get("actor") or "undeclared", kind="tool")
            origin.pop("agent", None)
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
        self.dispatch_dirty = True
        self.event.set()

    def on_event(self, event_type, data):
        if event_type == "missed":
            self.items.gap()
        if event_type in ("resource", "resource.removed", "entries", "provider", "missed"):
            self.inventory_dirty = True
            self.dispatch_dirty = True
            self.event.set()

    def receipts(self, agent, incarnation, events):
        if self.engine and self.engine.observe_receipts(agent, incarnation, events):
            self.dispatch_dirty = True
            self.event.set()

    def _job_finished(self, job):
        with self.job_lock:
            self.jobs.discard(job)
        try:
            job.result()
        except Exception as error:
            self.ext.log("native attempt needs attention:", getattr(error, "kind", "unknown"), str(error)[:512])
        self.dispatch_dirty = True
        self.event.set()

    def schedule(self):
        while True:
            with self.job_lock:
                if len(self.jobs) >= 8:
                    return
            record = self.engine.claim_one()
            if record is None:
                return
            job = self.pool.submit(self.engine.execute_claim, record)
            with self.job_lock:
                self.jobs.add(job)
            job.add_done_callback(self._job_finished)

    def identity_cache(self):
        """Project limits from the SDK's current-epoch public snapshot, never guess."""
        result = {"owner": "core", "capacity": None, "incomplete": True,
                  "source": "workspace.snapshot.identity_reservations"}
        with self.ext.cache_lock:
            header = getattr(self.ext, "snapshot_header", {})
            if not isinstance(header, dict) or header.get("epoch") != self.ext.epoch:
                return result
            stats = header.get("identity_reservations")
            if not isinstance(stats, dict):
                return result
            capacity, entry_bytes = stats.get("max_entries"), stats.get("max_entry_bytes")
            if type(capacity) is not int or capacity < 0 or type(entry_bytes) is not int or entry_bytes <= 0:
                return result
            return dict(result, capacity=capacity, max_entry_bytes=entry_bytes, incomplete=False,
                        source_epoch=header["epoch"], source_seq=header.get("seq"))

    def handle(self, method, params):
        if self.items.handle(method, params):
            return None
        if method != "command.invoke" or not str(params.get("name", "")).startswith("coord."):
            return self.old_handler(method, params)
        command = params["name"][len("coord."):]
        if command not in COMMANDS:
            raise VqroError("unknown coordination command", "not_found")
        args = params.get("args") or {}
        if not isinstance(args, dict):
            raise VqroError("coordination arguments must be an object", "invalid")
        if command == "status":
            return {"state": "ready" if self.engine and self.engine.current else "unavailable" if self.bootstrap_done.is_set() else "initializing",
                    "reason": self.unavailable or ("owner fenced" if self.engine and not self.engine.current else None), "bootstrap": self.bootstrap_state,
                    "physical_suspension": False, "window": self.engine.window() if self.engine else None,
                    "recovery_readable": self.read_only_rows is not None,
                    "limits": {"scope_targets": SCOPE_CAPACITY, "page_items": 64, "take_items": 16,
                               "page_bytes": 262144, "operation_window": WINDOW,
                               "domain_bytes": self.store.byte_limit, "domain_records": self.store.record_limit,
                               "control_reserve_percent": 10, "identity_summary_bytes": IDENTITY_SUMMARY_BYTES,
                               "normal_control_headroom": {"bytes": CONTROL_ADMISSION_BYTES, "records": CONTROL_ADMISSION_RECORDS},
                               "archive_escrow": {"bytes": ARCHIVE_ESCROW_BYTES, "records": ARCHIVE_ESCROW_RECORDS, "within_control_reserve": True},
                               "archive_plan_future": {"bytes": ARCHIVE_FUTURE_BYTES, "records": 4,
                                                       "exported_bytes": ARCHIVE_FUTURE_BYTES // 2, "exported_records": 2},
                               "quotas": asdict(QuotaLimits(domain_bytes=self.store.byte_limit, scope_capacity=SCOPE_CAPACITY))},
                    "identity_cache": self.identity_cache(),
                    "control_latency_ms": CONTROL_LATENCY_MS,
                    "control_latency_conditions": copy.deepcopy(CONTROL_LATENCY_CONDITIONS),
                    "receipts": self.items.status(), "request_routing": {"error": self.request_error},
                    "maintenance": self.engine.maintenance_error if self.engine else None,
                    "limits_confirmed": self.engine is not None, "configuration": self.engine.configuration_status() if self.engine else None,
                    "attention": self.engine.attention_summary() if self.engine else None}
        if self.engine is None and command in ("inbox.get", "inbox.list", "inbox.peek", "inbox.op") and self.read_only_rows is not None:
            return self.read_only(command, args, principal=callback_context(params)["principal"])
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

    def read_only(self, command, args, *, principal="undeclared"):
        if command == "inbox.op":
            identity = digest([principal, args.get("op")])
            external = self.read_only_rows.get("external/" + identity)
            if external:
                result = copy.deepcopy(external["value"])
                if result.get("state") == "claimed":
                    result.update(state="unknown", reason="owner_unavailable")
                return dict(result, read_only=True, stale=True, historical=True)
            for key, row in self.read_only_rows.items():
                if key.startswith("ops/") and identity in row["value"]:
                    result = copy.deepcopy(row["value"][identity]["result"])
                    if isinstance(result.get("refusal"), str):
                        result = refusal_receipt(result["refusal"])
                    if result.get("delivery"):
                        return self.read_only("inbox.get", {"delivery": result["delivery"]}, principal=principal)
                    if result.get("state") == "claimed":
                        result.update(state="unknown", reason="owner_unavailable")
                    # A cached take is history only: never return content for insertion.
                    return dict(result, records=[], read_only=True, stale=True, historical=True, incomplete=True)
            return {"state": "unknown", "read_only": True, "stale": True, "incomplete": True}
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
        heads = Coordinator._ordering_heads(records)
        records = [Coordinator._visible_ordering(record, heads) for record in records]
        if command == "inbox.get":
            return next((record for record in records if record["delivery"] == args.get("delivery")),
                        {"state": "unknown", "read_only": True})
        if args.get("target"):
            records = [record for record in records if record["to"]["agent"] == args["target"].removeprefix("agent:")]
        if args.get("sender"):
            records = [record for record in records if Coordinator._sender_matches(record, args["sender"])]
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
        if command == "control" and args.get("action") == "inspect":
            return engine.control(args, context)
        if command == "request_route" and args.get("action", "inspect") == "inspect":
            policy = engine.policy()
            return engine.request_route(policy.resolve_target(args.get("agent")), policy)
        if command == "scope" and args.get("action") == "result":
            return engine.scope_result(args.get("receipt"), args.get("page"))
        if command == "inbox.archive":
            return engine.archive(args, context)
        actions = {"inbox.send": engine.send, "inbox.edit": engine.edit, "inbox.take": engine.take,
                   "inbox.report": engine.report, "inbox.reconcile": engine.reconcile, "inbox.resolve": engine.resolve,
                   "handover": engine.handover, "scope": engine.scope, "control": engine.control}
        if command == "inbox.withdraw":
            result = engine.edit(args, context, withdraw=True)
        elif command in ("names", "grants", "membership.set", "request_route"):
            if command == 'request_route' and args.get('action') == 'initialize':
                self.ext.fresh()  # observed spawn must not race this SDK's resource notification cache
            result = engine.config_action("membership" if command == "membership.set" else command, args, context)
        else:
            result = actions[command](args, context)
        self.dispatch_dirty = True
        self.event.set()
        return result

    def route_requests(self):
        """Bounded autonomous notices, never answers; all data/evidence stays in the existing ledger."""
        engine = self.engine
        policy = engine.policy()
        records = engine.records()
        existing = {(r['request_notice']['key'], r['to']['agent']): r for r in records
                    if isinstance(r.get('request_notice'), dict) and isinstance(r['request_notice'].get('key'), str)}
        views, candidates, expected = {}, [], {}
        fences, history_gap, _ = engine.notice_fences()
        capacity = getattr(engine, 'notice_fence_capacity', None)
        for agent, resource in policy.resources.items():
            native = native_facts(resource)
            rows = native.get('requests')
            if not isinstance(rows, list) or not rows:
                continue
            route = engine.request_route(agent, policy)
            view = dict(route, notices=[])
            views[agent] = view
            pending = pending_requests(resource)
            if pending is None:
                view.update(human=True, fallback_reason='request_facts_incomplete')
                continue
            view['request_keys'] = [request_key(resource, question) for question in pending]
            if not route['coordinator']:
                continue
            expected[agent] = len(pending)
            for question in pending:
                key = request_key(resource, question)
                previous = existing.get((key, route['coordinator']))
                if previous:
                    view['notices'].append({'source_key': key, 'request': question['request'], 'revision': question['revision'],
                                            'delivery': previous['delivery'], 'state': previous['state']})
                    if previous['state'] in ('withdrawn', 'expired', 'superseded', 'rejected', 'resolved'):
                        view.update(human=True, fallback_reason='notice_no_longer_deliverable')
                else:
                    if type(capacity) is not int or capacity <= 0:
                        view.update(human=True, fallback_reason='notice_fence_capacity_unknown')
                        continue
                    identity = request_identity_key(resource, question)
                    revision = question['revision']
                    fenced = fences.get(identity)
                    if fenced is not None and revision <= fenced:
                        view.update(human=True, fallback_reason='notice_revision_fenced')
                        continue
                    after_gap = False
                    if fenced is None and history_gap:
                        baseline = self.request_baselines.setdefault(identity, revision)
                        if type(capacity) is int and capacity > 0:
                            while len(self.request_baselines) > capacity:
                                self.request_baselines.popitem(last=False)
                        if revision <= baseline:
                            view.update(human=True, fallback_reason='notice_history_incomplete')
                            continue
                        after_gap = True
                    view.update(human=True, fallback_reason='notification_pending')
                    candidates.append((agent, resource, question, key, route['coordinator'], identity, after_gap))
        count = len(candidates)
        chosen = [candidates[(self.request_cursor + i) % count] for i in range(min(4, count))] if count else []
        self.request_cursor = (self.request_cursor + len(chosen)) % count if count else 0
        identity = self.ext.info
        context = {name: identity.get(name) for name in ('principal', 'actor', 'author', 'initiator')}
        context['kind'] = 'tool'
        context['author'] = identity.get('actual_author', context.get('author'))
        for agent, resource, question, key, target, identity_key, after_gap in chosen:
            view = views[agent]
            epoch = engine.window()['epoch']
            operation = epoch + ':request:' + digest([key, target])
            facts = native_facts(resource)
            content = {'agent': agent, 'epoch': facts.get('epoch'), 'request': question['request'],
                       'revision': question['revision'], 'scope': copy.deepcopy(question['scope']),
                       'expires_at_ms': question.get('expires_at_ms'), 'question': copy.deepcopy(question)}
            args = {'op': operation, 'submission': operation, 'to': target, 'mode': 'admit', 'send_new': True,
                    'content': [{'type': 'text', 'text': 'Native request notice (not an answer):\n' +
                                 json.dumps(content, ensure_ascii=False, sort_keys=True)}]}
            try:
                result = engine.send(args, context, request_notice={'agent': agent, 'key': key,
                                     'source_key': identity_key, 'revision': question['revision']}, notice_after_gap=after_gap)
                view['notices'].append({'source_key': key, 'request': question['request'], 'revision': question['revision'], **result})
                self.dispatch_dirty = True
            except (PolicyError, StoreError, VqroError) as error:
                view.update(human=True, fallback_reason='notification_' + str(getattr(error, 'kind', 'unknown')))
        for agent, view in views.items():
            if view['fallback_reason'] == 'notification_pending' and len(view['notices']) == expected.get(agent):
                view.update(human=view['configured'] == 'both', fallback_reason=None)
            # Own cleanup before I/O: publication can become live then raise,
            # or a later view can fail before the scan reaches its end.
            self.request_views.add(agent)
            self.ext.publish('coord-requests-' + agent, view)
        for agent in self.request_views - set(views):
            self.ext.publish('coord-requests-' + agent, None)
            self.request_views.discard(agent)

    def scan_requests(self):
        try:
            self.route_requests()
            self.request_error = None
        except Exception as error:
            self.request_error = {'kind': getattr(error, 'kind', None) or 'routing_error', 'message': str(error)[:512]}
            self.ext.log('request routing attention:', self.request_error)
            for agent in tuple(self.request_views):
                try:
                    self.ext.publish('coord-requests-' + agent, None)
                except Exception:
                    pass  # retain failed clears for retry, never gate dispatch
                else:
                    self.request_views.discard(agent)
        finally:
            self.next_request_scan = time.monotonic() + 1.0

    def work(self):
        next_sweep = 0.0
        while True:
            self.event.wait(1.0)
            self.event.clear()
            if self.engine is None:
                continue
            try:
                if self.inventory_dirty:
                    self.inventory_dirty = False
                    self.items.sync(self.resources(), self.providers())
                if self.items.pump():
                    self.event.set()
                if time.monotonic() >= self.next_request_scan:
                    self.scan_requests()
                if time.monotonic() >= next_sweep:
                    self.engine.lifecycle_sweep(forget=self.forget)
                    next_sweep = time.monotonic() + 5.0
                if self.dispatch_dirty:
                    self.dispatch_dirty = False
                    self.schedule()
                with self.wake_lock:
                    pending, self.pending_wakes = self.pending_wakes, {}
                    sequence = self.wake_seq
                with self.engine.lock:
                    pending = {agent: count for agent, count in pending.items() if agent in self.engine.permits}
                    control = self.store.get("control")["value"] if pending else None
                if pending:
                    # A count hint is fenced by its epoch/revision, never authority
                    # to wake/insert; the plugin must still make a gated take.
                    self.ext.publish("coord-status", {"items": [{"text": str(sum(pending.values())) + " pending inbox records; admission hints only"}],
                                                      "wake": {"seq": sequence, "pending": pending},
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
        self.register()
        self.start_bootstrap()
        threading.Thread(target=self.work, daemon=True, name="coord-dispatch").start()
        self.ext.contribute({"view/coord-status": {"kind": "panel", "title": "Coordination", "items": []}})
        # Extension.run() would register/subscribe a second time before waiting.
        # Its transport reader, notifications and lifetime watcher already run.
        threading.Event().wait()


def main():
    ext = Extension("coord", event_types=["entries", "resource", "resource.removed", "provider", "missed", "operational"])
    CoordinationService(ext, defer_bootstrap=True).run()


if __name__ == "__main__":
    main()
