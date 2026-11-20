"""Pure coordination-v0 policy over one captured configuration snapshot.

No I/O, dispatch, claims or principal inference live here. The owner supplies the
core-resolved effective principal and commits returned ordinary-config CAS plans.
Plans must be revalidated/serialized by that owner against configuration changes;
per-key CAS alone is not a cross-entry read fence. Delivery state is elsewhere.
"""
import base64
import copy
from collections import OrderedDict
from dataclasses import dataclass
import hashlib
import json
import re


NAMES = "doc/agents/names/"
MEMBERS = "doc/agents/members/"
GRANTS = "doc/agents/grants/"
TERMINALS = "doc/agents/terminals/"
OWNER = "doc/principals/owner"
HANDLE = re.compile(r"@[a-z0-9][a-z0-9-]{0,31}(?:/[a-z0-9][a-z0-9-]{0,31}){0,2}\Z")
AGENT = re.compile(r"r[1-9][0-9]*\Z")
MODES = frozenset(("start", "boundary", "settle", "preempt", "admit"))
RIGHTS = frozenset(("send:*", "send:interrupt_then_start", "receive", "interrupt",
                    "answer", "queue", "session", "configure", "command",
                    "inbox.edit_any", "recover", "handover", "names", "membership",
                    "pause", "resume", "stop", "grants.manage")) | {"send:" + m for m in MODES}
OPEN = frozenset(("recorded", "held", "claimed", "unknown"))
MAX_CONFIG = 16384
MAX_MEMBERS = 4096
MAX_GRANTS = 1024
MAX_PAGE = 64
PAGE_BYTES = 256 * 1024
DEFAULT_EXPIRY_MS = 5 * 60 * 1000


class PolicyError(Exception):
    def __init__(self, kind, message, data=None, **details):
        super().__init__(message)
        self.kind = kind
        self.data = dict(data or {}, **details)


def _fail(kind, message, **data):
    raise PolicyError(kind, message, data)


def encoded(value):
    """The compact UTF-8 payload charge, never character count or ASCII escapes."""
    try:
        return json.dumps(value, ensure_ascii=False, allow_nan=False,
                          separators=(",", ":"), sort_keys=True).encode("utf-8")
    except (TypeError, ValueError, RecursionError, UnicodeError):
        _fail("invalid", "value must be finite, encodable JSON")


def _string(value, name, limit=128):
    try:
        valid = isinstance(value, str) and value and len(value.encode("utf-8")) <= limit
    except UnicodeError:
        valid = False
    if not valid:
        _fail("invalid", name + " must be a non-empty bounded string")
    return value


def _integer(value, name, minimum=0, maximum=2 ** 63 - 1):
    if type(value) is not int or not minimum <= value <= maximum:
        _fail("invalid", name + " must be a bounded integer")
    return value


def _agent(value):
    if not isinstance(value, str) or not AGENT.fullmatch(value):
        _fail("invalid", "expected exact resource id rN")
    return value


def _handle(value):
    if not isinstance(value, str) or not HANDLE.fullmatch(value):
        _fail("invalid", "invalid agent handle")
    return value


def _member(value):
    if not isinstance(value, dict):
        _fail("invalid", "membership row must be an object")
    project, parent = value.get("project"), value.get("reports_to")
    if project is not None:
        _string(project, "project")
    if parent is not None:
        _agent(parent)
    return {"project": project, "reports_to": parent}


@dataclass(frozen=True)
class QuotaLimits:
    domain_bytes: int = 4 * 1024 * 1024
    domain_records: int = 1024
    sender_target_records: int = 8
    sender_target_bytes: int = 128 * 1024
    sender_records: int = 64
    sender_bytes: int = 512 * 1024
    target_records: int = 64
    target_bytes: int = 512 * 1024
    content_bytes: int = 32 * 1024
    metadata_bytes: int = 2 * 1024
    outcome_bytes: int = 1024
    scope_capacity: int = 1024

    def __post_init__(self):
        for name, value in vars(self).items():
            _integer(value, name, 1)
        if self.scope_capacity < 200:
            _fail("invalid", "scope capacity must support at least 200 agents")

    @property
    def normal_bytes(self):
        return self.domain_bytes - (self.domain_bytes + 9) // 10

    @property
    def normal_records(self):
        return self.domain_records - (self.domain_records + 9) // 10


