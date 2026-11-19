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
from coordination_policy import Policy, PolicyError, QuotaLimits, check_admission, bounded_page, FairQueue

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
            return self.engine.store.apply(ops, control=control)


class Coordinator:
    def __init__(self, store, *, config=None, resources=None, invoke=None, config_apply=None,
                 clock=None, core_epoch="fixture-core", owner_incarnation=None, wake=None):
        self.store = store
        self.config = config or (lambda: {})
        self.resources = resources or (lambda: {})
        self.invoke = invoke or (lambda target, op, args, deadline_ms: {"outcome": "indeterminate", "error": {"kind": "unavailable"}})
        self.config_apply = config_apply
        self.clock = clock or (lambda: int(time.time() * 1000))
        self.wake = wake or (lambda agent, count: None)
        self.control_epoch = core_epoch + ":" + (owner_incarnation or nonce("owner:"))
        self.lock = threading.RLock()
        self.permits = set()
        self.current = True
        self.fair = FairQueue()
        self._boot()

    def policy(self):
        return Policy(self.config(), self.resources(), now_ms=self.clock(), tombstones=self.tombstones())

    def tombstones(self):
        return {key[6:]: row["value"] for key, row in self.store.snapshot().items() if key.startswith("agent/")}

    def records(self):
        return [row["value"] for key, row in self.store.snapshot().items() if key.startswith("message/")]

    def _boot(self):
        # Read old state before changing it. A failed recovery write leaves service unavailable.
        state = self.store.snapshot()
        for offset in range(0, len(state), 60):
            work = Working(self)
            for key, row in list(state.items())[offset:offset + 60]:
                if key.startswith("message/") and row["value"].get("state") == "claimed":
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
                result = row["value"] if row else {"state": "unknown"}
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
            work.commit(control=control)
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
        return row

    def _pending_access(self, record, policy, principal):
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
            normalized = policy.validate_send(args)
            principal = context["principal"]
            address = args.get("to")
            agent = policy.resolve_target(address if isinstance(address, str) else address.get("agent"))
            policy.require(principal, "send:" + (args.get("strategy") or normalized["mode"]), agent)
            submission = args.get("submission")
            if not isinstance(submission, str) or len(submission.encode()) > 256:
                raise CoordinationError("invalid", "submission must be a bounded caller-chosen id")
            prior = next((r for r in self.records() if r["from"]["principal"] == principal and r["submission"] == submission), None)
            if prior:
                if prior.get("intent") != digest({key: value for key, value in args.items() if key != "op"}):
                    raise CoordinationError("conflict", "submission id reused for different intent")
                return {"state": prior["state"], "delivery": prior["delivery"]}
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
            occupancy = {key: row for key, row in work.rows.items() if not key.startswith("message/")}
            check_admission(records, principal, agent, normalized["content"], reserved_bytes=encoded(occupancy), reserved_records=len(occupancy), replaces=supersedes)
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
            record = dict(normalized, v=0, delivery=delivery, submission=submission, to=target, **{"from": sender}, origin=origin,
                          state="held", reason="paused", held_by="vqro", order=meta["order"], attempt=None, result=None,
                          intent=digest({key: value for key, value in args.items() if key != "op"}), created_ms=self.clock(), failures=0, handed_over_from=None)
            for key in ("guard", "quoted", "render", "strategy", "follow_current_session", "reply_to", "thread", "retry_of", "supersedes", "key", "correlation"):
                record[key] = copy.deepcopy(args.get(key))
            record["follow_current_session"] = args.get("follow_current_session", False)
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
            return {"state": "recorded", "delivery": delivery, "_after": lambda: self._wake(agent)}
        return self._mutate("inbox.send", args, context, admit)

    def edit(self, args, context, *, withdraw=False):
        def change(work, policy):
            record = self._record(work, args.get("delivery"))
            self._pending_access(record, policy, context["principal"])
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
                record.update(policy.validate_send(updated))
                for key in ("guard", "render", "quoted"):
                    if key in args:
                        record[key] = copy.deepcopy(args[key])
                policy.require(context["principal"], "send:" + record["mode"], record["to"]["agent"])
            work.put("message/" + record["delivery"], record)
            return {"state": record["state"], "delivery": record["delivery"], "_after": lambda: self._wake(record["to"]["agent"])}
        return self._mutate("inbox.withdraw" if withdraw else "inbox.edit", args, context, change, control=True)

    def get(self, delivery):
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

    def scope(self, args, context):
        def apply(work, policy):
            action = args.get("action")
            if action not in ("pause", "resume"):
                raise CoordinationError("invalid", "scope action must be pause or resume")
            control = work.get("control")
            expect = args.get("expect") or {}
            if (expect.get("control_epoch") != self.control_epoch or expect.get("control_revision") != control["revision"]
                    or expect.get("membership_revision") != policy.membership_revision):
                raise CoordinationError("conflict", "scope preview is stale")
            targets = policy.scope(args.get("scope"))
            for agent in targets:
                policy.require(context["principal"], action, agent)
            for agent in targets:
                control["gates"][agent] = action == "resume"
            control["revision"] += 1
            work.put("control", control)
            if action == "pause":
                self.permits.difference_update(targets)
            def release():
                if action == "resume":
                    self.permits.update(targets)
                    for agent in targets:
                        self._wake(agent)
            return {"state": "accepted", "action": action, "targets": targets, "control_revision": control["revision"],
                    "execution": "not_asserted", "_after": release}
        return self._mutate("inbox.scope", args, context, apply, control=True)

    def scope_preview(self, scope):
        policy = self.policy()
        control = self.store.get("control")["value"]
        return {"targets": policy.scope(scope), "expect": {
            "control_epoch": self.control_epoch, "control_revision": control["revision"],
            "membership_revision": policy.membership_revision}}

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
        _, native = self._facts(agent)
        attempt = {"id": nonce("a:"), "strategy": record.get("strategy") or record["mode"],
                   "session": (native.get("session") or {}).get("id"), "control_epoch": self.control_epoch,
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
            self._claim(work, selected)
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
                now["attempt"]["steps"] = copy.deepcopy(outcomes) + [{"op": op, "outcome": "unknown"}]
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
            if encoded(result) > MAX_NATIVE_RESULT:
                # Keep the durable claim; no outcome can be invented from an overlarge reply.
                raise CoordinationError("too_large", "native result exceeds reserved evidence bound; inspect claimed attempt")
            record["attempt"]["steps"] = copy.deepcopy(outcomes)
            record["result"] = result
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
            work.commit(control=True)
            return copy.deepcopy(result)

    def take(self, args, context):
        def pull(work, policy):
            agent = policy.resolve_target(args.get("agent"))
            policy.require(context["principal"], "receive", agent)
            limit = args.get("limit", 16)
            if type(limit) is not int or not 1 <= limit <= 16:
                raise CoordinationError("invalid", "take limit is 1..16")
            consumer, at = args.get("consumer"), args.get("at")
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
            eligible.sort(key=lambda r: (r["order"], r["delivery"]))
            supplied, claims = [], []
            for record in eligible[:limit]:
                claimed = self._claim(work, record, consumer=consumer, at=at)
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
            if (args.get("claim") != attempt.get("claim") or consumer != attempt.get("consumer")
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
