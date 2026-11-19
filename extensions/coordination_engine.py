"""Coordination-v0 state machine, independent of sockets, UI and native runtimes.

Only a confirmed operational CAS authorizes an external attempt. Native calls run
outside the state lock so controls do not sit behind long-running content calls.
Every restart changes control_epoch and destroys live permits, not message evidence.
"""
import copy
import hashlib
import json
import threading
import time
import uuid

from coordination_store import StoreError, encoded
from coordination_archive import export as export_archive
from coordination_policy import Policy, PolicyError, QuotaLimits, check_admission, bounded_page, FairQueue, record_charge

OPEN = {"recorded", "held", "claimed", "unknown"}
PENDING = {"recorded", "held"}
FINAL = {"withdrawn", "expired", "superseded", "completed", "handled", "resolved"}
WINDOW = 1024
OP_BUCKETS = 32
SCOPE_CAPACITY = 256
MAX_NATIVE_RESULT = 256 * 1024 - 4096


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def nonce(prefix):
    return prefix + uuid.uuid4().hex


class CoordinationError(PolicyError):
    pass


class Working:
    def __init__(self, engine):
        self.engine = engine
        self.rows = engine.store.snapshot()
        self.changes = {}

    def get(self, key, default=None):
        row = self.changes.get(key, self.rows.get(key))
        return copy.deepcopy(row["value"]) if row else copy.deepcopy(default)

    def version(self, key):
        return self.rows.get(key, {}).get("version", 0)

    def put(self, key, value):
        self.changes[key] = {"value": copy.deepcopy(value)}

    def delete(self, key):
        self.changes[key] = None

    def commit(self, *, control=False):
        ops = [{"op": "set", "key": key, "value": row["value"], "expect": self.version(key)} if row else
               {"op": "delete", "key": key, "expect": self.version(key)} for key, row in self.changes.items()]
        if ops:
            prospective = copy.deepcopy(self.rows)
            for key, row in self.changes.items():
                if row:
                    prospective[key] = {"version": max((r["version"] for r in self.rows.values()), default=0) + 1, "value": row["value"]}
                else:
                    prospective.pop(key, None)
            self.engine._preflight_reservations(prospective, self.rows, control=control)
            return self.engine.store.apply(ops, control=control)