def record_charge(record, limits=None):
    """Reserve metadata plus the eventual outcome before any claim is admitted."""
    limits = limits or QuotaLimits()
    content = len(encoded(record.get("content", [])))
    steps = _integer(record.get("outcome_steps", 1), "outcome_steps", 1, 3)
    # Existing derived metadata is not unused outcome headroom. Reserve the
    # future compact step evidence in addition to the larger actual/floor base.
    return max(len(encoded(record)), content + limits.metadata_bytes) + steps * limits.outcome_bytes


class Policy:
    def __init__(self, entries, resources, now_ms, *, tombstones=None, limits=None):
        if not isinstance(entries, dict) or not isinstance(resources, dict):
            _fail("invalid", "policy needs captured entry and resource maps")
        if len(entries) > MAX_CONFIG or len(resources) > MAX_MEMBERS:
            _fail("quota", "policy snapshot exceeds declared traversal bound")
        self.entries = copy.deepcopy(entries)
        self.resources = copy.deepcopy(resources)
        self.tombstones = copy.deepcopy(tombstones or {})
        if not isinstance(self.tombstones, dict) or len(self.tombstones) > MAX_MEMBERS:
            _fail("quota", "tombstone traversal bound")
        for rid, tombstone in self.tombstones.items():
            _agent(rid)
            self.resources.setdefault(rid, {"id": rid, "kind": "agent", **tombstone})
        if len(self.resources) > MAX_MEMBERS:
            _fail("quota", "combined agent traversal bound")
        self.now_ms = _integer(now_ms, "now_ms")
        self.limits = limits or QuotaLimits()
        self.owner = self._value(OWNER)
        self.names = {}
        self.members = {}
        self.grants = []
        self.markers = set()
        for key, entry in self.entries.items():
            if not isinstance(key, str) or not isinstance(entry, dict) or "value" not in entry:
                _fail("invalid", "invalid captured entry")
            value = entry["value"]
            if key.startswith(NAMES):
                handle = _handle(key[len(NAMES):])
                if not isinstance(value, dict):
                    _fail("invalid", "name binding must be an object")
                _agent(value.get("agent"))
                if value.get("principal") is not None:
                    _string(value["principal"], "principal", 256)
                self.names[handle] = value
            elif key.startswith(MEMBERS):
                self.members[_agent(key[len(MEMBERS):])] = _member(value)
            elif key.startswith(GRANTS):
                self._validate_grant(value)
                self.grants.append(value)
            elif key.startswith(TERMINALS):
                self.markers.add(_agent(key[len(TERMINALS):]))
        if len(self.members) > MAX_MEMBERS or len(self.grants) > MAX_GRANTS:
            _fail("quota", "configuration exceeds declared row bound")
        counts = {}
        for row in self.names.values():
            counts[row["agent"]] = counts.get(row["agent"], 0) + 1
        if any(count > 4 for count in counts.values()):
            _fail("quota", "at most four handles per agent")
        self._validate_forest(self.members)
        membership_versions = [(k, e.get("version")) for k, e in sorted(self.entries.items())
                               if k.startswith(MEMBERS)]
        self.membership_revision = hashlib.sha256(encoded(membership_versions)).hexdigest()
        self.agents = set(self.members) | self.markers | set(counts)
        for rid, resource in self.resources.items():
            status = resource.get("status") or {}
            if resource.get("kind") == "agent" or status.get("app"):
                self.agents.add(_agent(rid))
        self._principals = {rid: set() for rid in self.agents}
        for rid in self.agents:
            resource = self.resources.get(rid, {})
            agent = (resource.get("data") or {}).get("agent") or {}
            principal = resource.get("principal") or (agent.get("principal") if isinstance(agent, dict) else None)
            if isinstance(principal, str) and principal:
                self._principals[rid].add(principal)
        for row in self.names.values():
            if row.get("principal"):
                self._principals[row["agent"]].add(row["principal"])
        self._principal_agents = {}
        for rid, principals in self._principals.items():
            for principal in principals:
                self._principal_agents.setdefault(principal, set()).add(rid)

    def _value(self, key):
        return self.entries.get(key, {}).get("value")

    def _version(self, key):
        return self.entries.get(key, {}).get("version", 0)

    @staticmethod
    def _validate_forest(members):
        done = set()
        for start in members:
            path = set()
            node = start
            while node is not None and node not in done:
                if node in path:
                    _fail("invalid", "membership cycle")
                path.add(node)
                row = members[node]
                parent = row["reports_to"]
                if parent is not None:
                    if parent not in members:
                        _fail("invalid", "membership parent is not configured")
                    if members[parent]["project"] != row["project"]:
                        _fail("invalid", "parent and child must share a project")
                node = parent
            done.update(path)

    def _subtree(self, root, members=None):
        members = self.members if members is None else members
        children = {}
        for rid, row in members.items():
            children.setdefault(row["reports_to"], []).append(rid)
        result, pending = [], [root]
        while pending:
            rid = pending.pop()
            result.append(rid)
            if len(result) > MAX_MEMBERS:
                _fail("quota", "membership traversal bound")
            pending.extend(reversed(sorted(children.get(rid, []))))
        return result

    def _target(self, address):
        if isinstance(address, str) and address.startswith("@"):
            handle = _handle(address)
            if handle not in self.names:
                _fail("not_found", "unknown handle")
            return self.names[handle]["agent"]
        if isinstance(address, str) and address.startswith("agent:"):
            address = address[6:]
        return _agent(address)

    def resolve(self, address):
        rid = self._target(address)
        if rid not in self.agents:
            _fail("not_found", "agent is not known")
        resource = self.resources.get(rid, {})
        agent = (resource.get("data") or {}).get("agent") or {}
        lifecycle = agent.get("lifecycle") if isinstance(agent, dict) else None
        lifecycle = resource.get("lifecycle", lifecycle)
        if resource.get("state") in ("retired", "ended"):
            lifecycle = resource["state"]
        stone = self.tombstones.get(rid, {})
        if lifecycle == "ended" or resource.get("state") == "finished" or stone.get("lifecycle", stone.get("state")) == "ended":
            _fail("ended", "agent has ended")
        if lifecycle == "retired" or stone.get("lifecycle", stone.get("state")) == "retired":
            _fail("retired", "agent is retired")
        return rid

    def scope(self, scope, limit=None):
        capacity = self.limits.scope_capacity if limit is None else min(
            _integer(limit, "scope limit", 1), self.limits.scope_capacity)
        if scope == "workspace":
            chosen = set(self.agents)
        elif isinstance(scope, str) and scope.startswith("project:"):
            project = _string(scope[8:], "project")
            chosen = {rid for rid, row in self.members.items() if row["project"] == project}
        elif isinstance(scope, str) and scope.startswith("team:"):
            root = _agent(scope[5:])
            if root not in self.agents:
                _fail("not_found", "unknown team coordinator")
            chosen = set(self._subtree(root))
        elif isinstance(scope, str) and (scope.startswith("agent:") or AGENT.fullmatch(scope)):
            chosen = {self._target(scope)}
            if not chosen <= self.agents:
                _fail("not_found", "unknown agent")
        else:
            _fail("invalid", "invalid scope")
        if len(chosen) > capacity:
            _fail("quota", "scope capacity exceeded", capacity=capacity)
        # All parent links were validated at construction. Iterative DFS avoids
        # Python recursion limits even for adversarially deep but legal teams.
        roots = sorted(rid for rid in chosen if self.members.get(rid, {}).get("reports_to") not in chosen)
        ordered = []
        children = {}
        for rid in chosen:
            children.setdefault(self.members.get(rid, {}).get("reports_to"), []).append(rid)
        pending = list(reversed(roots))
        while pending:
            rid = pending.pop()
            ordered.append(rid)
            pending.extend(reversed(sorted(children.get(rid, []))))
        return ordered

    def _selector(self, value, *, subject=False):
        _string(value, "grant selector", 256)
        if value == "*":
            return
        if value.startswith("@"):
            _handle(value)
        elif value.startswith("agent:"):
            if subject:
                # Exact principal strings may be agent:rN too.
                return
            _agent(value[6:])
        elif value.startswith("team:"):
            _agent(value[5:])
        elif value.startswith("project:"):
            _string(value[8:], "project")
        elif not subject:
            _fail("invalid", "invalid grant target")

    def _validate_grant(self, row):
        if not isinstance(row, dict):
            _fail("invalid", "grant must be an object")
        self._selector(row.get("who"), subject=True)
        self._selector(row.get("on"))
        rights = row.get("can")
        if not isinstance(rights, list) or not rights or len(rights) > len(RIGHTS):
            _fail("invalid", "grant needs a bounded list of rights")
        if any(not isinstance(r, str) or r not in RIGHTS for r in rights):
            _fail("invalid", "unknown coordination right")
        if len(set(rights)) != len(rights):
            _fail("invalid", "duplicate grant right")
        if row.get("until_ms") is not None:
            _integer(row["until_ms"], "until_ms")

    def _subject_matches(self, selector, principal):
        if selector == "*":
            return True
        if selector.startswith("@"):
            binding = self.names.get(selector)
            principals = self._principals.get(binding["agent"], set()) if binding else set()
            return len(principals) == 1 and principal in principals
        if selector.startswith(("team:", "project:")):
            return any(self._contains(selector, "agent:" + rid)
                       for rid in self._principal_agents.get(principal, ()))
        return selector == principal

    def _contains(self, container, requested):
        """Structural containment: a leaf today does not authorize a future team."""
        if container == "*" or container == requested:
            return True
        if requested == "*" or requested.startswith("@"):
            return False
        if requested.startswith("project:"):
            return False
        if requested.startswith("team:"):
            if not container.startswith(("team:", "project:")):
                return False
            return self._contains(container, "agent:" + requested[5:])
        try:
            rid = self._target(requested)
            if container.startswith("@"):
                return self._target(container) == rid
            if container.startswith("agent:"):
                return container[6:] == rid
            if container.startswith("project:"):
                return self.members.get(rid, {}).get("project") == container[8:]
            if container.startswith("team:"):
                root = container[5:]
                # Grant checks are bounded ancestry walks, not scope actions:
                # a large team may reject bulk resume but still grant one member.
                for _ in range(MAX_MEMBERS + 1):
                    if rid == root:
                        return True
                    rid = self.members.get(rid, {}).get("reports_to")
                    if rid is None:
                        return False
            return False
        except PolicyError:
            return False

    def _has_scope(self, principal, right, target, *, through=None):
        if not isinstance(principal, str) or principal in ("", "undeclared"):
            return False
        if principal == self.owner:
            return True
        for row in self.grants:
            expiry = row.get("until_ms")
            if expiry is not None and expiry <= self.now_ms:
                continue
            if through is not None and expiry is not None and (through is False or expiry < through):
                continue
            if not self._subject_matches(row["who"], principal):
                continue
            if right not in row["can"] and not (right.startswith("send:") and "send:*" in row["can"]):
                continue
            if self._contains(row["on"], target):
                return True
        return False

    def has(self, principal, right, target):
        if not isinstance(right, str):
            return False
        rid = self._target(target)
        return self._has_scope(principal, right, "agent:" + rid)

    def require(self, principal, right, target):
        if not self.has(principal, right, target):
            _fail("no_grant", "coordination right not granted", right=right, target=target)

    def validate_send(self, args):
        if not isinstance(args, dict):
            _fail("invalid", "send arguments must be an object")
        normalized = copy.deepcopy(args)
        mode = args.get("mode")
        if not isinstance(mode, str) or mode not in MODES:
            _fail("invalid", "unknown delivery mode")
        strategy = args.get("strategy")
        if strategy is not None and (strategy != "interrupt_then_start" or mode != "preempt"):
            _fail("invalid", "strategy is not valid for this mode")
        content = args.get("content")
        if not isinstance(content, list) or not content or len(content) > 256:
            _fail("invalid", "content must be a non-empty bounded block array")
        for block in content:
            if not isinstance(block, dict) or not isinstance(block.get("type"), str):
                _fail("invalid", "each content block needs a type")
            kind = block["type"]
            if kind == "text" and not isinstance(block.get("text"), str):
                _fail("invalid", "text block needs text")
            if kind in ("image", "audio"):
                _string(block.get("mime"), "mime", 256)
                if not isinstance(block.get("data"), str):
                    _fail("invalid", "media block needs encoded data")
            if kind in ("resource_link", "resource"):
                _string(block.get("uri"), "resource uri", 4096)
            if kind not in ("text", "image", "audio", "resource_link", "resource"):
                _fail("unsupported", "unsupported content block type")
        content_bytes = len(encoded(content))
        if content_bytes > self.limits.content_bytes:
            _fail("quota", "content exceeds byte limit", limit=self.limits.content_bytes)
        metadata = {k: v for k, v in normalized.items() if k != "content"}
        if len(encoded(metadata)) > self.limits.metadata_bytes:
            _fail("quota", "message metadata exceeds byte limit")
        expiry = args.get("expires_at_ms")
        if expiry is None and mode in ("boundary", "preempt"):
            expiry = self.now_ms + DEFAULT_EXPIRY_MS
        if expiry is not None:
            _integer(expiry, "expires_at_ms")
            if expiry <= self.now_ms:
                _fail("expired", "message expiry already passed")
            normalized["expires_at_ms"] = expiry
        if type(args.get("follow_current_session", False)) is not bool:
            _fail("invalid", "follow_current_session must be boolean")
        guard = args.get("guard")
        if guard is not None and not isinstance(guard, dict):
            _fail("invalid", "guard must be an object")
        if args.get("follow_current_session") and guard and guard.get("session") is not None:
            _fail("invalid", "follow_current_session conflicts with a session guard")
        if "send_new" in args and type(args["send_new"]) is not bool:
            _fail("invalid", "send_new must be boolean")
        if args.get("render") is not None:
            render = args["render"]
            if not isinstance(render, list) or not render or len(render) > 16 or any(not isinstance(r, str) for r in render):
                _fail("invalid", "render must be a bounded list")
        return normalized

    def check_quota(self, records, candidate, *, reserved_bytes=0, reserved_records=0):
        """Check open-message caps; extra reservations include non-message state.

        The protected store remains the authority for exact aggregate charging and
        control admission. This helper never evicts evidence to make a send fit.
        """
        records = list(records.values()) if isinstance(records, dict) else records
        if not isinstance(records, (list, tuple)) or len(records) > self.limits.domain_records:
            _fail("quota", "domain record count exceeded")
        _integer(reserved_bytes, "reserved_bytes")
        _integer(reserved_records, "reserved_records")
        if len(encoded(candidate.get("content", []))) > self.limits.content_bytes:
            _fail("quota", "content exceeds byte limit")
        principal = candidate.get("from", {}).get("principal")
        target = candidate.get("to", {}).get("agent")
        _string(principal, "sender principal", 256)
        _agent(target)
        replacing = candidate.get("supersedes")
        rows = [r for r in records if r.get("state") in OPEN and r.get("delivery") != replacing]
        # Supersession reduces open quotas only for the caller's same-target
        # editable intent. It cannot hide another sender's or claimed evidence.
        if replacing is not None:
            prior = next((r for r in records if r.get("delivery") == replacing), None)
            if not prior or prior.get("from", {}).get("principal") != principal or prior.get("to", {}).get("agent") != target or prior.get("state") not in ("recorded", "held"):
                _fail("conflict", "supersedes must name own pending same-target record")
        charge = record_charge(candidate, self.limits)
        selectors = (
            ("sender_target", lambda r: r.get("from", {}).get("principal") == principal and r.get("to", {}).get("agent") == target),
            ("sender", lambda r: r.get("from", {}).get("principal") == principal),
            ("target", lambda r: r.get("to", {}).get("agent") == target),
        )
        for prefix, selected in selectors:
            matching = [r for r in rows if selected(r)]
            if len(matching) + 1 > getattr(self.limits, prefix + "_records") or sum(record_charge(r, self.limits) for r in matching) + charge > getattr(self.limits, prefix + "_bytes"):
                _fail("quota", prefix + " open-message quota exceeded", quota=prefix)
        # Count every retained record, not only pending ones. Final records,
        # unknown evidence and op results do not disappear merely for admission.
        if len(records) + reserved_records + 1 > self.limits.normal_records:
            _fail("quota", "normal record budget would consume control reserve")
        total = sum(record_charge(r, self.limits) if r.get("state") in OPEN else len(encoded(r)) for r in records)
        if total + reserved_bytes + charge > self.limits.normal_bytes:
            _fail("quota", "normal byte budget would consume control reserve")

    def config_ops(self, kind, args, principal):
        """Return one atomic ordinary workspace plan; never apply it here."""
        if not isinstance(args, dict):
            _fail("invalid", "configuration arguments must be an object")
        if kind in ("coord.names", "agents.names"):
            return self._names_ops(args, principal)
        if kind in ("coord.membership.set", "membership.set"):
            return self._membership_ops(args, principal)
        if kind in ("coord.grants", "agents.grants"):
            return self._grant_ops(args, principal)
        _fail("invalid", "unknown configuration command")

    # Public API used by the coordination engine. The compact methods above also
    # remain useful for tests/command adapters; neither path does I/O.
    resolve_target = resolve
    allowed = has

    def names_ops(self, principal, handle, agent, *, expect, endpoint_principal=None):
        return self._names_ops({"handle": handle, "agent": agent, "expect": expect,
                                "principal": endpoint_principal}, principal)

    def membership_ops(self, principal, agent, project, reports_to, *, expect):
        return self._membership_ops({"agent": agent, "project": project,
                                     "reports_to": reports_to, "expect": expect}, principal)

    def grant_ops(self, principal, identifier, value, *, expect):
        return self._grant_ops({"id": identifier, "value": value, "expect": expect}, principal)

    def _cas(self, key, value, expect):
        _integer(expect, "expect")
        if expect != self._version(key):
            _fail("conflict", "configuration revision changed")
        if value is None:
            return {"op": "delete", "key": key, "expect": expect}
        return {"op": "set", "key": key, "value": copy.deepcopy(value), "expect": expect}

    def _names_ops(self, args, principal):
        handle = _handle(args.get("handle"))
        key = NAMES + handle
        previous = self.names.get(handle)
        agent = args.get("agent")
        if previous:
            self.require(principal, "names", previous["agent"])
        if agent is None:
            if not previous:
                _fail("not_found", "handle is not bound")
            return [self._cas(key, None, args.get("expect"))]
        _agent(agent)
        if agent not in self.agents:
            _fail("not_found", "name target is not a known agent")
        self.require(principal, "names", agent)
        count = sum(row["agent"] == agent for name, row in self.names.items() if name != handle)
        if count >= 4:
            _fail("quota", "at most four handles per agent")
        value = {"agent": agent}
        if args.get("principal") is not None:
            value["principal"] = _string(args["principal"], "bound principal", 256)
        return [self._cas(key, value, args.get("expect"))]

    def _membership_ops(self, args, principal):
        agent = _agent(args.get("agent"))
        if agent not in self.agents:
            _fail("not_found", "unknown membership target")
        expected_revision = args.get("membership_revision")
        if expected_revision is not None and expected_revision != self.membership_revision:
            _fail("conflict", "membership selection changed")
        subtree = self._subtree(agent)
        old = self.members.get(agent, {"project": None, "reports_to": None})
        deleting = args.get("action") == "delete"
        if deleting and len(subtree) > 1:
            _fail("invalid", "cannot delete membership of a coordinator with reports")
        new = None if deleting else _member(args)
        for rid in subtree:
            self.require(principal, "membership", rid)
        # Sources/destinations are their coordinator, or their project for roots.
        for row in (old, new):
            if row is None:
                continue
            if row["reports_to"]:
                self.require(principal, "membership", row["reports_to"])
            elif row["project"] is not None and not self._has_scope(principal, "membership", "project:" + row["project"]):
                _fail("no_grant", "membership right missing on source or destination project")
        candidate = copy.deepcopy(self.members)
        if deleting:
            candidate.pop(agent, None)
        else:
            candidate[agent] = new
            for rid in subtree[1:]:
                candidate[rid]["project"] = new["project"]
        if len(candidate) > MAX_MEMBERS:
            _fail("quota", "membership row limit")
        self._validate_forest(candidate)
        ops = [self._cas(MEMBERS + agent, new, args.get("expect"))]
        for rid in subtree[1:]:
            if candidate[rid] != self.members[rid]:
                ops.append(self._cas(MEMBERS + rid, candidate[rid], self._version(MEMBERS + rid)))
        return ops

    def _grant_ops(self, args, principal):
        identifier = _string(args.get("id"), "grant id")
        if "/" in identifier:
            _fail("invalid", "grant id must be one path segment")
        key = GRANTS + identifier
        old = self._value(key)
        value = args.get("value")
        if old is None and value is None:
            _fail("not_found", "unknown grant")
        if value is not None:
            self._validate_grant(value)
            value = copy.deepcopy(value)
            # Public commands pin aliases at issuance; a later name rebind never
            # transfers an existing grant. Team/project membership stays dynamic.
            if value["on"].startswith("@"):
                value["on"] = "agent:" + self._target(value["on"])
            if value["who"].startswith("@"):
                rid = self._target(value["who"])
                principals = self._principals.get(rid, set())
                if len(principals) != 1:
                    _fail("invalid", "handle has no unambiguous explicit principal binding")
                value["who"] = next(iter(principals))
        for row in (old, value):
            if row is None:
                continue
            # Existing rights need standing to revoke; new rights also may not
            # outlive the issuer's grant. Owner is the explicit unbounded case.
            through = None if row is old else row.get("until_ms", None)
            through = False if row is not old and through is None else through
            if not self._has_scope(principal, "grants.manage", row["on"], through=through):
                _fail("no_grant", "cannot manage that grant scope or duration")
            for right in row["can"]:
                if not self._has_scope(principal, right, row["on"], through=through):
                    _fail("no_grant", "cannot grant or revoke rights not held", right=right)
        if old is None and len(self.grants) >= MAX_GRANTS:
            _fail("quota", "grant row limit")
        return [self._cas(key, value, args.get("expect"))]


