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
CONTROL_ADMISSION_BYTES = 64 * 1024
CONTROL_ADMISSION_RECORDS = 16
ARCHIVE_FUTURE_BYTES = 16 * 1024
ARCHIVE_ESCROW_BYTES = 64 * 1024
ARCHIVE_ESCROW_RECORDS = 16
IDENTITY_SUMMARY_BYTES = 3 * 1024


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def nonce(prefix):
    return prefix + uuid.uuid4().hex


class CoordinationError(PolicyError):
    pass


class Working:
    def __init__(self, engine, *, archive=False):
        self.engine = engine
        self.archive = archive
        self.rows = engine.store.view()
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

    def commit(self, *, control=False, normal=False):
        ops = [{"op": "set", "key": key, "value": row["value"], "expect": self.version(key)} if row else
               {"op": "delete", "key": key, "expect": self.version(key)} for key, row in self.changes.items()]
        if ops:
            prospective = dict(self.rows)
            for key, row in self.changes.items():
                if row:
                    prospective[key] = {"version": max((r["version"] for r in self.rows.values()), default=0) + 1, "value": row["value"]}
                else:
                    prospective.pop(key, None)
            self.engine._preflight_reservations(prospective, self.rows, control=control, normal=normal, archive=self.archive)
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
        self.native_inflight = {}
        self.current = True
        self.fair = FairQueue()
        self.maintenance_error = None
        self._boot()
        self._safe_maintain()

    def policy(self):
        limits = QuotaLimits(domain_bytes=self.store.byte_limit, domain_records=min(self.store.record_limit, 1024), scope_capacity=SCOPE_CAPACITY)
        return Policy(self.config(), self.resources(), now_ms=self.clock(), tombstones=self.tombstones(), limits=limits)

    def _reservation(self, rows):
        charge = self.store.charge(rows)
        records = len(rows)
        for key, row in rows.items():
            value = row["value"]
            if key.startswith("message/"):
                state = value.get("state")
                if state in OPEN:
                    remaining = value.get("outcome_steps", 1)
                    if state == "claimed":
                        done = sum(step.get("outcome") not in (None, "unknown") for step in (value.get("attempt") or {}).get("steps", []))
                        remaining = max(0, remaining - done)
                    elif state == "unknown":
                        remaining = 1  # a late receipt, never an executable recipe tail
                    claim_reserve = 1024 if state in PENDING else 0
                    base = max(encoded(value) + claim_reserve, encoded(value.get("content", [])) + 2048)
                    charge += base + remaining * 1024 - encoded(value)
                elif state in ("accepted", "dispatched", "admitted", "queued", "inserted", "supplied", "read"):
                    charge += max(0, 1024 - encoded(value))
            elif key.startswith("external/") and value.get("state") == "claimed":
                charge += 1024
            elif key == "settled":
                charge += max(0, 128 * len(value) - encoded(value))
            elif key.startswith("archive/") and value.get("state") in ("frozen", "exporting", "exported", "partial"):
                # Four accepted plans retain funding for export, readback, free,
                # locator-index growth and their operation receipts.
                charge += ARCHIVE_FUTURE_BYTES if value["state"] in ("frozen", "exporting") else ARCHIVE_FUTURE_BYTES // 2
                records += 4 if value["state"] in ("frozen", "exporting") else 2
        summaries = {agent: value for key, row in rows.items() if key.startswith("agents/") for agent, value in row["value"].items()}
        charge += sum(max(0, IDENTITY_SUMMARY_BYTES - encoded(value)) for value in summaries.values())
        gates = rows.get("control", {}).get("value", {}).get("gates", {})
        for agent in gates:
            if agent not in summaries:
                charge += IDENTITY_SUMMARY_BYTES
        return charge, records

    def _preflight_reservations(self, rows, before, *, control, normal=False, archive=False):
        charge, count = self._reservation(rows)
        old_charge, old_count = self._reservation(before)
        byte_limit = self.store.byte_limit if control and not normal else self.store.byte_limit - (self.store.byte_limit + 9) // 10
        count_limit = self.store.record_limit if control and not normal else self.store.record_limit - (self.store.record_limit + 9) // 10
        if control and not normal and not archive:
            byte_limit -= ARCHIVE_ESCROW_BYTES
            count_limit -= ARCHIVE_ESCROW_RECORDS
        if not control:
            # Ordinary content leaves bounded normal-ledger space for controls;
            # unresolved controls themselves never borrow the recovery reserve.
            byte_limit -= CONTROL_ADMISSION_BYTES
            count_limit -= CONTROL_ADMISSION_RECORDS
        if ((charge > byte_limit and charge > old_charge) or (count > count_limit and count > old_count)):
            raise CoordinationError("quota", "outcome/tombstone reservations preserve recovery capacity")

    def tombstones(self):
        return {agent: value for key, row in self.store.view().items() if key.startswith("agents/") for agent, value in row["value"].items()}

    @staticmethod
    def _agent_key(agent):
        return "agents/" + digest(agent)[0]

    def _agent_get(self, work, agent, default=None):
        return work.get(self._agent_key(agent), {}).get(agent, copy.deepcopy(default))

    def _agent_put(self, work, agent, value):
        if encoded(value) > IDENTITY_SUMMARY_BYTES:
            # Invalid/future oversized identity data cannot brick workspace pause.
            # Keep explicit bounded evidence, never silently pretend it is a full
            # identity, and hold input for this target only.
            identity = value.get("identity", {})
            value = {"agent": agent, "identity": {"bytes": encoded(identity), "sha256": digest(identity), "incomplete": True},
                     "identity_incomplete": True, "summary_sha256": digest(value),
                     "lifecycle": value.get("lifecycle", "unknown"), "handles": [],
                     "last_seen_ms": value.get("last_seen_ms"), "last_evidence": value.get("last_evidence"),
                     "held": value.get("held"), "unknown": value.get("unknown"), "membership_ref": None}
        page = work.get(self._agent_key(agent), {})
        page[agent] = value
        work.put(self._agent_key(agent), page)

    def records(self):
        records = [copy.deepcopy(row["value"]) for key, row in self.store.view().items() if key.startswith("message/")]
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

    def configuration_status(self):
        try:
            policy = self.policy()
            return {"membership_valid": not policy.membership_error,
                    "membership_errors": policy.membership_errors[:8], "incomplete": len(policy.membership_errors) > 8}
        except PolicyError as error:
            return {"valid": False, "error": {"kind": error.kind, "message": str(error)[:1024]}}

    def attention_summary(self):
        rows = self.store.view()
        messages = [{"delivery": row["value"]["delivery"], "agent": row["value"]["to"]["agent"], "state": row["value"]["state"]}
                    for key, row in rows.items() if key.startswith("message/") and row["value"]["state"] in ("unknown", "claimed", "partial")]
        controls = [{"reference": key, "principal": row["value"].get("principal"), "caller": row["value"].get("caller"),
                     "command": row["value"].get("command"), "agent": row["value"].get("intent", {}).get("agent"), "state": row["value"]["state"]}
                    for key, row in rows.items() if key.startswith("external/") and row["value"]["state"] in ("claimed", "unknown", "partial")]
        identities = [{"agent": agent, "state": "held", "reason": "identity_unavailable"}
                      for agent, summary in self.tombstones().items() if summary.get("identity_incomplete")]
        return {"messages": len(messages), "controls": len(controls), "identities": len(identities),
                "items": (messages + controls + identities)[:64],
                "incomplete": len(messages) + len(controls) + len(identities) > 64}

    def _cached(self, work, principal, op):
        return work.get(self._bucket(principal, op), {}).get(digest([principal, op]))

    def operation(self, principal, op):
        with self.lock:
            saved = self._cached(Working(self), principal, op)
            if not saved:
                unresolved = self.store.get("external/" + digest([principal, op]))
                if unresolved:
                    return dict(unresolved["value"], window=self.window(), historical=True)
                take_key = digest([principal, op])
                retained = [record for record in self.records()
                            if (record.get("attempt") or {}).get("take") == take_key]
                if retained:
                    return {"state": "claimed" if any(r["state"] == "claimed" for r in retained) else "unknown",
                            "claims": [{"delivery": r["delivery"], "claim": r["attempt"]["claim"], "state": r["state"]}
                                       for r in retained if r["attempt"].get("claim")],
                            "records": [], "historical": True, "window": self.window()}
                return {"state": "unknown", "window": self.window()}
            result = copy.deepcopy(saved["result"])
            if result.get("take"):
                take_key = digest([principal, op])
                claims = [{"delivery": record["delivery"], "claim": record["attempt"]["claim"]}
                          for record in self.records() if (record.get("attempt") or {}).get("take") == take_key]
                result = {"state": result["state"] if claims or result["state"] != "claimed" else "unknown", "claims": claims, "records": []}
            elif result.get("scope"):
                result = self.scope_result(digest([principal, op]))
            elif result.get("external"):
                row = self.store.get("external/" + digest([principal, op]))
                result = row["value"] if row else {"state": result["state"], "incomplete": True, "native_hash": result.get("native_hash")}
            if "result_ref" in result:
                row = self.store.get(result["result_ref"])
                result = row["value"] if row else self.archive_locator(result["result_ref"]) or {"state": "unknown"}
            # Take replays return claim history, NEVER the content to insert again.
            result["window"] = self.window()
            result["historical"] = True
            return result

    def _mutate(self, command, args, context, fn, *, control=False, normal=False):
        principal = context.get("principal", "undeclared")
        if principal == "undeclared":
            raise CoordinationError("no_grant", "undeclared callers have no coordination rights")
        op = args.get("op")
        if not isinstance(op, str) or len(op.encode()) > 256:
            raise CoordinationError("invalid", "op must be a bounded caller-chosen string")
        fingerprint = digest([command, args])
        archive = command.startswith("coord.inbox.archive")
        with self.lock:
            if not self.current:
                raise CoordinationError("unavailable", "inbox owner was replaced")
            work = Working(self, archive=archive)
            saved = self._cached(work, principal, op)
            if saved:
                if saved["fingerprint"] != fingerprint:
                    raise CoordinationError("conflict", "operation id used for different intent")
                refusal = saved["result"].get("refusal")
                if refusal:
                    raise CoordinationError(refusal["kind"], refusal["message"], refusal["data"])
                return self.operation(principal, op)
            meta = work.get("meta")
            if not op.startswith(meta["window_epoch"] + ":"):
                raise CoordinationError("expired_operation", "operation is outside its window; inspect, never recreate")
            # Bound even an adversarial concentration in one hash bucket. Closing
            # the old epoch cannot make forgotten operation IDs admissible again.
            bucket = work.get(self._bucket(principal, op), {})
            if sum(value.get("window") == meta["window_epoch"] for value in bucket.values()) >= 64:
                meta["window_epoch"], meta["window_count"] = nonce("w:"), 0
                work.put("meta", meta)
                work.commit(control=True)
                raise CoordinationError("busy", "bounded result window closed before evaluation", {"window": self.window()})
            # A refusal also needs a slot. Test room for the largest canonical
            # refusal before consulting policy or evaluating the requested action.
            trial = Working(self, archive=archive)
            self._cache_operation(trial, command, principal, op, fingerprint,
                                  {"state": "rejected", "reserved": " " * 2048}, compact=False)
            prospective = dict(trial.rows)
            for key, value in trial.changes.items():
                prospective[key] = {"version": max((row["version"] for row in trial.rows.values()), default=0) + 1, "value": value["value"]}
            try:
                self._preflight_reservations(prospective, before=trial.rows, control=control, archive=archive)
            except PolicyError:
                meta["window_epoch"], meta["window_count"] = nonce("w:"), 0
                work.put("meta", meta)
                try:
                    work.commit(control=True)
                except StoreError:
                    self.current = False
                    self.permits.clear()
                    raise
                self._safe_maintain()
                raise CoordinationError("busy", "result window closed before evaluation: no reply slot", {"window": self.window()})
            failure, first, after, uncertain = None, None, None, None
            write_control = control
            try:
                result = fn(work, self.policy())
                first = result.pop("_first", None)
                after = result.pop("_after", None)
                uncertain = result.pop("_uncertain", None)
                write_control = result.pop("_control", control)
                self._cache_operation(work, command, principal, op, fingerprint, result)
                work.commit(control=write_control, normal=normal)
            except (PolicyError, StoreError) as error:
                if isinstance(error, StoreError) and error.kind not in ("quota", "too_large", "invalid", "conflict", "io"):
                    if uncertain and error.kind in ("durability_unconfirmed", "indeterminate", "timeout"):
                        uncertain()
                    raise
                failure = self._bounded_refusal(error)
                work = Working(self, archive=archive)  # discard all uncommitted requested changes
                result = {"state": "rejected", "reason": failure["kind"], "refusal": failure}
                self._cache_operation(work, command, principal, op, fingerprint, result, compact=False)
                try:
                    work.commit(control=control)
                except (PolicyError, StoreError):
                    # No refusal can be forgotten and later become an admission.
                    # A fresh owner rolls the old window before doing anything.
                    self.current = False
                    self.permits.clear()
                    raise
            if not failure and after:
                after()
            if not failure and first:
                result.update(first)
            self._safe_maintain()
            if failure:
                raise CoordinationError(failure["kind"], failure["message"], failure["data"])
            result["window"] = self.window()
            return result

    @staticmethod
    def _bounded_refusal(error):
        message = str(error).encode("utf-8")[:512].decode("utf-8", "ignore")
        data = copy.deepcopy(getattr(error, "data", {}))
        if encoded(data) > 1024:
            data = {"incomplete": True, "sha256": digest(data), "bytes": encoded(data)}
        return {"kind": error.kind, "message": message, "data": data}

    def _cache_operation(self, work, command, principal, op, fingerprint, result, *, compact=True):
        meta = work.get("meta")
        key = self._bucket(principal, op)
        bucket = {key: value for key, value in work.get(key, {}).items() if value.get("window") == meta["window_epoch"]}
        cached = copy.deepcopy(result)
        if compact and command == "inbox.take":
            cached = {"state": result["state"], "take": True}
        elif compact and command == "inbox.scope":
            cached = {"state": result["state"], "scope": True}
        elif compact and encoded(cached) > 256:
            cached = {key: result[key] for key in ("state", "delivery", "plan", "archive", "reason") if key in result}
            cached["incomplete"] = True
            if encoded(cached) > 256:
                cached = {"state": result["state"], "incomplete": True}
        bucket[digest([principal, op])] = {"fingerprint": fingerprint, "result": cached, "window": meta["window_epoch"]}
        work.put(key, bucket)
        meta["window_count"] += 1
        if meta["window_count"] >= WINDOW:
            meta["window_epoch"], meta["window_count"] = nonce("w:"), 0
        work.put("meta", meta)

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
        if self._agent_get(work, agent):
            return
        resource = policy.resources.get(agent, {})
        native = (resource.get("data") or {}).get("agent") or {}
        lifecycle = native.get("lifecycle", "unknown")
        identity = {key: resource.get(key) for key in ("provider", "instance", "incarnation")}
        identity["ref"] = resource.get("backend_ref", resource.get("ref"))
        row = {"agent": agent, "identity": identity,
               "handles": sorted(handle for handle, bound in policy.names.items() if bound["agent"] == agent),
               "last_seen_ms": self.clock() if resource and resource.get("available") is not False else None,
               "last_evidence": "native_ended" if lifecycle == "ended" else None,
               "held": 0, "unknown": 0, "membership_ref": None, "lifecycle": lifecycle}
        key = "doc/agents/members/" + agent
        if key in policy.entries:
            row["membership_ref"] = {"rN": agent, "revision": policy.entries[key]["version"]}
        self._agent_put(work, agent, row)

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
            if normalized.get("strategy") == "interrupt_then_start":
                for step_right in ("queue", "interrupt", "send:start"):
                    policy.require(principal, step_right, agent)
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
            associated = policy._principal_agents.get(principal, set())
            sender_agent = context.get("agent") or (next(iter(associated)) if len(associated) == 1 else None)
            sender["kind"] = "agent" if sender_agent else context.get("kind", "tool")
            if sender_agent:
                sender["agent"] = sender_agent
            origin = {"principal": principal, "actor": context.get("actor", principal), "kind": sender["kind"],
                      "sender": dict(copy.deepcopy(sender), provenance="inbox-record")}
            if sender_agent:
                origin["agent"] = sender_agent
            payload = {key: normalized[key] for key in ("mode", "content", "expires_at_ms", "priority") if key in normalized}
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
                allowed = {"content", "mode", "guard", "render", "quoted", "expires_at_ms", "priority"}
                if any(key not in allowed | {"op", "delivery", "expect"} for key in args):
                    raise CoordinationError("invalid", "edit changes only pending input fields")
                updated = dict(record, **{key: value for key, value in args.items() if key in allowed})
                request = {key: updated[key] for key in ("mode", "content", "expires_at_ms", "strategy", "guard", "render", "quoted", "follow_current_session", "priority") if key in updated}
                record.update(policy.validate_send(request))
                for key in ("guard", "render", "quoted"):
                    if key in args:
                        record[key] = copy.deepcopy(args[key])
                policy.require(context["principal"], "send:" + (record.get("strategy") or record["mode"]), record["to"]["agent"])
                if record.get("strategy") == "interrupt_then_start":
                    for right in ("queue", "interrupt", "send:start"):
                        policy.require(context["principal"], right, record["to"]["agent"])
                if encoded(record) > encoded(before):
                    check_admission(self.records(), record["from"]["principal"], record["to"]["agent"], record["content"],
                                    reserved_bytes=self.store.charge(work.rows), reserved_records=len(work.rows),
                                    replaces=record["delivery"], outcome_steps=record.get("outcome_steps", 1), limits=policy.limits)
                record.pop("review_required", None)
                record.update(state="held", reason="edited")
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
        if work.get("message/" + str(delivery)) is None:
            return dict(work.get("settled", {}).get(delivery, {"state": "unknown"}), delivery=delivery, incomplete=True)
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
                self._ensure_agent(work, agent, policy)
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
        stone = (self.store.get(self._agent_key(agent)) or {}).get("value", {}).get(agent, {})
        if stone.get("lifecycle") == "retired":
            return "agent_retired"
        if stone.get("lifecycle") == "ended":
            return "agent_ended"
        resource, native = self._facts(agent)
        lifecycle = native.get("lifecycle", "live")
        if lifecycle == "ended":
            return "agent_ended"
        if agent not in self.permits or not self.current:
            return "paused"
        if stone.get("identity_incomplete"):
            return "identity_unavailable"
        if (self.store.get("control") or {}).get("value", {}).get("reconciling", {}).get(agent):
            return "reconciling"
        if record.get("review_required"):
            return "stale_guard"
        if not resource or resource.get("available") is False:
            return "agent_absent"
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
        if guard.get("run") is not None and native.get("run") is not None and guard["run"] != native["run"]:
            return "stale_guard"
        if any(prior["state"] == "unknown" and prior["to"]["agent"] == agent
               and prior["from"]["principal"] == record["from"]["principal"]
               and prior["mode"] == record["mode"] and prior["order"] < record["order"] for prior in self.records()):
            return "ordering_unknown"
        right = "send:" + (record.get("strategy") or record["mode"])
        if not policy.allowed(record["from"]["principal"], right, agent):
            return "membership_invalid" if policy.team_grant_blocked(record["from"]["principal"], right, agent) else "no_grant"
        if record.get("strategy") == "interrupt_then_start" and not all(
                policy.allowed(record["from"]["principal"], step_right, agent)
                for step_right in ("queue", "interrupt", "send:start")):
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
            points = declaration.get("points")
            point = points.get(at) if isinstance(points, dict) else None
            if not isinstance(point, dict) or point.get("precision") not in ("exact", "approximate"):
                return "declaration_invalid"
            if declaration.get("insert") == "context" and "context" not in (record.get("render") or []):
                return "origin_unsupported"
            modes = {"turn": {"boundary"}, "settle": {"boundary", "settle"},
                     "idle": {"start", "boundary", "settle"}, "tool": {"admit"}}
            if at == "idle" and point["precision"] == "approximate":
                modes["idle"] = {"boundary", "settle"}
            if at == "tool" and declaration.get("tool_boundary"):
                modes["tool"].add("boundary")
            return None if record["mode"] in modes.get(at, set()) else "unsupported_mode"
        caps = self._caps(resource, native)
        send = caps.get("agent.send", {})
        modes = send.get("modes", {})
        if record.get("strategy") == "interrupt_then_start":
            if record["mode"] != "preempt":
                return "unsupported_mode"
            if ("agent.interrupt" not in caps or "start" not in modes
                    or "clear" not in caps.get("agent.queue", {}).get("actions", [])):
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
        if record.get("result"):
            record["previous"] = {"attempt": (record.get("attempt") or {}).get("id"), "sha256": digest(record["result"])}
        record.update(state="claimed", reason=None, attempt=attempt, result=None)
        record.pop("retry_wait", None)
        record.pop("reason_consumer", None)
        work.put("message/" + record["delivery"], record)
        return record

    def claim_one(self, agent=None):
        """Claim one eligible input; a bounded service pool executes independent targets."""
        with self.lock:
            records = self.records()
            if not any(record["state"] in PENDING for record in records):
                return None
            work, policy = Working(self), self.policy()
            in_flight = {r["to"]["agent"] for r in records if r["state"] == "claimed"} | set(self.native_inflight)
            eligible = []
            for record in records:
                if record["state"] not in PENDING or record["to"]["agent"] in in_flight:
                    continue
                if agent is not None and record["to"]["agent"] != agent:
                    continue
                reason = self._eligibility(record, policy)
                if (reason in ("integration_unavailable", "unsupported_mode", "origin_unsupported")
                        and record.get("reason") in ("unsupported_content", "too_large_for_consumer")
                        and record.get("reason_consumer") == work.get("control")["consumers"].get(record["to"]["agent"])):
                    continue  # the native dispatcher cannot erase the active pull consumer's precise hold
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
            self.native_inflight[selected["to"]["agent"]] = selected["attempt"]["id"]
            return selected

    def execute_claim(self, record):
        try:
            return self._invoke_claim(record)
        finally:
            with self.lock:
                self.native_inflight.pop(record["to"]["agent"], None)
                self._safe_maintain()
                self._wake(record["to"]["agent"])

    def dispatch_one(self, agent=None):
        record = self.claim_one(agent)
        return self.execute_claim(record) if record is not None else None

    def _invoke_claim(self, record):
        target = record["to"]["agent"]
        _, native = self._facts(target)
        mode = record["mode"]
        caps = self._caps(self.resources().get(target, {}), native)
        if record.get("strategy") == "interrupt_then_start":
            steps = [("agent.queue", {"v": 0, "epoch": native.get("epoch"), "action": "clear"}, "queue"),
                     ("agent.interrupt", {"v": 0}, "interrupt"),
                     ("agent.send", self._send_args(record, "start"), "send:start")]
        else:
            if mode == "settle" and mode not in caps.get("agent.send", {}).get("modes", {}):
                mode = "start"
            steps = [("agent.send", self._send_args(record, mode), "send:" + record["mode"])]
        deadline = time.monotonic() + 5.0
        outcomes = []
        for op, args, right in steps:
            args = dict(args, guard=dict(record.get("guard") or {}, session=record["attempt"]["session"]))
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
                elif ((record.get("guard") or {}).get("run") is not None and facts.get("run") is not None
                      and record["guard"]["run"] != facts["run"]):
                    reason = "stale_guard"
                elif not self.policy().allowed(record["from"]["principal"], right, target):
                    reason = "no_grant"
                elif not outcomes and record.get("expires_at_ms") is not None and self.clock() >= record["expires_at_ms"]:
                    reason = "expired"
                if reason:
                    return self._finish_attempt(record, outcomes, {"outcome": "rejected", "error": {"kind": reason}}, partial=bool(outcomes), coordinated=True)
                # Persist each composition step before issuing it. A restart never runs a tail.
                work = Working(self)
                now = self._record(work, record["delivery"])
                now["attempt"]["steps"] = self._compact_steps(outcomes) + [{"op": op, "outcome": "unknown"}]
                work.put("message/" + record["delivery"], now)
                work.commit(control=True)
            reported = None
            try:
                result = self.invoke(target, op, args, max(1, int((deadline - time.monotonic()) * 1000)))
                reported = result if isinstance(result, dict) else None
                if not isinstance(result, dict) or result.get("outcome") not in ("accepted", "observed", "dispatched", "rejected", "indeterminate"):
                    result = {"outcome": "indeterminate", "error": {"kind": "invalid_native_result"}, "raw_native": result}
            except Exception as error:
                result, reported = self._rpc_outcome(error)
            coordinated = reported is None
            outcomes.append({"op": op, "outcome": result.get("outcome"),
                             "native": reported, "coordination": result if coordinated else None})
            if result["outcome"] not in ("accepted", "observed", "dispatched"):
                return self._finish_attempt(record, outcomes, result, partial=len(outcomes) > 1,
                                            coordinated=coordinated, native_reply=reported)
            if result["outcome"] == "dispatched" and len(steps) > 1 and op != "agent.send":
                # No acknowledgement that a prerequisite completed: do not run its tail.
                return self._finish_attempt(record, outcomes, result, partial=True)
        return self._finish_attempt(record, outcomes, outcomes[-1]["native"], partial=False)

    def _send_args(self, record, mode):
        args = {"v": 0, "mode": mode, "content": record["content"], "delivery": record["delivery"],
                "origin": dict(copy.deepcopy(record["origin"]), sender=dict(copy.deepcopy(record["from"]), provenance="inbox-record")),
                "quoted": record.get("quoted"), "render": record.get("render"),
                "key": record.get("key"), "guard": dict(record.get("guard") or {}, session=record["attempt"]["session"]),
                "cause": record.get("cause"), "correlation": record.get("correlation")}
        return {key: value for key, value in args.items() if value is not None}

    @staticmethod
    def _rpc_outcome(error):
        data = getattr(error, "data", None)
        reported = getattr(error, "wire_error", None)
        reported = reported if isinstance(reported, dict) else None
        outcome = data.get("outcome") if reported is not None and isinstance(data, dict) else None
        if outcome not in ("rejected", "indeterminate"):
            outcome = "indeterminate"  # a kind or missing reply is never certainty
        return ({"outcome": outcome, "error": {"kind": getattr(error, "kind", None) or "transport",
                                                "message": str(error)[:1024]}}, reported)

    @staticmethod
    def _compact(native, operation=None):
        data = native.get("data") or {}
        receipt = data.get("receipt", native.get("receipt", {})) if isinstance(data, dict) else {}
        error = native.get("error") or {}
        def tag(value):
            return value if value is None or isinstance(value, str) and len(value.encode()) <= 128 else "_oversize"
        rpc_data = data if "code" in native and "outcome" not in native and isinstance(data, dict) else {}
        compact = {"outcome": tag(native.get("outcome", rpc_data.get("outcome", "indeterminate"))), "stage": tag(receipt.get("stage")) if isinstance(receipt, dict) else None,
                   "error_kind": tag(error.get("kind", rpc_data.get("kind"))) if isinstance(error, dict) else None, "native_op": tag(operation),
                   "bytes": encoded(native), "sha256": digest(native), "incomplete": True}
        if encoded(compact) > 1024:
            raise CoordinationError("too_large", "compact outcome exceeds its 1 KiB reservation")
        return compact

    def _compact_steps(self, steps):
        return [{"op": step["op"], "outcome": step["outcome"],
                 "native" if step.get("native") is not None else "coordination":
                 self._compact(step["native"] if step.get("native") is not None else step["coordination"], step["op"])}
                for step in steps]

    def _finish_attempt(self, original, outcomes, native, *, partial, coordinated=False, native_reply=None):
        with self.lock:
            if not self.current:
                return {"state": "unknown", "reason": "owner_replaced"}
            work = Working(self)
            record = self._record(work, original["delivery"])
            if record.get("attempt", {}).get("id") != original["attempt"]["id"]:
                return {"state": "unknown", "reason": "attempt_replaced"}
            reported = native if native_reply is None else native_reply
            result = {"state": "partial" if partial else native.get("outcome", "unknown"),
                      "attempt": {"id": record["attempt"]["id"], "steps": outcomes}, "native": None if coordinated else reported}
            if coordinated:
                result["reason"] = (native.get("error") or {}).get("kind")
            observed_receipt = (record.get("result") or {}).get("receipt")
            compact_steps = self._compact_steps(outcomes)
            if compact_steps and not coordinated:
                compact_steps[-1].pop("native")
                compact_steps[-1]["native_ref"] = "result.native"
            record["attempt"]["steps"] = compact_steps
            record["result"] = dict(result, native=None if coordinated else self._compact(reported, outcomes[-1]["op"] if outcomes else None),
                                    attempt={"id": record["attempt"]["id"], "steps_ref": "attempt.steps"})
            outcome = native.get("outcome")
            kind = (native.get("error") or {}).get("kind")
            if outcome == "indeterminate" or partial:
                record.update(state="unknown" if outcome == "indeterminate" else "partial", reason="partial_recipe" if partial else "native_unknown")
            elif outcome == "rejected" and kind in ("busy", "stale", "paused", "no_grant", "stale_guard", "awaiting_route"):
                record.update(state="held", reason={"busy": "agent_busy", "stale": "stale_guard"}.get(kind, kind))
                if kind in ("stale", "stale_guard"):
                    record["review_required"] = "stale_guard"
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
            if observed_receipt:
                record["result"].update(receipt=observed_receipt, state="observed")
                result.update(receipt=observed_receipt, state="observed")
                record.update(state="accepted" if observed_receipt["stage"] == "admitted" else observed_receipt["stage"], reason=None)
            work.put("message/" + record["delivery"], record)
            try:
                work.commit(control=True)
            except (StoreError, PolicyError):
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
            accept = args.get("accept", ["text"])
            if (not isinstance(accept, list) or not 1 <= len(accept) <= 16
                    or any(not isinstance(kind, str) or not kind or len(kind.encode()) > 64 for kind in accept)
                    or len(set(accept)) != len(accept)):
                raise CoordinationError("invalid", "accept must list 1..16 distinct bounded block types")
            max_bytes = args.get("max_bytes", policy.limits.content_bytes)
            if type(max_bytes) is not int or not 1 <= max_bytes <= 256 * 1024:
                raise CoordinationError("invalid", "max_bytes must be 1..262144 UTF-8 content bytes")
            _, pull_native = self._facts(agent)
            pull_resource = self.resources().get(agent, {})
            declaration = pull_native.get("pull") or pull_resource.get("pull")
            if declaration:
                points = declaration.get("points") if isinstance(declaration, dict) else None
                if (not isinstance(points, dict) or any(not isinstance(point, dict)
                        or point.get("precision") not in ("exact", "approximate") for point in points.values())):
                    raise CoordinationError("invalid", "declaration_invalid: pull.points must map points to exact/approximate precision")
            consumer, at = args.get("consumer"), args.get("at")
            if at not in ("turn", "settle", "idle", "tool"):
                raise CoordinationError("invalid", "take must name a declared safe point")
            if not isinstance(consumer, str) or not consumer or len(consumer.encode()) > 128:
                raise CoordinationError("invalid", "bounded consumer incarnation required")
            control = work.get("control")
            if control["consumers"].get(agent) != consumer:
                raise CoordinationError("conflict", "consumer must reconcile before taking")
            if control.get("reconciling", {}).get(agent):
                return {"state": "held", "records": [], "held": "reconciling"}
            if agent not in self.permits:
                return {"state": "held", "records": [], "held": "paused"}
            records = self.records()
            if agent in self.native_inflight or any(r["to"]["agent"] == agent and r["state"] == "claimed" for r in records):
                return {"state": "held", "records": [], "held": "claim_in_flight"}
            # Leave 128 KiB for the bounded operation bucket and 16 KiB for
            # protocol/meta overhead. Holding a large record must not prevent a
            # supported later record from fitting the same atomic C1 request.
            def changed_bytes():
                return sum(encoded(row["value"]) for row in work.changes.values() if row)
            eligible, unsupported, oversized, changed = [], 0, 0, 0
            for record in records:
                if record["to"]["agent"] != agent or record["state"] not in PENDING or self._eligibility(record, policy, at=at) is not None:
                    continue
                reason = "unsupported_content" if any(block["type"] not in accept for block in record["content"]) else (
                    "too_large_for_consumer" if encoded(record["content"]) > max_bytes else None)
                if reason:
                    unsupported += reason == "unsupported_content"
                    oversized += reason == "too_large_for_consumer"
                    if changed < 40 and (record.get("reason") != reason or record.get("reason_consumer") != consumer):
                        record.update(state="held", reason=reason, reason_consumer=consumer)
                        if changed_bytes() + encoded(record) <= 48 * 1024:
                            work.put("message/" + record["delivery"], record)
                            changed += 1
                    continue
                eligible.append(record)
            supplied, claims = [], []
            for _ in range(limit):
                record = self.fair.pick(eligible)
                if record is None:
                    break
                eligible.remove(record)
                claimed = self._claim(work, record, consumer=consumer, at=at)
                if claimed is None:
                    if changed_bytes() > 112 * 1024:
                        work.changes.pop("message/" + record["delivery"], None)
                    continue
                claimed["attempt"]["take"] = digest([context["principal"], args["op"]])
                work.put("message/" + claimed["delivery"], claimed)
                row = {key: claimed.get(key) for key in ("delivery", "from", "origin", "quoted", "render", "mode", "priority", "content")}
                row["origin"] = {"principal": context["principal"], "actor": context.get("actor", context["principal"]),
                                 "kind": context.get("kind", "tool"),
                                 "sender": dict(copy.deepcopy(claimed["from"]), provenance="inbox-record")}
                if context.get("agent"):
                    row["origin"]["agent"] = context["agent"]
                row["claim"] = claimed["attempt"]["claim"]
                if encoded(supplied + [row]) > 250 * 1024 or changed_bytes() > 112 * 1024:
                    work.changes.pop("message/" + claimed["delivery"], None)
                    break
                supplied.append(row)
                claims.append({"delivery": row["delivery"], "claim": row["claim"]})
            # The first response carries content only after the whole claim batch commits.
            return {"state": "claimed" if claims else "held", "claims": claims, "records": [],
                    "_first": {"records": supplied}, "unsupported": unsupported, "oversized": oversized,
                    "held": None if claims else "unsupported_content" if unsupported else "too_large_for_consumer" if oversized else "no_eligible_input"}
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
            if (any(not isinstance(row, dict) or not isinstance(row.get("delivery"), str) for row in entries)
                    or len({row["delivery"] for row in entries}) != len(entries)):
                raise CoordinationError("invalid", "reconciliation needs distinct delivery references")
            proofs = {row["delivery"]: row.get("entry") for row in entries}
            _, native = self._facts(agent)
            resource = self.resources().get(agent, {})
            declaration = native.get("pull") or resource.get("pull") or {}
            candidates = sorted((r for r in self.records() if r["to"]["agent"] == agent
                and (r.get("attempt") or {}).get("consumer")
                and (r["state"] == "claimed" or r["state"] == "unknown"
                     and bool(proofs.get(r["delivery"])) and declaration.get("evidence") == "entry")),
                key=lambda row: (row["order"], row["delivery"]))
            changed, change_bytes = [], 0
            for record in candidates:
                entry = proofs.get(record["delivery"])
                evidenced = bool(entry) and declaration.get("evidence") == "entry"
                evidence = entry if evidenced else None
                if evidenced and encoded(entry) > 1024:
                    evidence = {"bytes": encoded(entry), "sha256": digest(entry), "incomplete": True}
                record.update(state="inserted" if evidenced else "unknown", reason=None if evidenced else "consumer_restarted")
                record["result"] = {"state": record["state"], "evidence": "entry" if evidenced else "none", "entry": evidence}
                size = encoded(record)
                if len(changed) >= 56 or change_bytes + size > 112 * 1024:
                    break
                work.put("message/" + record["delivery"], record)
                change_bytes += size
                changed.append({"delivery": record["delivery"], "state": record["state"]})
            remaining = len(candidates) - len(changed)
            control = work.get("control")
            control["consumers"][agent] = consumer  # fences old reports on the first confirmed page
            active = control.setdefault("reconciling", {})
            if remaining:
                active[agent] = consumer
            else:
                active.pop(agent, None)
            work.put("control", control)
            return {"state": "reconciling" if remaining else "reconciled", "records": changed,
                    "remaining": remaining, "next": candidates[len(changed)]["delivery"] if remaining else None}
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
                           "principal": context["principal"], "caller": context.get("author"),
                           "from": {name: context.get(name) for name in ("principal", "actor", "author", "initiator", "label")},
                           "control_epoch": self.control_epoch, "attempt": {"id": nonce("a:"), "steps": []}})
            def close():
                gates = intent.get("close_gates") or ([intent["close_gate"]] if intent.get("close_gate") else [])
                self.permits.difference_update(gates)
            return {"state": "claimed", "result_ref": key, "_after": close, "_uncertain": close}
        result = self._mutate(command, args, context, begin, control=True,
                              normal=command != "coord.inbox.archive.export")
        if result.get("historical"):
            return result
        with self.lock:
            row = self.store.get(key)
            if not self.current or row["value"]["control_epoch"] != self.control_epoch:
                return {"state": "unknown", "reason": "owner_replaced", "window": self.window()}
            intent = row["value"]["intent"]
        reported = None
        try:
            native = execute(intent)
            reported = native if isinstance(native, dict) else None
            if not isinstance(native, dict) or native.get("outcome") not in ("accepted", "observed", "dispatched", "rejected", "indeterminate"):
                native = {"outcome": "indeterminate", "error": {"kind": "invalid_native_result"}, "raw_native": native}
        except Exception as error:
            native, reported = self._rpc_outcome(error)
            if command == "coord.control" and isinstance(error, PolicyError):
                native["outcome"] = "rejected"  # local precondition, before native invocation
        coordinated = reported is None
        with self.lock:
            if not self.current:
                return {"state": "unknown", "reason": "owner_replaced"}
            work = Working(self, archive=command.startswith("coord.inbox.archive"))
            record = work.get(key)
            outcome = native.get("outcome", "accepted")
            native_kind = command == "coord.control" and intent.get("op") is not None and not coordinated
            evidence = self._compact(reported if reported is not None else native, intent.get("op") or command)
            state = "unknown" if outcome == "indeterminate" else outcome
            if intent.get("close_gate") and outcome == "rejected":
                state = "partial"  # admission closed, even though native stop had no effect
            record.update(state=state, native=evidence if native_kind else None,
                          coordination={"gate": "closed"} if intent.get("close_gate") else None)
            if not native_kind:
                record["coordination"] = evidence
            record["attempt"]["steps"] = [{"op": intent.get("op") or command, "outcome": outcome}]
            work.put(key, record)
            bucket_key = self._bucket(context["principal"], args["op"])
            bucket = work.get(bucket_key, {})
            cached = bucket.get(digest([context["principal"], args["op"]]))
            if cached and outcome != "indeterminate":
                cached["result"] = {"state": record["state"], "external": True, "native_hash": evidence["sha256"]}
                work.put(bucket_key, bucket)
            work.commit(control=True)
            self._safe_maintain()
            return dict(record, native=reported if native_kind else None,
                        coordination=record["coordination"] if native_kind else reported if reported is not None else native, window=self.window())

    def config_action(self, command, args, context):
        def prepare(work, policy):
            principal = context["principal"]
            if command == "names":
                ops = policy.names_ops(principal, args.get("handle"), args.get("agent"), expect=args.get("expect"), endpoint_principal=args.get("endpoint_principal"))
            elif command == "membership":
                ops = policy.membership_ops(principal, args.get("agent"), args.get("project"), args.get("reports_to"),
                                            expect=args.get("expect"), membership_revision=args.get("membership_revision"))
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

    def _host_kill_preview(self, agent):
        resources = self.resources()
        resource = resources.get(agent, {})
        host = ((resource.get("data") or {}).get("agent") or {}).get("host") or {}
        target = resources.get(host.get("resource"))
        caps = self.providers().get((target or {}).get("provider"), {}).get("capabilities", {})
        route = caps.get("terminal.kill")
        if (not target or target.get("available") is False or target.get("incarnation") != host.get("incarnation")
                or not isinstance(route, dict) or route.get("effect") is False):
            raise CoordinationError("unsupported", "no declared exact host termination route; never escalate silently")
        def served(endpoint):
            return ((((endpoint.get("data") or {}).get("agent") or {}).get("host")) or {}).get("resource")
        affected = sorted({agent} | {rid for rid, endpoint in resources.items()
                           if endpoint.get("kind") == "agent" and served(endpoint) == host["resource"]})
        if len(affected) > SCOPE_CAPACITY:
            raise CoordinationError("quota", "host endpoint selection exceeds declared scope capacity")
        return {"resource": host["resource"], "incarnation": host["incarnation"], "endpoints": affected}

    def control(self, args, context):
        action = args.get("action")
        if action == "inspect":
            policy = self.policy()
            agent = policy.resolve_target(args.get("target"))
            policy.require(context["principal"], "stop", agent)
            resource, native = self._facts(agent)
            try:
                host = self._host_kill_preview(agent)
            except PolicyError:
                host = None
            return {"state": "preview", "agent": agent, "graceful": "agent.stop" in self._caps(resource or {}, native),
                    "confirm_host": host, "warning": "Host kill affects all host work, including unreported work; endpoint list is an observed snapshot."}
        actions = {"interrupt": ("interrupt", "agent.interrupt"), "respond": ("answer", "agent.respond"),
                   "queue": ("queue", "agent.queue"), "session": ("session", "agent.session"),
                   "configure": ("configure", "agent.configure"), "command": ("command", "agent.command"),
                   "object": ("command", "agent.object"), "stop": ("stop", "agent.stop"), "retire": ("stop", None)}
        if action == "retire":
            def administrative(work, policy):
                agent = policy.resolve_target(args.get("target"))
                policy.require(context["principal"], "stop", agent)
                self._ensure_agent(work, agent, policy)
                stone = self._agent_get(work, agent)
                options = args.get("args") if args.get("args") is not None else {}
                if not isinstance(options, dict):
                    raise CoordinationError("invalid", "retire args must be an object")
                reason = options.get("reason")
                if reason is not None and (not isinstance(reason, str) or len(reason.encode()) > 512):
                    raise CoordinationError("invalid", "retire reason must be bounded text")
                stone.update(lifecycle="retired", retired={"by": context["principal"], "reason": reason})
                self._agent_put(work, agent, stone)
                control = work.get("control")
                control["gates"][agent] = False
                control["revision"] += 1
                work.put("control", control)
                close = lambda: self.permits.discard(agent)
                return {"state": "retired", "execution": "unknown", "native": None, "_after": close, "_uncertain": close}
            return self._mutate("coord.control", args, context, administrative, control=True)
        right, operation = actions.get(action, (None, None))
        def prepare(work, policy):
            if right is None:
                raise CoordinationError("invalid", "unknown control action")
            if args.get("args") is not None and not isinstance(args["args"], dict):
                raise CoordinationError("invalid", "native control args must be an object")
            version = (args.get("args") or {}).get("v", 0)
            if type(version) is not int or version != 0:
                raise CoordinationError("unsupported", "native control version is unsupported")
            agent = policy.resolve_target(args.get("target"))
            policy.require(context["principal"], right, agent)
            resource, native = self._facts(agent)
            if not resource and action != "stop":
                raise CoordinationError("unavailable", "target is absent")
            resource = resource or {}
            self._ensure_agent(work, agent, policy)
            if action in ("stop", "retire"):
                control = work.get("control")
                control["gates"][agent] = False
                control["revision"] += 1
                work.put("control", control)
            method, target = operation, agent
            native_args = dict(args.get("args") or {}, v=0)
            if method == "agent.queue":
                native_args.setdefault("epoch", native.get("epoch"))
            caps = self._caps(resource, native)
            how = native_args.pop("how", "graceful") if action == "stop" else None
            if action == "stop" and how not in ("graceful", "host_kill"):
                raise CoordinationError("unsupported", "unknown stop strategy; no weaker fallback")
            confirmation, close_gates = None, [agent] if action == "stop" else []
            if action == "stop" and how == "host_kill":
                confirmation = self._host_kill_preview(agent)
                if args.get("confirm_host") != confirmation:
                    raise CoordinationError("conflict", "inspect and explicitly confirm the exact host/endpoint snapshot")
                close_gates = confirmation["endpoints"]
                for endpoint in close_gates:
                    policy.require(context["principal"], "stop", endpoint)
                    self._ensure_agent(work, endpoint, policy)
                    control["gates"][endpoint] = False
                work.put("control", control)
                method, target, native_args = "terminal.kill", confirmation["resource"], {}
            elif operation is not None and operation not in caps:
                if action == "stop":
                    method = None
                else:
                    raise CoordinationError("unsupported", "native control is undeclared")
            return {"agent": agent, "target": target, "incarnation": resource.get("incarnation"), "op": method, "args": native_args,
                    "close_gate": agent if action == "stop" else None, "close_gates": close_gates,
                    "host_confirmation": confirmation, "right": right}
        def execute(intent):
            if intent["op"] is None:
                raise CoordinationError("unsupported", "admission closed; native stop was not invoked; execution remains unknown")
            resource, _ = self._facts(intent["agent"])
            if not resource or resource.get("incarnation") != intent["incarnation"]:
                raise CoordinationError("stale", "endpoint changed before native control")
            policy = self.policy()
            for endpoint in intent.get("close_gates") or [intent["agent"]]:
                policy.require(context["principal"], intent["right"], endpoint)
            if intent.get("host_confirmation") and self._host_kill_preview(intent["agent"]) != intent["host_confirmation"]:
                raise CoordinationError("stale", "host/endpoint snapshot changed before termination")
            return self.invoke(intent["target"], intent["op"], intent["args"], 5000)
        return self._external("coord.control", args, context, prepare, execute)

    def handover(self, args, context):
        if args.get("action") == "stage":
            return self._handover_stage(args, context)
        def transfer(work, policy):
            if args.get("action") == "abort":
                plan_id = str(args.get("plan"))
                plan = work.get("plan/" + plan_id)
                if not plan:
                    raise CoordinationError("not_found", "unknown handover plan")
                if plan["state"] in ("completed", "aborted"):
                    raise CoordinationError("conflict", "handover is already final")
                for target in (plan["from"], plan["to"]):
                    policy.require(context["principal"], "handover", target["agent"])
                for delivery in plan["deliveries"]:
                    record = self._record(work, delivery)
                    if record["state"] not in PENDING:
                        if record.get("handover_plan") == plan_id:
                            record.pop("handover_plan")
                            work.put("message/" + delivery, record)
                        continue
                    if record.get("handover_plan") != plan_id:
                        raise CoordinationError("conflict", "handover record changed")
                    record["to"] = copy.deepcopy(record["handed_over_from"])
                    record.pop("handover_plan")
                    record.update(state="held", reason="handover_aborted")
                    work.put("message/" + delivery, record)
                plan.update(state="aborted", aborted_ms=self.clock(), configuration="not_reverted")
                work.put("plan/" + plan_id, plan)
                return {"state": "aborted", "plan": plan_id, "configuration": "not_reverted"}
            if args.get("action") == "complete":
                plan = work.get("plan/" + str(args.get("plan")))
                if not plan:
                    raise CoordinationError("not_found", "unknown handover plan")
                if plan["state"] == "aborted":
                    raise CoordinationError("conflict", "handover was aborted")
                destination = plan["to"]
                policy.require(context["principal"], "handover", destination["agent"])
                resource, native = self._facts(destination["agent"])
                if not resource or resource.get("incarnation", "") != destination["incarnation"] or native.get("lifecycle") == "ended":
                    raise CoordinationError("conflict", "successor changed or ended")
                if any(stage["state"] != "accepted" for stage in plan["stages"]):
                    raise CoordinationError("conflict", "handover still has unfinished or uncertain ordinary stages")
                successor_session = (native.get("session") or {}).get("id")
                for delivery in plan["deliveries"]:
                    record = self._record(work, delivery)
                    if record["state"] not in PENDING:
                        if record.get("handover_plan") == args["plan"]:
                            record.pop("handover_plan")
                            work.put("message/" + delivery, record)
                        continue
                    if record.get("handover_plan") != args["plan"]:
                        raise CoordinationError("conflict", "handover record changed")
                    record.pop("handover_plan")
                    record["to"]["session"] = successor_session
                    record["reason"] = "paused" if destination["agent"] not in self.permits else "awaiting_route"
                    work.put("message/" + delivery, record)
                plan["state"] = "completed"
                plan["to"]["session"] = successor_session
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
            stone = self._agent_get(work, source["agent"], {})
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
            if plan["state"] == "aborted":
                raise CoordinationError("conflict", "handover was aborted")
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
            resources, config, records = self.resources(), self.config(), self.records()
            counts = {}
            for record in records:
                held, unknown = counts.setdefault(record["to"]["agent"], [0, 0])
                counts[record["to"]["agent"]] = [held + (record["state"] in PENDING), unknown + (record["state"] in ("unknown", "claimed", "partial"))]
            handles = {}
            for key, row in config.items():
                if key.startswith("doc/agents/names/"):
                    handles.setdefault(row["value"].get("agent"), []).append(key[len("doc/agents/names/"):])
            for agent, old in list(self.tombstones().items()):
                resource = resources.get(agent)
                native = ((resource or {}).get("data") or {}).get("agent") or {}
                stone = copy.deepcopy(old)
                if native.get("lifecycle") == "ended" or stone.get("lifecycle") == "ended":
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
                stone["held"], stone["unknown"] = counts.get(agent, [0, 0])
                member = config.get("doc/agents/members/" + agent)
                stone["membership_ref"] = {"rN": agent, "revision": member["version"]} if member else None
                stone["handles"] = sorted(handles.get(agent, []))[:4]
                if stone != old:
                    work = Working(self)
                    self._agent_put(work, agent, stone)
                    work.commit(control=True)
            # Only evidence-free, unreferenced tombstones are eligible for pruning.
            candidates = sorted((row for row in self.tombstones().values() if row["lifecycle"] in ("unavailable", "ended")
                                 and not row["held"] and not row["unknown"] and not row["membership_ref"]),
                                key=lambda row: (row["last_seen_ms"] is not None, row["last_seen_ms"] or 0))
            plans = [row["value"] for key, row in self.store.snapshot().items() if key.startswith("plan/") and row["value"]["state"] != "completed"]
            for stone in candidates:
                if any(stone["agent"] in (plan["from"]["agent"], plan["to"]["agent"]) for plan in plans):
                    continue
                # Keep a useful bounded tail; pressure never drops protected evidence.
                if len(self.tombstones()) <= 256:
                    break
                work = Working(self)
                key = self._agent_key(stone["agent"])
                page = work.get(key, {})
                page.pop(stone["agent"], None)
                work.put(key, page)
                work.commit(control=True)
        if forget:
            for agent, incarnation in forgotten:
                forget(agent, incarnation)
        return forgotten

    def observe_receipts(self, agent, incarnation, projections):
        """Only typed, correlated adapter receipt observations; never text/completion inference."""
        if not isinstance(projections, list) or len(projections) > 256:
            raise CoordinationError("too_large", "receipt projection batch exceeds its bound")
        changed_any = False
        with self.lock:
            resource, native = self._facts(agent)
            if not self.current or not resource or resource.get("incarnation", "") != incarnation:
                return False
            send = self._caps(resource, native).get("agent.send", {})
            allowed = {
                None: {"admitted", "queued", "inserted", "handled", "supplied", "read", "completed"},
                "admitted": {"queued", "inserted", "handled", "supplied", "read", "completed"},
                "queued": {"inserted", "handled", "supplied", "read", "completed"},
                "inserted": {"read", "completed"}, "supplied": {"read", "completed"},
                "read": {"completed"}, "handled": set(), "completed": set(),
            }
            for offset in range(0, len(projections), 48):
                work = Working(self)
                settled = work.get("settled", {})
                settled_changed = False
                changed = []
                for event in projections[offset:offset + 48]:
                    receipt = event.get("receipt") or {}
                    delivery, stage = receipt.get("delivery"), receipt.get("stage")
                    if stage not in allowed[None] or not isinstance(delivery, str):
                        continue
                    if stage in ("inserted", "read", "completed") and send.get("correlation") != "insertion":
                        continue
                    if stage == "handled" and send.get("handled", "none") == "none":
                        continue
                    record = work.get("message/" + delivery)
                    if not record:
                        marker = settled.get(delivery)
                        if marker and marker.get("agent") == agent and marker.get("state") not in ("withdrawn", "expired", "superseded", "rejected"):
                            old = marker.get("state")
                            old = "admitted" if old in ("accepted", "dispatched") else old
                            if stage in allowed.get(old, set()):
                                marker["state"] = stage
                                settled_changed = True
                        continue
                    if record["to"]["agent"] != agent or record["state"] in PENDING | {"withdrawn", "expired", "superseded", "rejected", "resolved"}:
                        continue
                    result = record.get("result") or {}
                    old = (result.get("receipt") or {}).get("stage") or (result.get("native") or {}).get("stage")
                    if stage not in allowed.get(old, set()):
                        continue
                    if stage == "supplied" and "context" not in (record.get("render") or []):
                        continue
                    projected = {"delivery": delivery, "stage": stage, "source": {"epoch": event.get("epoch"), "seq": event.get("seq")},
                                 "sha256": digest(receipt), "incomplete": True}
                    if isinstance(receipt.get("run"), str) and len(receipt["run"].encode()) <= 128:
                        projected["run"] = receipt["run"]
                    if encoded(projected) > 1024:
                        raise CoordinationError("too_large", "receipt evidence exceeds its reserved compact bound")
                    result["receipt"] = projected
                    result["state"] = "observed"
                    record.update(state="accepted" if stage == "admitted" else stage, reason=None, result=result)
                    record.pop("final_seq", None)
                    work.put("message/" + delivery, record)
                    changed.append(delivery)
                if settled_changed:
                    work.put("settled", settled)
                work.commit(control=True)
                for delivery in changed:
                    self.attention.pop(delivery, None)
                changed_any = changed_any or bool(changed) or settled_changed
            self._safe_maintain()
        return changed_any

    def _safe_maintain(self):
        try:
            self.maintenance()
            self.maintenance_error = None
        except (StoreError, PolicyError) as error:
            self.maintenance_error = {"kind": getattr(error, "kind", "unknown"), "message": str(error)[:512]}

    def maintenance(self):
        """Bound decided history; never discard unresolved evidence or reopen intent."""
        if not self.current:
            return
        work = Working(self)
        finals = []
        frozen = {record_key for name, row in work.rows.items() if name.startswith("archive/")
                  and row["value"].get("state") in ("frozen", "exporting", "exported", "partial")
                  for record_key in row["value"].get("frozen", {})}
        for key, row in work.rows.items():
            value = row["value"]
            if key.startswith("message/") and value.get("state") not in OPEN:
                if (value.get("attempt") or {}).get("id") == self.native_inflight.get(value["to"]["agent"]) and value["to"]["agent"] in self.native_inflight:
                    continue
                pull = (value.get("attempt") or {}).get("consumer")
                if pull and value["state"] not in ("read", "handled", "completed", "resolved", "withdrawn", "expired", "superseded", "rejected"):
                    continue  # insertion/context supply is not a completed pull report
                if key in frozen:
                    continue
                finals.append((value.get("final_seq", row["version"]), key, row))
            elif key.startswith("external/") and value.get("state") in ("accepted", "observed", "dispatched", "rejected", "partial"):
                if key not in frozen:
                    finals.append((value.get("final_seq", row["version"]), key, row))
        finals.sort(key=lambda item: (item[0], item[1]))
        settled = work.get("settled", {})
        excess = max(0, len(finals) - 256)
        for _, key, row in finals[:min(excess, 30)]:
            value = row["value"]
            if key.startswith("message/"):
                settled.pop(value["delivery"], None)
                settled[value["delivery"]] = {"state": value["state"], "agent": value["to"]["agent"], "incomplete": True}
            work.delete(key)
        if len(settled) > 1024:
            for delivery in list(settled)[:len(settled) - 1024]:
                settled.pop(delivery)
        if settled != work.get("settled", {}):
            work.put("settled", settled)
        # Compact retained native outcomes, not active pull tokens or unknown claims.
        for _, key, row in finals[excess:]:
            if len(work.changes) >= 45:
                break
            value = row["value"]
            if value.get("incomplete"):
                continue
            if key.startswith("external/"):
                intent = value.get("intent") or {}
                compact = {name: copy.deepcopy(value.get(name)) for name in ("state", "command", "principal", "caller", "native", "coordination")}
                compact.update(intent={name: intent[name] for name in ("agent", "target", "op") if name in intent},
                               incomplete=True, final_seq=value.get("final_seq", row["version"]))
                if encoded(compact) > 1024:
                    compact["intent"] = {"agent": intent.get("agent")}
                if encoded(compact) > 1024:
                    raise CoordinationError("too_large", "compact control metadata exceeds its bound; original retained")
                work.put(key, compact)
                continue
            result = value.get("result") or {}
            compact = {name: copy.deepcopy(value.get(name)) for name in ("v", "delivery", "submission", "intent", "mode", "priority", "order", "state", "reason")}
            compact.update(to={"agent": value["to"]["agent"]}, **{"from": {"principal": value["from"]["principal"]}},
                           incomplete=True, final_seq=value.get("final_seq", row["version"]),
                           content_hash=digest(value.get("content", [])),
                           attempt={"id": (value.get("attempt") or {}).get("id")},
                           result={"state": result.get("state", value["state"]), "native": result.get("native"),
                                   "receipt": result.get("receipt"), "incomplete": True})
            if value.get("handed_over_from"):
                compact["handed_over_from"] = {"agent": value["handed_over_from"]["agent"]}
            if encoded(compact) > 1024:
                compact.pop("content_hash", None)
                compact.pop("attempt", None)
                compact["result"] = {"state": value["state"], "sha256": digest(result), "incomplete": True}
            if encoded(compact) > 1024:
                raise CoordinationError("too_large", "compact final metadata exceeds its explicit bound; original retained")
            work.put(key, compact)
        refs = {entry["result"].get("result_ref") for key, row in work.rows.items() if key.startswith("ops/")
                for entry in row["value"].values() if isinstance(entry.get("result"), dict)}
        for key in work.rows:
            if len(work.changes) >= 60:
                break
            if key.startswith("result/") and key not in refs:
                work.delete(key)
        work.commit(control=True)

    def _archive_frozen(self, key):
        return any(key in row["value"].get("frozen", {}) for name, row in self.store.view().items()
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
                    work = Working(self, archive=True)
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