class Coordinator:
    def __init__(self, store, *, config=None, resources=None, invoke=None, config_apply=None,
                 clock=None, core_epoch="fixture-core", owner_incarnation=None, wake=None, providers=None):
        self.store = store
        self.config = config or (lambda: {})
        self.resources = resources or (lambda: {})
        self.providers = providers or (lambda: {})
        self.invoke = invoke or (lambda target, op, args, deadline_ms: {"outcome": "indeterminate", "error": {"kind": "unavailable"}})
        self.config_apply = config_apply
        self.clock = clock or (lambda: int(time.time() * 1000))
        self.wake = wake or (lambda agent, count: None)
        self.control_epoch = core_epoch + ":" + (owner_incarnation or nonce("owner:"))
        self.lock = threading.RLock()
        self.permits = set()
        self.attention = {}
        self.current = True
        self.fair = FairQueue()
        self._boot()

    def policy(self):
        limits = QuotaLimits(domain_bytes=self.store.byte_limit, domain_records=min(self.store.record_limit, 1024), scope_capacity=SCOPE_CAPACITY)
        return Policy(self.config(), self.resources(), now_ms=self.clock(), tombstones=self.tombstones(), limits=limits)

    def _reservation(self, rows):
        charge = self.store.charge(rows)
        records = len(rows)
        for key, row in rows.items():
            value = row["value"]
            if key.startswith("message/") and value.get("state") in OPEN:
                charge += max(0, record_charge(value) - encoded(value))
            elif key.startswith("external/") and value.get("state") == "claimed":
                charge += 1024
        gates = rows.get("control", {}).get("value", {}).get("gates", {})
        for agent in gates:
            if "agent/" + agent not in rows:
                charge += 2048
                records += 1
        return charge, records

    def _preflight_reservations(self, rows, before, *, control):
        charge, count = self._reservation(rows)
        old_charge, old_count = self._reservation(before)
        byte_limit = self.store.byte_limit if control else self.store.byte_limit - (self.store.byte_limit + 9) // 10
        count_limit = self.store.record_limit if control else self.store.record_limit - (self.store.record_limit + 9) // 10
        if ((charge > byte_limit and charge > old_charge) or (count > count_limit and count > old_count)):
            raise CoordinationError("quota", "outcome/tombstone reservations preserve recovery capacity")

    def tombstones(self):
        return {key[6:]: row["value"] for key, row in self.store.snapshot().items() if key.startswith("agent/")}

    def records(self):
        records = [row["value"] for key, row in self.store.snapshot().items() if key.startswith("message/")]
        for record in records:
            if record["delivery"] in self.attention:
                record.update(state="unknown", reason=self.attention[record["delivery"]])
        return records

    def _boot(self):
        # Read old state before changing it. A failed recovery write leaves service unavailable.
        state = self.store.snapshot()
        for offset in range(0, len(state), 60):
            work = Working(self)
            for key, row in list(state.items())[offset:offset + 60]:
                if key.startswith(("message/", "external/")) and row["value"].get("state") == "claimed":
                    record = row["value"]
                    record.update(state="unknown", reason="owner_restarted")
                    work.put(key, record)
            work.commit(control=True)
        work = Working(self)
        meta = work.get("meta", {"window_epoch": nonce("w:"), "window_count": 0, "order": 0})
        work.put("meta", meta)
        prior = work.get("control", {})
        work.put("control", {"epoch": self.control_epoch, "revision": prior.get("revision", 0) + 1,
                             "gates": {}, "consumers": {}})
        work.commit(control=True)

    def fence(self):
        with self.lock:
            self.current = False
            self.permits.clear()

    def window(self):
        meta = self.store.get("meta")["value"]
        return {"epoch": meta["window_epoch"], "from": 0, "to": WINDOW, "used": meta["window_count"]}

    def _bucket(self, principal, op):
        return "ops/" + str(int(digest([principal, op])[:8], 16) % OP_BUCKETS)

    def _cached(self, work, principal, op):
        return work.get(self._bucket(principal, op), {}).get(digest([principal, op]))

    def operation(self, principal, op):
        with self.lock:
            saved = self._cached(Working(self), principal, op)
            if not saved:
                return {"state": "unknown", "window": self.window()}
            result = copy.deepcopy(saved["result"])
            if "result_ref" in result:
                row = self.store.get(result["result_ref"])
                result = row["value"] if row else self.archive_locator(result["result_ref"]) or {"state": "unknown"}
            # Take replays return claim history, NEVER the content to insert again.
            result["window"] = self.window()
            result["historical"] = True
            return result

    def _mutate(self, command, args, context, fn, *, control=False):
        principal = context.get("principal", "undeclared")
        if principal == "undeclared":
            raise CoordinationError("no_grant", "undeclared callers have no coordination rights")
        op = args.get("op")
        if not isinstance(op, str) or len(op.encode()) > 256:
            raise CoordinationError("invalid", "op must be a bounded caller-chosen string")
        fingerprint = digest([command, args])
        with self.lock:
            if not self.current:
                raise CoordinationError("unavailable", "inbox owner was replaced")
            work = Working(self)
            saved = self._cached(work, principal, op)
            if saved:
                if saved["fingerprint"] != fingerprint:
                    raise CoordinationError("conflict", "operation id used for different intent")
                return self.operation(principal, op)
            meta = work.get("meta")
            if not op.startswith(meta["window_epoch"] + ":"):
                raise CoordinationError("expired_operation", "operation is outside its window; inspect, never recreate")
            # Cache buckets are lazily replaced after window rollover. Their old
            # epoch is never admissible again, even when its result is forgotten.
            result = fn(work, self.policy())
            first = result.pop("_first", None)
            after = result.pop("_after", None)
            uncertain = result.pop("_uncertain", None)
            write_control = result.pop("_control", control)
            bucket_key = self._bucket(principal, op)
            bucket = {key: value for key, value in work.get(bucket_key, {}).items()
                      if value.get("window") == meta["window_epoch"]}
            cached_result = copy.deepcopy(result)
            if encoded(cached_result) > 1024:
                ref = "result/" + digest([principal, op])
                if encoded(cached_result) > MAX_NATIVE_RESULT:
                    raise CoordinationError("too_large", "coordination result exceeds retained result bound")
                work.put(ref, cached_result)
                cached_result = {"result_ref": ref}
            bucket[digest([principal, op])] = {"fingerprint": fingerprint, "result": cached_result, "window": meta["window_epoch"]}
            work.put(bucket_key, bucket)
            meta = work.get("meta")
            meta["window_count"] += 1
            if meta["window_count"] >= WINDOW:
                meta["window_epoch"], meta["window_count"] = nonce("w:"), 0
            work.put("meta", meta)
            try:
                work.commit(control=write_control)
            except StoreError as error:
                if uncertain and error.kind in ("durability_unconfirmed", "indeterminate", "timeout"):
                    uncertain()
                raise
            if after:
                after()
            if first:
                result.update(first)
            result["window"] = self.window()
            return result

    def _record(self, work, delivery):
        row = work.get("message/" + str(delivery))
        if row is None:
            archived = work.get("archived/" + str(delivery))
            if archived:
                raise CoordinationError("archived", "delivery is archived", **archived)
            raise CoordinationError("not_found", "unknown delivery; absence is not proof of no effect")
        if delivery in self.attention:
            row.update(state="unknown", reason=self.attention[delivery])
        return row

    def _pending_access(self, record, policy, principal):
        if self._archive_frozen("message/" + record["delivery"]):
            raise CoordinationError("conflict", "delivery is frozen for explicit archive")
        if principal != record["from"]["principal"]:
            policy.require(principal, "inbox.edit_any", record["to"]["agent"])
        if record["state"] not in PENDING:
            raise CoordinationError("too_late", "delivery was claimed or disposed; input cannot be recalled")

    def _ensure_agent(self, work, agent, policy):
        if work.get("agent/" + agent):
            return
        resource = self.resources().get(agent, {})
        row = {"agent": agent, "identity": {key: resource.get(key) for key in ("provider", "instance", "ref", "incarnation")},
               "handles": [], "last_seen_ms": self.clock(), "last_evidence": None,
               "held": 0, "unknown": 0, "membership_ref": None, "lifecycle": "live"}
        key = "doc/agents/members/" + agent
        if key in self.config():
            row["membership_ref"] = {"rN": agent, "revision": self.config()[key]["version"]}
        work.put("agent/" + agent, row)

    def send(self, args, context):
        def admit(work, policy):
            principal = context["principal"]
            submission = args.get("submission")
            if not isinstance(submission, str) or len(submission.encode()) > 256:
                raise CoordinationError("invalid", "submission must be a bounded caller-chosen id")
            submissions = work.get("submissions", {})
            submission_key = digest([principal, submission])
            submitted = submissions.get(submission_key)
            if submitted:
                if submitted["intent"] != digest({key: value for key, value in args.items() if key != "op"}):
                    raise CoordinationError("conflict", "submission id reused for different intent")
                saved = work.get("message/" + submitted["delivery"])
                return {"state": saved["state"] if saved else "unknown", "delivery": submitted["delivery"]}
            prior = next((r for r in self.records() if r["from"]["principal"] == principal and r["submission"] == submission), None)
            if prior:
                if prior.get("intent") != digest({key: value for key, value in args.items() if key != "op"}):
                    raise CoordinationError("conflict", "submission id reused for different intent")
                return {"state": prior["state"], "delivery": prior["delivery"]}
            normalized = policy.validate_send(args)
            address = args.get("to")
            if not isinstance(address, (str, dict)):
                raise CoordinationError("invalid", "send needs an exact address or readable handle")
            agent = policy.resolve_target(address if isinstance(address, str) else address.get("agent"))
            policy.require(principal, "send:" + (args.get("strategy") or normalized["mode"]), agent)
            if not submission.startswith(work.get("meta")["window_epoch"] + ":"):
                raise CoordinationError("expired_operation", "submission outside current operation window")
            records = self.records()
            supersedes = args.get("supersedes")
            previous = self._record(work, supersedes) if supersedes else None
            if previous:
                self._pending_access(previous, policy, principal)
                if previous["from"]["principal"] != principal or previous["to"]["agent"] != agent:
                    raise CoordinationError("no_grant", "supersede requires same sender and exact target")
            waiting = [r for r in records if r["state"] in PENDING and r["from"]["principal"] == principal and r["to"]["agent"] == agent and r["delivery"] != supersedes]
            if waiting and not args.get("send_new", False):
                raise CoordinationError("pending_exists", "update waiting message or explicitly send another", deliveries=[r["delivery"] for r in waiting[:16]])
            outcome_steps = 3 if args.get("strategy") == "interrupt_then_start" else 1
            check_admission(records, principal, agent, normalized["content"], reserved_bytes=self.store.charge(work.rows), reserved_records=len(work.rows), replaces=supersedes, outcome_steps=outcome_steps, limits=policy.limits)
            resource = self.resources().get(agent, {})
            native = (resource.get("data") or {}).get("agent") or {}
            session = native.get("session") or {}
            target = {"agent": agent, "incarnation": resource.get("incarnation", ""), "session": session.get("id"), "as": address if isinstance(address, str) and address.startswith("@") else None}
            if isinstance(address, dict):
                for name in ("incarnation", "session"):
                    if name in address:
                        target[name] = address[name]
            if args.get("follow_current_session") and (args.get("guard") or {}).get("session") is not None:
                raise CoordinationError("invalid", "follow_current_session conflicts with explicit session guard")
            if args.get("origin") is not None or args.get("from") is not None:
                raise CoordinationError("invalid", "sender attribution is core-derived, never body supplied")
            delivery = nonce("d:")
            meta = work.get("meta")
            meta["order"] += 1
            work.put("meta", meta)
            sender = {key: context.get(key) for key in ("principal", "actor", "author", "initiator", "label")}
            origin = {"principal": principal, "actor": context.get("actor", principal), "kind": context.get("kind", "tool")}
            if context.get("agent"):
                origin["agent"] = context["agent"]
            payload = {key: normalized[key] for key in ("mode", "content", "expires_at_ms") if key in normalized}
            record = dict(payload, v=0, delivery=delivery, submission=submission, to=target, **{"from": sender}, origin=origin,
                          state="held", reason="paused", held_by="vqro", order=meta["order"], attempt=None, result=None,
                          intent=digest({key: value for key, value in args.items() if key != "op"}), created_ms=self.clock(), failures=0, handed_over_from=None)
            for key in ("guard", "quoted", "render", "strategy", "follow_current_session", "reply_to", "thread", "retry_of", "supersedes", "key", "correlation"):
                record[key] = copy.deepcopy(args.get(key))
            record["follow_current_session"] = args.get("follow_current_session", False)
            record["outcome_steps"] = outcome_steps
            cause = copy.deepcopy(args.get("cause"))
            if cause:
                cause["provenance"] = "asserted"
            linked = args.get("reply_to") or args.get("retry_of")
            if linked and work.get("message/" + linked):
                cause = {"kind": "delivery", "id": linked, "provenance": "adapter"}
            record["cause"] = cause
            self._ensure_agent(work, agent, policy)
            if previous:
                previous.update(state="superseded", reason="replaced_before_claim")
                work.put("message/" + previous["delivery"], previous)
            work.put("message/" + delivery, record)
            epoch = work.get("meta")["window_epoch"]
            submissions = {key: value for key, value in submissions.items() if value["epoch"] == epoch}
            submissions[submission_key] = {"delivery": delivery, "intent": record["intent"], "epoch": epoch}
            work.put("submissions", submissions)
            return {"state": "recorded", "delivery": delivery, "_after": lambda: self._wake(agent)}
        return self._mutate("inbox.send", args, context, admit)

    def edit(self, args, context, *, withdraw=False):
        def change(work, policy):
            record = self._record(work, args.get("delivery"))
            self._pending_access(record, policy, context["principal"])
            before = copy.deepcopy(record)
            key = "message/" + record["delivery"]
            if args.get("expect") != work.version(key):
                raise CoordinationError("conflict", "message revision changed")
            if withdraw:
                record.update(state="withdrawn", reason="sender_withdrawn")
            else:
                allowed = {"content", "mode", "guard", "render", "quoted", "expires_at_ms"}
                if any(key not in allowed | {"op", "delivery", "expect"} for key in args):
                    raise CoordinationError("invalid", "edit changes only pending input fields")
                updated = dict(record, **{key: value for key, value in args.items() if key in allowed})
                request = {key: updated[key] for key in ("mode", "content", "expires_at_ms", "strategy", "guard", "render", "quoted", "follow_current_session") if key in updated}
                record.update(policy.validate_send(request))
                for key in ("guard", "render", "quoted"):
                    if key in args:
                        record[key] = copy.deepcopy(args[key])
                policy.require(context["principal"], "send:" + record["mode"], record["to"]["agent"])
                if encoded(record) > encoded(before):
                    check_admission(self.records(), record["from"]["principal"], record["to"]["agent"], record["content"],
                                    reserved_bytes=self.store.charge(work.rows), reserved_records=len(work.rows),
                                    replaces=record["delivery"], outcome_steps=record.get("outcome_steps", 1), limits=policy.limits)
            work.put("message/" + record["delivery"], record)
            return {"state": record["state"], "delivery": record["delivery"], "_after": lambda: self._wake(record["to"]["agent"]),
                    "_control": withdraw or encoded(record) <= encoded(before)}
        return self._mutate("inbox.withdraw" if withdraw else "inbox.edit", args, context, change, control=True)

    def get(self, delivery):
        if self.store.get("message/" + str(delivery)) is None:
            archived = self.archive_locator("message/" + str(delivery))
            if archived:
                return dict(archived, delivery=delivery)
        work = Working(self)
        record = self._record(work, delivery)
        return dict(record, revision=work.version("message/" + delivery))

    def listing(self, args):
        rows = self.records()
        if args.get("target"):
            target = self.policy().resolve_target(args["target"])
            rows = [r for r in rows if r["to"]["agent"] == target]
        if args.get("sender"):
            rows = [r for r in rows if r["from"]["principal"] == args["sender"]]
        if args.get("state"):
            rows = [r for r in rows if r["state"] == args["state"]]
        revision = digest([(r["delivery"], self.store.get("message/" + r["delivery"])["version"]) for r in rows])
        return bounded_page(rows, page=args.get("page"), revision=revision, key=lambda r: (r["order"], r["delivery"]))

    @staticmethod
    def _membership_cut(policy):
        # Workspace selection includes newly discovered, unassigned endpoints too.
        # A resource addition must invalidate a preview even before its first member row.
        return digest([policy.membership_revision, sorted(policy.agents)])

    def scope(self, args, context):
        def apply(work, policy):
            action = args.get("action")
            if action not in ("pause", "resume"):
                raise CoordinationError("invalid", "scope action must be pause or resume")
            control = work.get("control")
            expect = args.get("expect") or {}
            if (expect.get("control_epoch") != self.control_epoch or expect.get("control_revision") != control["revision"]
                    or expect.get("membership_revision") != self._membership_cut(policy)):
                raise CoordinationError("conflict", "scope preview is stale")
            targets = policy.scope(args.get("scope"))
            for agent in targets:
                policy.require(context["principal"], action, agent)
            for agent in targets:
                control["gates"][agent] = action == "resume"
            control["revision"] += 1
            work.put("control", control)
            def release():
                if action == "resume":
                    for agent in targets:
                        self.permits.add(agent)
                        self._wake(agent)
                else:
                    self.permits.difference_update(targets)
            receipt = digest([context["principal"], args["op"]])
            retained = sorted((key for key in work.rows if key.startswith("scope/")),
                              key=lambda key: work.rows[key]["value"]["control_revision"])
            for key in retained[:-15] if len(retained) >= 16 else []:
                work.delete(key)
            work.put("scope/" + receipt, {"targets": targets, "control_revision": control["revision"], "membership_revision": self._membership_cut(policy)})
            page = bounded_page([{"agent": agent, "order": order} for order, agent in enumerate(targets)],
                                revision=receipt, key=lambda row: row["order"])
            return {"state": "accepted", "action": action, "targets": [row["agent"] for row in page["items"]],
                    "selected": len(targets), "receipt": receipt, "next": page["next"], "control_revision": control["revision"],
                    "execution": "not_asserted", "_after": release,
                    "_uncertain": lambda: self.permits.difference_update(targets) if action == "pause" else None}
        return self._mutate("inbox.scope", args, context, apply, control=True)

    def scope_preview(self, scope):
        policy = self.policy()
        control = self.store.get("control")["value"]
        return {"targets": policy.scope(scope), "expect": {
            "control_epoch": self.control_epoch, "control_revision": control["revision"],
            "membership_revision": self._membership_cut(policy)}}

    def scope_result(self, receipt, page=None):
        row = self.store.get("scope/" + str(receipt))
        if not row:
            return {"state": "unknown", "receipt": receipt}
        targets = row["value"]["targets"]
        result = bounded_page([{"agent": agent, "order": order} for order, agent in enumerate(targets)],
                              page=page, revision=receipt, key=lambda item: item["order"])
        return {"state": "accepted", "receipt": receipt, "targets": [item["agent"] for item in result["items"]],
                "selected": len(targets), "next": result["next"]}

    def _wake(self, agent):
        if agent in self.permits and self.current:
            count = sum(r["to"]["agent"] == agent and r["state"] in PENDING for r in self.records())
            if count:
                self.wake(agent, count)  # count only; never content

    def _facts(self, agent):
        resource = self.resources().get(agent)
        native = ((resource or {}).get("data") or {}).get("agent") or {}
        return resource, native

    def _caps(self, resource, native):
        return native.get("caps") or {}

    def _eligibility(self, record, policy, *, at=None):
        agent = record["to"]["agent"]
        if self._archive_frozen("message/" + record["delivery"]):
            return "archive_pending"
        if record.get("handover_plan"):
            return "handover_pending"
        if agent not in self.permits or not self.current:
            return "paused"
        stone = self.tombstones().get(agent, {})
        if stone.get("lifecycle") == "retired":
            return "agent_retired"
        resource, native = self._facts(agent)
        if not resource or resource.get("available") is False:
            return "agent_absent"
        lifecycle = native.get("lifecycle", "live")
        if lifecycle == "ended":
            return "agent_ended"
        if lifecycle in ("stopping", "disconnected", "unknown"):
            return "agent_absent"
        if resource.get("incarnation", "") != record["to"]["incarnation"]:
            return "awaiting_route"
        if record.get("expires_at_ms") is not None and self.clock() >= record["expires_at_ms"]:
            return "expired"
        session = (native.get("session") or {}).get("id")
        if session != record["to"]["session"] and not record.get("follow_current_session"):
            return "session_changed"
        guard = record.get("guard") or {}
        if guard.get("session") is not None and guard["session"] != session:
            return "stale_guard"
        if guard.get("run") is not None and guard["run"] != native.get("run"):
            return "stale_guard"
        if any(prior["state"] == "unknown" and prior["to"]["agent"] == agent
               and prior["from"]["principal"] == record["from"]["principal"]
               and prior["mode"] == record["mode"] and prior["order"] < record["order"] for prior in self.records()):
            return "ordering_unknown"
        right = "send:" + (record.get("strategy") or record["mode"])
        if not policy.allowed(record["from"]["principal"], right, agent):
            return "no_grant"
        if record.get("retry_wait"):
            # Busy retry needs a later actual idle transition, never a polling loop.
            marker = [native.get("epoch"), native.get("completion"), native.get("state")]
            if native.get("state") != "idle" or marker == record["retry_wait"]:
                return "agent_busy"
        if at is not None:
            declaration = native.get("pull") or resource.get("pull")
            if not declaration:
                return "integration_unavailable"
            if at not in declaration.get("points", []):
                return "integration_unavailable"
            if declaration.get("insert") == "context" and "context" not in (record.get("render") or []):
                return "origin_unsupported"
            modes = {"turn": {"boundary"}, "settle": {"boundary", "settle"},
                     "idle": {"start", "boundary", "settle"}, "tool": {"admit"}}
            if at == "tool" and declaration.get("tool_boundary"):
                modes["tool"].add("boundary")
            return None if record["mode"] in modes.get(at, set()) else "unsupported_mode"
        caps = self._caps(resource, native)
        send = caps.get("agent.send", {})
        modes = send.get("modes", {})
        if record.get("strategy") == "interrupt_then_start":
            if record["mode"] != "preempt":
                return "unsupported_mode"
            if "agent.interrupt" not in caps or "start" not in modes:
                return "unsupported_mode"
        elif record["mode"] not in modes:
            if record["mode"] != "settle" or "start" not in modes:
                return "unsupported_mode" if resource.get("kind") == "agent" else "integration_unavailable"
        accepted = record.get("render") or ["participant", "attributed_user"]
        renderings = send.get("origin", [])
        if not any(r.get("as") in accepted for r in renderings):
            return "origin_unsupported"
        mode = modes.get(record["mode"], {})
        if native.get("state") != "idle" and (record["mode"] == "start" or record["mode"] == "settle" and not mode):
            return "agent_busy"
        return None

    def _claim(self, work, record, *, consumer=None, at=None):
        agent = record["to"]["agent"]
        resource, native = self._facts(agent)
        observed_session = (native.get("session") or {}).get("id")
        effective_session = observed_session if record.get("follow_current_session") else record["to"]["session"]
        if observed_session != effective_session or resource is None or resource.get("incarnation", "") != record["to"]["incarnation"]:
            record.update(state="held", reason="session_changed" if observed_session != effective_session else "awaiting_route")
            work.put("message/" + record["delivery"], record)
            return None
        attempt = {"id": nonce("a:"), "strategy": record.get("strategy") or record["mode"],
                   "session": effective_session, "control_epoch": self.control_epoch,
                   "claimant": consumer or "dispatcher", "consumer": consumer, "at": at,
                   "claim": nonce("claim:"), "steps": [], "started_ms": self.clock()}
        record.update(state="claimed", reason=None, attempt=attempt)
        record.pop("retry_wait", None)
        work.put("message/" + record["delivery"], record)
        return record

    def dispatch_one(self, agent=None):
        """One native attempt; event-driven caller owns when this is run again."""
        with self.lock:
            work, policy = Working(self), self.policy()
            records = self.records()
            in_flight = {r["to"]["agent"] for r in records if r["state"] == "claimed"}
            eligible = []
            for record in records:
                if record["state"] not in PENDING or record["to"]["agent"] in in_flight:
                    continue
                if agent is not None and record["to"]["agent"] != agent:
                    continue
                reason = self._eligibility(record, policy)
                if reason:
                    desired = "expired" if reason == "expired" else "held"
                    if record.get("reason") != reason or record["state"] != desired:
                        record.update(state=desired, reason=reason)
                        work.put("message/" + record["delivery"], record)
                else:
                    eligible.append(record)
            # Do not make one scan produce an unbounded atomic update.
            if len(work.changes) > 60:
                work.changes = dict(list(work.changes.items())[:60])
            work.commit(control=True)
            selected = self.fair.pick(eligible)
            if selected is None:
                return None
            work = Working(self)
            selected = self._record(work, selected["delivery"])
            claimed = self._claim(work, selected)
            if claimed is None:
                work.commit(control=True)
                return None
            try:
                work.commit()
            except StoreError as error:
                if error.kind == "durability_unconfirmed":
                    recovery = Working(self)
                    record = self._record(recovery, selected["delivery"])
                    record.update(state="held", reason="claim_unconfirmed")
                    recovery.put("message/" + record["delivery"], record)
                    try:
                        recovery.commit(control=True)
                    except StoreError:
                        self.permits.discard(record["to"]["agent"])
                raise
        return self._invoke_claim(selected)

    def _invoke_claim(self, record):
        target = record["to"]["agent"]
        _, native = self._facts(target)
        mode = record["mode"]
        caps = self._caps(self.resources().get(target, {}), native)
        if record.get("strategy") == "interrupt_then_start":
            steps = [("agent.queue", {"v": 0, "action": "clear"}, "queue"),
                     ("agent.interrupt", {"v": 0}, "interrupt"),
                     ("agent.send", self._send_args(record, "start"), "send:interrupt_then_start")]
        else:
            if mode == "settle" and mode not in caps.get("agent.send", {}).get("modes", {}):
                mode = "start"
            steps = [("agent.send", self._send_args(record, mode), "send:" + record["mode"])]
        deadline = time.monotonic() + 5.0
        outcomes = []
        for op, args, right in steps:
            with self.lock:
                current = self.get(record["delivery"])
                valid = (self.current and current["state"] == "claimed" and current["attempt"]["id"] == record["attempt"]["id"]
                         and current["attempt"]["control_epoch"] == self.control_epoch)
                resource, facts = self._facts(target)
                session = (facts.get("session") or {}).get("id")
                reason = None
                if not valid:
                    return {"state": "unknown", "reason": "owner_or_attempt_changed"}
                if target not in self.permits:
                    reason = "paused"
                elif time.monotonic() >= deadline:
                    reason = "deadline"
                elif not resource or resource.get("incarnation", "") != record["to"]["incarnation"]:
                    reason = "awaiting_route"
                elif session != record["attempt"]["session"]:
                    reason = "stale_guard"
                elif (record.get("guard") or {}).get("run") is not None and record["guard"]["run"] != facts.get("run"):
                    reason = "stale_guard"
                elif not self.policy().allowed(record["from"]["principal"], right, target):
                    reason = "no_grant"
                elif not outcomes and record.get("expires_at_ms") is not None and self.clock() >= record["expires_at_ms"]:
                    reason = "expired"
                if reason:
                    return self._finish_attempt(record, outcomes, {"outcome": "rejected", "error": {"kind": reason}}, partial=bool(outcomes))
                # Persist each composition step before issuing it. A restart never runs a tail.
                work = Working(self)
                now = self._record(work, record["delivery"])
                now["attempt"]["steps"] = self._compact_steps(outcomes) + [{"op": op, "outcome": "unknown"}]
                work.put("message/" + record["delivery"], now)
                work.commit(control=True)
            try:
                result = self.invoke(target, op, args, max(1, int((deadline - time.monotonic()) * 1000)))
                if not isinstance(result, dict) or result.get("outcome") not in ("accepted", "observed", "dispatched", "rejected", "indeterminate"):
                    result = {"outcome": "indeterminate", "error": {"kind": "invalid_native_result"}}
            except Exception as error:
                result = {"outcome": "indeterminate", "error": {"kind": getattr(error, "kind", "transport"), "message": str(error)[:1024]}}
            outcomes.append({"op": op, "outcome": result.get("outcome"), "native": result})
            if result["outcome"] not in ("accepted", "observed", "dispatched"):
                return self._finish_attempt(record, outcomes, result, partial=len(outcomes) > 1)
            if result["outcome"] == "dispatched" and len(steps) > 1 and op != "agent.send":
                # No acknowledgement that a prerequisite completed: do not run its tail.
                return self._finish_attempt(record, outcomes, result, partial=True)
        return self._finish_attempt(record, outcomes, outcomes[-1]["native"], partial=False)

    def _send_args(self, record, mode):
        return {"v": 0, "mode": mode, "content": record["content"], "delivery": record["delivery"],
                "origin": record["origin"], "quoted": record.get("quoted"), "render": record.get("render"),
                "key": record.get("key"), "guard": dict(record.get("guard") or {}, session=record["attempt"]["session"]),
                "cause": record.get("cause"), "correlation": record.get("correlation")}

    @staticmethod
    def _compact(native, operation=None):
        data = native.get("data") or {}
        receipt = data.get("receipt", native.get("receipt", {})) if isinstance(data, dict) else {}
        error = native.get("error") or {}
        def tag(value):
            return value if value is None or isinstance(value, str) and len(value.encode()) <= 128 else "_oversize"
        compact = {"outcome": tag(native.get("outcome", "indeterminate")), "stage": tag(receipt.get("stage")) if isinstance(receipt, dict) else None,
                   "error_kind": tag(error.get("kind")) if isinstance(error, dict) else None, "native_op": tag(operation),
                   "bytes": encoded(native), "sha256": digest(native), "incomplete": True}
        if encoded(compact) > 1024:
            raise CoordinationError("too_large", "compact outcome exceeds its 1 KiB reservation")
        return compact

    def _compact_steps(self, steps):
        return [{"op": step["op"], "outcome": step["outcome"], "native": self._compact(step["native"], step["op"])}
                for step in steps]

    def _finish_attempt(self, original, outcomes, native, *, partial):
        with self.lock:
            if not self.current:
                return {"state": "unknown", "reason": "owner_replaced"}
            work = Working(self)
            record = self._record(work, original["delivery"])
            if record.get("attempt", {}).get("id") != original["attempt"]["id"]:
                return {"state": "unknown", "reason": "attempt_replaced"}
            result = {"state": "partial" if partial else native.get("outcome", "unknown"),
                      "attempt": {"id": record["attempt"]["id"], "steps": outcomes}, "native": native}
            compact_steps = self._compact_steps(outcomes)
            record["attempt"]["steps"] = compact_steps
            record["result"] = dict(result, native=self._compact(native, outcomes[-1]["op"] if outcomes else None),
                                    attempt={"id": record["attempt"]["id"], "steps": compact_steps})
            outcome = native.get("outcome")
            kind = (native.get("error") or {}).get("kind")
            if outcome == "indeterminate" or partial:
                record.update(state="unknown" if outcome == "indeterminate" else "partial", reason="partial_recipe" if partial else "native_unknown")
            elif outcome == "rejected" and kind in ("busy", "stale", "paused", "no_grant", "stale_guard", "awaiting_route"):
                record.update(state="held", reason={"busy": "agent_busy", "stale": "stale_guard"}.get(kind, kind))
                if kind == "busy":
                    record["failures"] += 1
                    _, facts = self._facts(record["to"]["agent"])
                    record["retry_wait"] = [facts.get("epoch"), facts.get("completion"), facts.get("state")]
                    if record["failures"] >= 3:
                        record.update(state="rejected", reason="busy_retry_exhausted")
            elif outcome == "rejected":
                record.update(state="expired" if kind == "expired" else "rejected", reason=kind)
            else:
                record.update(state=outcome, reason=None)
            work.put("message/" + record["delivery"], record)
            try:
                work.commit(control=True)
            except StoreError:
                self.attention[record["delivery"]] = "outcome_persistence_failed"
                self.permits.discard(record["to"]["agent"])
                raise
            self.attention.pop(record["delivery"], None)
            return copy.deepcopy(result)

    def take(self, args, context):
        def pull(work, policy):
            agent = policy.resolve_target(args.get("agent"))
            policy.require(context["principal"], "receive", agent)
            limit = args.get("limit", 16)
            if type(limit) is not int or not 1 <= limit <= 16:
                raise CoordinationError("invalid", "take limit is 1..16")
            consumer, at = args.get("consumer"), args.get("at")
            if at not in ("turn", "settle", "idle", "tool"):
                raise CoordinationError("invalid", "take must name a declared safe point")
            if not isinstance(consumer, str) or not consumer or len(consumer.encode()) > 128:
                raise CoordinationError("invalid", "bounded consumer incarnation required")
            control = work.get("control")
            if control["consumers"].get(agent) != consumer:
                raise CoordinationError("conflict", "consumer must reconcile before taking")
            if agent not in self.permits:
                return {"state": "held", "records": [], "held": "paused"}
            records = self.records()
            if any(r["to"]["agent"] == agent and r["state"] == "claimed" for r in records):
                return {"state": "held", "records": [], "held": "claim_in_flight"}
            eligible = [r for r in records if r["to"]["agent"] == agent and r["state"] in PENDING and self._eligibility(r, policy, at=at) is None]
            supplied, claims = [], []
            for _ in range(limit):
                record = self.fair.pick(eligible)
                if record is None:
                    break
                eligible.remove(record)
                claimed = self._claim(work, record, consumer=consumer, at=at)
                if claimed is None:
                    continue
                row = {key: claimed.get(key) for key in ("delivery", "from", "origin", "quoted", "render", "mode", "content")}
                row["claim"] = claimed["attempt"]["claim"]
                if encoded(supplied + [row]) > 250 * 1024:
                    work.changes.pop("message/" + claimed["delivery"], None)
                    break
                supplied.append(row)
                claims.append({"delivery": row["delivery"], "claim": row["claim"]})
            # The first response carries content only after the whole claim batch commits.
            return {"state": "claimed" if claims else "held", "claims": claims, "records": [],
                    "_first": {"records": supplied}, "held": None if claims else "no_eligible_input"}
        return self._mutate("inbox.take", args, context, pull)

    def reconcile(self, args, context):
        def inspect(work, policy):
            agent = policy.resolve_target(args.get("agent"))
            policy.require(context["principal"], "receive", agent)
            consumer = args.get("consumer")
            if not isinstance(consumer, str) or not consumer or len(consumer.encode()) > 128:
                raise CoordinationError("invalid", "bounded new consumer incarnation required")
            entries = args.get("records", [])
            if not isinstance(entries, list) or len(entries) > 64 or encoded(entries) > 256 * 1024:
                raise CoordinationError("too_large", "reconciliation page exceeds its bound")
            proofs = {row["delivery"]: row.get("entry") for row in entries}
            candidates = [r for r in self.records() if r["to"]["agent"] == agent and (r.get("attempt") or {}).get("consumer") and r["state"] in ("claimed", "unknown")]
            if len(candidates) > 60:
                raise CoordinationError("too_large", "reconcile exact bounded pages before replacing the consumer")
            _, native = self._facts(agent)
            resource = self.resources().get(agent, {})
            declaration = native.get("pull") or resource.get("pull") or {}
            changed = []
            for record in candidates:
                entry = proofs.get(record["delivery"])
                evidenced = bool(entry) and declaration.get("evidence") == "entry"
                record.update(state="inserted" if evidenced else "unknown", reason=None if evidenced else "consumer_restarted")
                record["result"] = {"state": record["state"], "evidence": "entry" if evidenced else "none", "entry": entry if evidenced else None}
                work.put("message/" + record["delivery"], record)
                changed.append({"delivery": record["delivery"], "state": record["state"]})
            control = work.get("control")
            control["consumers"][agent] = consumer
            work.put("control", control)
            return {"state": "reconciled", "records": changed}
        return self._mutate("inbox.reconcile", args, context, inspect, control=True)

    def report(self, args, context):
        def record_report(work, policy):
            record = self._record(work, args.get("delivery"))
            agent = record["to"]["agent"]
            policy.require(context["principal"], "receive", agent)
            attempt = record.get("attempt") or {}
            consumer = args.get("consumer", attempt.get("consumer"))
            control = work.get("control")
            if (not isinstance(attempt.get("consumer"), str) or not attempt["consumer"]
                    or attempt.get("at") not in ("turn", "settle", "idle", "tool")
                    or args.get("claim") != attempt.get("claim") or consumer != attempt.get("consumer")
                    or control["consumers"].get(agent) != consumer or attempt.get("control_epoch") != self.control_epoch):
                raise CoordinationError("conflict", "stale claim or consumer")
            stage = args.get("stage")
            if stage == "released":
                if record["state"] != "claimed":
                    raise CoordinationError("conflict", "cannot release already supplied input")
                record.update(state="held", reason="consumer_released", attempt=None)
            else:
                _, native = self._facts(agent)
                resource = self.resources().get(agent, {})
                declaration = native.get("pull") or resource.get("pull") or {}
                if stage == "inserted" and declaration.get("evidence") != "entry":
                    raise CoordinationError("invalid", "insertion requires a declared durable entry witness")
                if stage == "supplied" and not (declaration.get("insert") == "context" and declaration.get("evidence") == "none"):
                    raise CoordinationError("invalid", "supplied is the context/no-entry path")
                levels = {"claimed": 0, "supplied": 1, "inserted": 1, "read": 2, "handled": 2}
                if stage not in levels or levels[stage] < levels.get(record["state"], 99):
                    raise CoordinationError("conflict", "report must advance the original claim")
                if stage in ("inserted", "supplied") and record["state"] not in ("claimed", stage):
                    raise CoordinationError("conflict", "report cannot change evidence branches")
                record.update(state=stage, reason=None, result={"state": stage, "evidence": declaration.get("evidence", "none")})
            work.put("message/" + record["delivery"], record)
            return {"state": record["state"], "delivery": record["delivery"]}
        return self._mutate("inbox.report", args, context, record_report, control=True)

    def resolve(self, args, context):
        def judge(work, policy):
            record = self._record(work, args.get("delivery"))
            principal, agent = context["principal"], record["to"]["agent"]
            if principal != record["from"]["principal"] and not policy.allowed(principal, "receive", agent):
                policy.require(principal, "recover", agent)
            if record["state"] != "unknown" or args.get("as") not in ("delivered", "not_delivered"):
                raise CoordinationError("invalid", "resolution judges an unknown record only")
            record["resolution"] = {"by": principal, "as": args["as"], "at_ms": self.clock()}
            record["state"] = "resolved"
            work.put("message/" + record["delivery"], record)
            return {"state": "resolved", "delivery": record["delivery"], "native": "not_invoked"}
        return self._mutate("inbox.resolve", args, context, judge, control=True)

    def _external(self, command, args, context, prepare, execute):
        """Write one effect intent before external I/O; replay observes, never invokes."""
        key = "external/" + digest([context["principal"], args.get("op")])
        def begin(work, policy):
            intent = prepare(work, policy)
            work.put(key, {"state": "claimed", "command": command, "intent": intent,
                           "principal": context["principal"], "control_epoch": self.control_epoch,
                           "attempt": {"id": nonce("a:"), "steps": []}})
            def close():
                if intent.get("close_gate"):
                    self.permits.discard(intent["close_gate"])
            return {"state": "claimed", "result_ref": key, "_after": close, "_uncertain": close}
        result = self._mutate(command, args, context, begin, control=True)
        if result.get("historical"):
            return result
        with self.lock:
            row = self.store.get(key)
            if not self.current or row["value"]["control_epoch"] != self.control_epoch:
                return {"state": "unknown", "reason": "owner_replaced", "window": self.window()}
            intent = row["value"]["intent"]
        try:
            native = execute(intent)
        except Exception as error:
            native = {"outcome": "indeterminate", "error": {"kind": getattr(error, "kind", "transport"), "message": str(error)[:1024]}}
        with self.lock:
            if not self.current:
                return {"state": "unknown", "reason": "owner_replaced"}
            work = Working(self)
            record = work.get(key)
            outcome = native.get("outcome", "accepted")
            record.update(state="unknown" if outcome == "indeterminate" else outcome, native=self._compact(native, command))
            record["attempt"]["steps"] = [{"op": command, "outcome": outcome}]
            work.put(key, record)
            work.commit(control=True)
            return dict(record, native=native, window=self.window())

    def config_action(self, command, args, context):
        def prepare(work, policy):
            principal = context["principal"]
            if command == "names":
                ops = policy.names_ops(principal, args.get("handle"), args.get("agent"), expect=args.get("expect"), endpoint_principal=args.get("endpoint_principal"))
            elif command == "membership":
                ops = policy.membership_ops(principal, args.get("agent"), args.get("project"), args.get("reports_to"), expect=args.get("expect"))
            elif command == "grants":
                ops = policy.grant_ops(principal, args.get("id"), args.get("value"), expect=args.get("expect"))
            else:
                raise CoordinationError("invalid", "unknown configuration operation")
            return {"ops": ops}
        def execute(intent):
            if self.config_apply is None:
                return {"outcome": "rejected", "error": {"kind": "unsupported"}}
            result = self.config_apply(intent["ops"], context)
            return {"outcome": "accepted", "data": result}
        return self._external("coord." + command, args, context, prepare, execute)

    def control(self, args, context):
        action = args.get("action")
        actions = {"interrupt": ("interrupt", "agent.interrupt"), "respond": ("answer", "agent.respond"),
                   "queue": ("queue", "agent.queue"), "session": ("session", "agent.session"),
                   "configure": ("configure", "agent.configure"), "command": ("command", "agent.command"),
                   "object": ("command", "agent.object"), "stop": ("stop", "agent.stop"), "retire": ("stop", None)}
        if action not in actions:
            raise CoordinationError("invalid", "unknown control action")
        right, operation = actions[action]
        def prepare(work, policy):
            agent = policy.resolve_target(args.get("target"))
            policy.require(context["principal"], right, agent)
            resource, native = self._facts(agent)
            if not resource:
                raise CoordinationError("unavailable", "target is absent")
            self._ensure_agent(work, agent, policy)
            if action in ("stop", "retire"):
                control = work.get("control")
                control["gates"][agent] = False
                control["revision"] += 1
                work.put("control", control)
            if action == "retire":
                stone = work.get("agent/" + agent)
                stone.update(lifecycle="retired", retired={"by": context["principal"], "reason": (args.get("args") or {}).get("reason")})
                work.put("agent/" + agent, stone)
            method, target = operation, agent
            native_args = dict(args.get("args") or {}, v=0)
            caps = self._caps(resource, native)
            if action == "stop" and native_args.pop("how", "graceful") == "host_kill":
                host = native.get("host") or {}
                host_resource = self.resources().get(host.get("resource"))
                host_caps = self.providers().get((host_resource or {}).get("provider"), {}).get("capabilities", {})
                if (not host_resource or host_resource.get("incarnation") != host.get("incarnation")
                        or "terminal.kill" not in host_caps):
                    raise CoordinationError("unsupported", "no declared exact host termination route; never escalate silently")
                if args.get("confirm_host") != host.get("resource"):
                    raise CoordinationError("conflict", "inspect and explicitly confirm every endpoint served by this host")
                method, target, native_args = "terminal.kill", host["resource"], {}
            elif operation is not None and operation not in caps:
                raise CoordinationError("unsupported", "native control is undeclared")
            return {"agent": agent, "target": target, "incarnation": resource.get("incarnation"), "op": method, "args": native_args,
                    "close_gate": agent if action in ("stop", "retire") else None}
        def execute(intent):
            if intent["op"] is None:
                return {"outcome": "accepted", "data": {"administrative": "retired", "execution": "unknown"}}
            resource, _ = self._facts(intent["agent"])
            if not resource or resource.get("incarnation") != intent["incarnation"]:
                return {"outcome": "rejected", "error": {"kind": "stale"}}
            return self.invoke(intent["target"], intent["op"], intent["args"], 5000)
        return self._external("coord.control", args, context, prepare, execute)

    def handover(self, args, context):
        if args.get("action") == "stage":
            return self._handover_stage(args, context)
        def transfer(work, policy):
            if args.get("action") == "complete":
                plan = work.get("plan/" + str(args.get("plan")))
                if not plan:
                    raise CoordinationError("not_found", "unknown handover plan")
                destination = plan["to"]
                policy.require(context["principal"], "handover", destination["agent"])
                resource, native = self._facts(destination["agent"])
                if not resource or resource.get("incarnation", "") != destination["incarnation"] or native.get("lifecycle") == "ended":
                    raise CoordinationError("conflict", "successor changed or ended")
                if any(stage["state"] != "accepted" for stage in plan["stages"]):
                    raise CoordinationError("conflict", "handover still has unfinished or uncertain ordinary stages")
                for delivery in plan["deliveries"]:
                    record = self._record(work, delivery)
                    if record.get("handover_plan") != args["plan"] or record["state"] not in PENDING:
                        raise CoordinationError("conflict", "handover record changed")
                    record.pop("handover_plan")
                    record["reason"] = "paused" if destination["agent"] not in self.permits else "awaiting_route"
                    work.put("message/" + delivery, record)
                plan["state"] = "completed"
                work.put("plan/" + args["plan"], plan)
                return {"state": "completed", "plan": args["plan"], "_after": lambda: self._wake(destination["agent"])}
            source, destination = args.get("from"), args.get("to")
            if not isinstance(source, dict) or not isinstance(destination, dict):
                raise CoordinationError("invalid", "handover needs exact predecessor and successor")
            for target in (source, destination):
                if not isinstance(target.get("agent"), str) or "incarnation" not in target:
                    raise CoordinationError("invalid", "handover must pin exact agent and incarnation")
                policy.require(context["principal"], "handover", target["agent"])
            resource, native = self._facts(source["agent"])
            stone = work.get("agent/" + source["agent"], {})
            replaced = resource is not None and resource.get("incarnation", "") != source["incarnation"]
            if not replaced and native.get("lifecycle") != "ended" and stone.get("lifecycle") not in ("retired", "ended"):
                raise CoordinationError("conflict", "predecessor must be ended, retired or demonstrably replaced")
            successor, _ = self._facts(destination["agent"])
            if not successor or successor.get("incarnation", "") != destination["incarnation"]:
                raise CoordinationError("conflict", "successor identity changed")
            deliveries = args.get("deliveries")
            if not isinstance(deliveries, list) or len(deliveries) > 58 or len(set(deliveries)) != len(deliveries):
                raise CoordinationError("too_large", "handover exact list has a 58-record atomic bound")
            stages = []
            for kind in ("bind", "membership", "grants"):
                supplied = args.get(kind) or []
                if isinstance(supplied, dict):
                    supplied = [supplied]
                if not isinstance(supplied, list) or len(supplied) > 16:
                    raise CoordinationError("too_large", "handover stage list exceeds bound")
                for stage in supplied:
                    stages.append({"kind": {"bind": "names", "membership": "membership", "grants": "grants"}[kind],
                                   "args": copy.deepcopy(stage), "state": "pending"})
            if len(stages) > 16:
                raise CoordinationError("too_large", "handover has too many ordinary stages")
            plan_id = nonce("handover:")
            for delivery in deliveries:
                record = self._record(work, delivery)
                if record["state"] not in PENDING or record["to"]["agent"] != source["agent"] or record["to"]["incarnation"] != source["incarnation"]:
                    raise CoordinationError("too_late", "handover can move only listed exact pending intent")
                record["handed_over_from"] = copy.deepcopy(record["to"])
                record["to"].update(agent=destination["agent"], incarnation=destination["incarnation"])
                record.update(state="held", reason="handover_pending", handover_plan=plan_id)
                work.put("message/" + delivery, record)
            control = work.get("control")
            control["gates"][source["agent"]] = False
            control["revision"] += 1
            work.put("control", control)
            self._ensure_agent(work, destination["agent"], policy)
            plan = {"state": "pending", "from": source, "to": destination, "deliveries": deliveries, "stages": stages,
                    "principal": context["principal"], "created_ms": self.clock()}
            if encoded(plan) > 4096:
                raise CoordinationError("too_large", "handover plan exceeds 4 KiB; inspect a smaller plan")
            work.put("plan/" + plan_id, plan)
            return {"state": "held", "reason": "handover_pending", "plan": plan_id,
                    "_after": lambda: self.permits.discard(source["agent"]),
                    "_uncertain": lambda: self.permits.discard(source["agent"])}
        return self._mutate("coord.handover", args, context, transfer, control=True)

    def _handover_stage(self, args, context):
        index = args.get("stage")
        with self.lock:
            plan_row = self.store.get("plan/" + str(args.get("plan")))
            if not plan_row or type(index) is not int or not 0 <= index < len(plan_row["value"]["stages"]):
                raise CoordinationError("invalid", "inspect the handover stage first")
            plan = plan_row["value"]
            self.policy().require(context["principal"], "handover", plan["to"]["agent"])
            stage = plan["stages"][index]
            if any(before["state"] != "accepted" for before in plan["stages"][:index]):
                raise CoordinationError("conflict", "previous handover stage unfinished")
            if stage["state"] == "accepted":
                return {"state": "accepted", "plan": args["plan"], "stage": index, "historical": True}
        # config_action persists its own attempt before the ordinary write. The same
        # caller op inspects it after loss; it cannot overwrite a newer CAS binding.
        result = self.config_action(stage["kind"], dict(stage["args"], op=args["op"]), context)
        with self.lock:
            work = Working(self)
            current = work.get("plan/" + args["plan"])
            current["stages"][index]["state"] = result["state"]
            work.put("plan/" + args["plan"], current)
            work.commit(control=True)
        return result

    def lifecycle_sweep(self, *, forget=None, grace_ms=60000):
        """Observe native evidence; silence creates unavailable, never ended."""
        forgotten = []
        with self.lock:
            for agent, old in list(self.tombstones().items()):
                resource, native = self._facts(agent)
                stone = copy.deepcopy(old)
                if native.get("lifecycle") == "ended":
                    stone.update(lifecycle="ended", last_evidence="native_ended")
                    self.permits.discard(agent)
                elif stone.get("lifecycle") == "retired":
                    continue
                elif not resource or resource.get("available") is False:
                    since = stone.setdefault("disconnected_since_ms", self.clock())
                    stone["lifecycle"] = "unavailable" if self.clock() - since >= grace_ms else "disconnected"
                    if resource and stone["lifecycle"] == "unavailable":
                        forgotten.append((agent, resource.get("incarnation")))
                else:
                    stone.pop("disconnected_since_ms", None)
                    stone["lifecycle"] = native.get("lifecycle", "live")
                linked = [r for r in self.records() if r["to"]["agent"] == agent]
                stone["held"] = sum(r["state"] in PENDING for r in linked)
                stone["unknown"] = sum(r["state"] in ("unknown", "claimed", "partial") for r in linked)
                member = self.config().get("doc/agents/members/" + agent)
                stone["membership_ref"] = {"rN": agent, "revision": member["version"]} if member else None
                stone["handles"] = sorted(key[len("doc/agents/names/"):] for key, row in self.config().items()
                                          if key.startswith("doc/agents/names/") and row["value"].get("agent") == agent)[:4]
                if stone != old:
                    work = Working(self)
                    work.put("agent/" + agent, stone)
                    work.commit(control=True)
            # Only evidence-free, unreferenced tombstones are eligible for pruning.
            candidates = sorted((row for row in self.tombstones().values() if row["lifecycle"] in ("unavailable", "ended")
                                 and not row["held"] and not row["unknown"] and not row["membership_ref"]),
                                key=lambda row: row["last_seen_ms"])
            plans = [row["value"] for key, row in self.store.snapshot().items() if key.startswith("plan/") and row["value"]["state"] != "completed"]
            for stone in candidates:
                if any(stone["agent"] in (plan["from"]["agent"], plan["to"]["agent"]) for plan in plans):
                    continue
                # Keep a useful bounded tail; pressure never drops protected evidence.
                if len(self.tombstones()) <= 256:
                    break
                row = self.store.get("agent/" + stone["agent"])
                self.store.apply([{"op": "delete", "key": "agent/" + stone["agent"], "expect": row["version"]}], control=True)
        if forget:
            for agent, incarnation in forgotten:
                forget(agent, incarnation)
        return forgotten

    def _archive_frozen(self, key):
        return any(key in row["value"].get("frozen", {}) for name, row in self.store.snapshot().items()
                   if name.startswith("archive/") and row["value"].get("state") in ("frozen", "exporting", "exported", "partial"))

    def archive(self, args, context):
        action = args.get("action", "freeze")
        archive_id = args.get("archive")
        if action == "inspect":
            plan = self.store.get("archive/" + str(archive_id))
            return plan["value"] if plan else {"state": "unknown"}
        if action == "export":
            def prepare(work, policy):
                plan = work.get("archive/" + str(archive_id))
                if not plan:
                    raise CoordinationError("not_found", "unknown archive plan")
                for agent in plan["agents"]:
                    policy.require(context["principal"], "recover", agent)
                if plan["state"] not in ("frozen", "exporting", "exported"):
                    raise CoordinationError("conflict", "archive is not exportable")
                return {"archive": archive_id}
            def execute(intent):
                with self.lock:
                    plan = self.store.get("archive/" + archive_id)["value"]
                    records = []
                    for key, version in plan["frozen"].items():
                        row = self.store.get(key)
                        if not row or row["version"] != version:
                            return {"outcome": "rejected", "error": {"kind": "conflict", "message": "frozen evidence changed; retained in place"}}
                        records.append({"delivery": key, "version": version, "record": row["value"]})
                exported = export_archive(plan["destination"], archive_id, records)
                with self.lock:
                    work = Working(self)
                    current = work.get("archive/" + archive_id)
                    current.update(exported, state="exported")
                    work.put("archive/" + archive_id, current)
                    work.commit(control=True)
                return {"outcome": "accepted", "data": exported}
            return self._external("coord.inbox.archive.export", args, context, prepare, execute)
        def mutate(work, policy):
            if action == "freeze":
                agents = args.get("agents")
                if not isinstance(agents, list) or not agents or len(agents) > 64 or len(set(agents)) != len(agents):
                    raise CoordinationError("invalid", "archive needs an exact bounded agent list")
                for agent in agents:
                    policy.require(context["principal"], "recover", agent)
                destination = args.get("destination")
                if not isinstance(destination, str) or len(destination.encode()) > 1024:
                    raise CoordinationError("invalid", "archive destination must be an explicit bounded locator")
                chosen = {}
                inspected = args.get("deliveries")
                if inspected is not None and (not isinstance(inspected, list) or len(inspected) > 48):
                    raise CoordinationError("too_large", "archive inspected delivery list exceeds atomic bound")
                for key, row in work.rows.items():
                    value = row["value"]
                    if key.startswith("message/") and value["to"]["agent"] in agents:
                        if inspected is None or value["delivery"] in inspected:
                            chosen[key] = row["version"]
                    elif inspected is None and key.startswith("external/") and value.get("intent", {}).get("agent") in agents:
                        chosen[key] = row["version"]
                if any(work.rows[key]["value"].get("state") == "claimed" for key in chosen):
                    raise CoordinationError("conflict", "an in-flight claim must settle or become unknown before archive")
                if len(chosen) > 48:
                    raise CoordinationError("too_large", "archive capacity is 48 records; inspect an explicit smaller delivery set")
                if any(self._archive_frozen(key) for key in chosen):
                    raise CoordinationError("conflict", "another archive already froze this evidence")
                open_archives = sum(key.startswith("archive/") and row["value"].get("state") != "completed" for key, row in work.rows.items())
                if open_archives >= 4:
                    raise CoordinationError("quota", "four pending archives already use the reserved plan capacity")
                archive = uuid.uuid4().hex
                work.put("archive/" + archive, {"state": "frozen", "agents": agents, "frozen": chosen,
                                               "destination": destination, "principal": context["principal"], "created_ms": self.clock()})
                return {"state": "frozen", "archive": archive, "records": len(chosen)}
            if action == "free":
                plan = work.get("archive/" + str(archive_id))
                if not plan or plan["state"] != "exported" or not plan.get("manifest_hash"):
                    raise CoordinationError("conflict", "archive export must be durably verified before freeing")
                for agent in plan["agents"]:
                    policy.require(context["principal"], "recover", agent)
                # A missing or changed export cannot authorize source deletion.
                with open(plan["locator"], "rb") as source:
                    data = source.read(9 * 1024 * 1024 + 1)
                if len(data) > 9 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != plan["manifest_hash"]:
                    raise CoordinationError("conflict", "archive readback changed; retain all source evidence")
                index = work.get("archive-index", {})
                freed, changed = [], []
                for key, version in plan["frozen"].items():
                    if work.version(key) != version:
                        changed.append(key)
                        continue
                    index[key] = archive_id
                    work.delete(key)
                    freed.append(key)
                if encoded(index) > 200 * 1024:
                    raise CoordinationError("quota", "archive locator index full; no evidence freed")
                work.put("archive-index", index)
                plan.update(state="partial" if changed else "completed", changed=changed,
                            frozen={key: version for key, version in plan["frozen"].items() if key in changed})
                work.put("archive/" + archive_id, plan)
                return {"state": plan["state"], "archive": archive_id, "freed": freed, "changed": changed,
                        "locator": plan["locator"], "manifest_hash": plan["manifest_hash"]}
            raise CoordinationError("invalid", "archive action must be freeze, export, free or inspect")
        return self._mutate("coord.inbox.archive", args, context, mutate, control=True)

    def archive_locator(self, key):
        index = self.store.get("archive-index")
        archive = (index or {}).get("value", {}).get(key)
        plan = self.store.get("archive/" + archive) if archive else None
        if not plan:
            return None
        value = plan["value"]
        result = {"state": "archived", "archive": archive, "locator": value.get("locator"), "manifest_hash": value.get("manifest_hash")}
        try:
            with open(result["locator"], "rb") as source:
                data = source.read(9 * 1024 * 1024 + 1)
            if len(data) > 9 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != result["manifest_hash"]:
                result["state"] = "unknown"
        except (OSError, TypeError):
            result["state"] = "unknown"
        return result