def bounded_page(items, page=None, *, key=lambda row: row["delivery"], limit=64,
                 byte_limit=PAGE_BYTES, revision=None):
    """Deterministic keyset page; a changed cut refuses, never mixes snapshots.

    The caller's revision must qualify its query/filter as well as state. When
    omitted, a canonical content hash supplies a cut for small captured lists.
    Cursors are data, not authority; malformed/stale cursors never mutate state.
    """
    if not isinstance(items, (list, tuple)) or len(items) > MAX_CONFIG:
        _fail("quota", "list traversal bound")
    _integer(limit, "limit", 1, MAX_PAGE)
    _integer(byte_limit, "byte_limit", 1, PAGE_BYTES)
    def page_key(value):
        if isinstance(value, list):
            value = tuple(value)
        parts = value if isinstance(value, tuple) else (value,)
        if not parts or len(parts) > 4 or any(type(p) not in (str, int) for p in parts) or len(encoded(value)) > 512:
            _fail("invalid", "list key must be a bounded string, integer or tuple")
        return value

    try:
        keys = [page_key(key(row)) for row in items]
    except (KeyError, TypeError):
        _fail("invalid", "list item has no key")
    shapes = {tuple(type(p).__name__ for p in k) if isinstance(k, tuple) else type(k).__name__ for k in keys}
    if len(shapes) > 1 or len(set(keys)) != len(keys):
        _fail("invalid", "list needs unique, comparably typed keys")
    pairs = sorted(zip(keys, items), key=lambda pair: pair[0])
    keys = [pair[0] for pair in pairs]
    rows = [pair[1] for pair in pairs]
    if revision is None:
        revision = hashlib.sha256(encoded(rows)).hexdigest()
    _string(revision, "revision", 256)
    after = None
    if page is not None:
        try:
            if not isinstance(page, str) or len(page) > 4096:
                raise ValueError()
            token = json.loads(base64.b64decode(page, altchars=b"-_", validate=True))
            if set(token) != {"v", "revision", "after"} or type(token["v"]) is not int or token["v"] != 0:
                raise ValueError()
            token["after"] = page_key(token["after"])
        except (ValueError, TypeError, UnicodeError):
            _fail("invalid", "malformed page cursor")
        if token["revision"] != revision:
            _fail("conflict", "list changed; restart pagination")
        after = token["after"]
        if after not in keys:
            _fail("conflict", "page anchor no longer exists")

    def cursor(last):
        return base64.urlsafe_b64encode(encoded({"v": 0, "revision": revision, "after": last})).decode("ascii")

    selected = []
    start = 0 if after is None else keys.index(after) + 1
    for index in range(start, min(start + limit, len(rows))):
        candidate = selected + [copy.deepcopy(rows[index])]
        next_page = cursor(keys[index]) if index + 1 < len(rows) else None
        result = {"items": candidate, "next": next_page, "revision": revision}
        if len(encoded(result)) > byte_limit:
            if not selected:
                _fail("too_large", "one list item exceeds page byte bound")
            break
        selected = candidate
    next_page = cursor(key(selected[-1])) if selected and start + len(selected) < len(rows) else None
    result = {"items": selected, "next": next_page, "revision": revision}
    if len(encoded(result)) > byte_limit:
        _fail("too_large", "page envelope exceeds byte bound")
    return result


def check_admission(records, principal, target, content, *, reserved_bytes=0,
                    reserved_records=0, replaces=None, limits=None, outcome_steps=1):
    """Pure reservation preflight for the engine; never authorizes a dispatch.

    `reserved_*` is total captured store occupancy, including existing messages
    (gates, summaries, op results, plans included). Stored message bytes are
    subtracted before adding their outstanding outcome reservations, avoiding a
    double charge. One to three compact outcome steps are reserved per record;
    a recipe explicitly requests three. A successful return is a reservation,
    not a store commit.
    """
    _integer(outcome_steps, "outcome_steps", 1, 3)
    candidate = {"from": {"principal": principal}, "to": {"agent": target},
                 "content": content, "state": "recorded", "outcome_steps": outcome_steps}
    if replaces is not None:
        candidate["supersedes"] = replaces
    policy = Policy({}, {}, now_ms=0, limits=limits)
    rows = list(records.values()) if isinstance(records, dict) else records
    if not isinstance(rows, (list, tuple)) or len(rows) > policy.limits.domain_records:
        _fail("quota", "domain record count exceeded")
    _integer(reserved_bytes, "reserved_bytes")
    _integer(reserved_records, "reserved_records")
    extra_bytes = max(0, reserved_bytes - sum(len(encoded(row)) for row in rows))
    extra_records = max(0, reserved_records - len(rows))
    policy.check_quota(rows, candidate, reserved_bytes=extra_bytes,
                       reserved_records=extra_records)
    return {"bytes": record_charge(candidate, policy.limits), "records": 1,
            "content_bytes": len(encoded(content))}


class FairQueue:
    """Select among already eligible records; gates/unknown dependencies are not guessed.

    Urgent lanes precede idle-waiting lanes. Within each lane, senders alternate
    round-robin, retaining sender/target/lane order. Mixed-target input is also
    round-robin by target. The owner claims the returned record atomically and
    calls again after any lost race; this selector itself mutates no record.
    """
    lanes = ("boundary", "preempt", "admit", "start", "settle")

    def __init__(self):
        self._last_sender = OrderedDict()
        self._last_target = {}

    @staticmethod
    def _next(values, after):
        ordered = sorted(values)
        return next((value for value in ordered if after is None or value > after), ordered[0])

    def pick(self, eligible_records):
        if not isinstance(eligible_records, (list, tuple)) or len(eligible_records) > 1024:
            _fail("quota", "eligible selector input exceeds domain bound")
        for lane in self.lanes:
            rows = [row for row in eligible_records if row.get("mode") == lane]
            if not rows:
                continue
            targets = {row.get("to", {}).get("agent") for row in rows}
            for target in targets:
                _agent(target)
            target = self._next(targets, self._last_target.get(lane))
            rows = [row for row in rows if row["to"]["agent"] == target]
            senders = {row.get("from", {}).get("principal") for row in rows}
            for sender in senders:
                _string(sender, "sender principal", 256)
            cursor = (target, lane)
            sender = self._next(senders, self._last_sender.get(cursor))
            rows = [row for row in rows if row["from"]["principal"] == sender]
            for row in rows:
                _integer(row.get("order"), "order")
                _string(row.get("delivery"), "delivery")
            chosen = min(rows, key=lambda row: (row["order"], row["delivery"]))
            self._last_target[lane] = target
            self._last_sender[cursor] = sender
            self._last_sender.move_to_end(cursor)
            while len(self._last_sender) > MAX_MEMBERS * len(self.lanes):
                self._last_sender.popitem(last=False)
            return chosen
        return None
