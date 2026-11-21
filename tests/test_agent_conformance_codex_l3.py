"""Self-tests for the Part A L3 many-host replacement branch: each realistic failure must FAIL, the correct run PASS."""
import asyncio
from copy import deepcopy
import time
import importlib
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

import agent_conformance.cases as cases  # load the shared registry first; then the row module
from agent_conformance.results import CheckFailed, HarnessGap

m = importlib.import_module("agent_conformance.codex_l3_cases")

A, B = "inc-a", "inc-b"


def facts(incarnation, state="idle", run=None, lifecycle="live", completion=None, session="thread-1"):
    return {"state": state, "run": run, "lifecycle": lifecycle, "completion": completion, "session": {"id": session},
            "host": {"id": "codex:" + incarnation, "provider": "codex-x", "instance": "x:" + incarnation,
                     "incarnation": incarnation, "kind": "server", "owns_lifecycle": True},
            "caps": {"host": {"kind": "server", "endpoints": "many"}}}


def valid_obs():
    after = {"r6": facts(B, "running", "turn-1"), "r7": facts(B), "r8": facts(B)}
    work = m.identity_of("r6", facts(B, "running", "turn-1"))
    return {"old_incarnation": A, "new_incarnation": B, "old_provider": "codex-x", "new_provider": "codex-x",
            "old_ids": ["r2", "r3", "r4"], "new_ids": ["r6", "r7", "r8"], "same_image": True,
            "work_before": work, "work_after": dict(work),
            "own_end": {rid: facts(B, lifecycle="ended") for rid in ("r6", "r7", "r8")},
            "own_host": {"exited": True, "ledger": ["exit"], "exit_ns": 1000},
            "exit_fence": {"epoch": "e", "seq": 10},
            "arrivals": [{"epoch": "e", "seq": 9, "ns": 900, "id": "r6", "lifecycle": "live", "incarnation": B}]
                        + [{"epoch": "e", "seq": 11 + i, "ns": 1100, "id": rid, "lifecycle": "ended", "incarnation": B}
                           for i, rid in enumerate(("r6", "r7", "r8"))],
            "after_end": after, "available_after": {rid: True for rid in after},
            "old_after": {"r2": None, "r3": None, "r4": None},
            "settled": facts(B, "idle", None, completion={"reason": "completed"}),
            "history": [("r6", facts(B, "running", "turn-1"))], "order": True,
            "late_return": {"old_native_rows": ["exit"], "old_bridge_resumed": True,
                            "old_bridge_live_after_window": False,
                            "provider_after": {"instance": "x:" + B, "state": "connected"}}}


def failing(obs):
    return [message for ok, message, _ in m.grade_replacement(obs) if not ok]


class GradeReplacement(unittest.TestCase):
    def test_valid_replacement_passes(self):
        self.assertEqual(failing(valid_obs()), [])

    def test_realistic_failures_fail(self):
        def late_end_ends_new(o): o["after_end"]["r7"] = facts(B, lifecycle="ended")
        def new_unavailable(o): o["available_after"]["r8"] = False
        def new_endpoint_gone(o): o["after_end"]["r8"] = None
        def work_lost(o): o["after_end"]["r6"] = facts(B); o["work_after"] = m.identity_of("r6", facts(B))
        def run_replaced(o): o["work_after"] = m.identity_of("r6", facts(B, "running", "turn-9"))
        def session_switched(o): o["work_after"] = m.identity_of("r6", facts(B, "running", "turn-1", session="thread-9"))
        def run_rebound_to_old_host(o): o["work_after"] = m.identity_of("r6", facts(A, "running", "turn-1"))
        def own_end_not_ended(o): o["own_end"]["r7"] = facts(B)
        def own_end_misattributed(o): o["own_end"]["r8"] = facts(A, lifecycle="ended")
        def other_image(o): o["same_image"] = False
        def own_host_still_live(o): o["own_host"]["exited"] = False
        def own_host_no_exit_row(o): o["own_host"]["ledger"] = []
        def ended_before_own_exit(o): o["arrivals"].insert(1, {"epoch": "e", "seq": 10, "ns": 5000, "id": "r7", "lifecycle": "ended", "incarnation": B})
        def ended_old_attribution_before_exit(o): o["arrivals"].insert(1, {"epoch": "e", "seq": 3, "ns": 5000, "id": "r8", "lifecycle": "ended", "incarnation": A})
        def no_exit_time(o): o["exit_fence"] = {}
        def own_end_never_observed_after_fence(o): o["arrivals"] = o["arrivals"][:1]
        def fence_from_another_epoch(o): o["exit_fence"]["epoch"] = "other"
        def arrival_log_truncated(o): o["arrivals"] = o["arrivals"] * (m.MAX_OBSERVED // 4 + 1)
        def never_completes(o): o["settled"] = facts(B, "running", "turn-1")
        def ended_attributed_to_new(o): o["history"].append(("r2", facts(B, lifecycle="ended")))
        def old_resurrected(o): o["old_after"]["r3"] = (True, facts(A))
        def old_took_provider(o): o["late_return"]["provider_after"] = {"instance": "x:" + A, "state": "connected"}
        def provider_lost(o): o["late_return"]["provider_after"] = {"instance": "x:" + B, "state": "disconnected"}
        def not_a_replacement(o): o["new_incarnation"] = A
        def old_ids_reused(o): o["new_ids"] = ["r2", "r7", "r8"]
        def old_host_never_ended(o): o["late_return"]["old_native_rows"] = []
        def wrong_order(o): o["order"] = False
        cases_ = {late_end_ends_new: "adapter", new_unavailable: "adapter", new_endpoint_gone: "adapter",
                  work_lost: "adapter", run_replaced: "adapter", never_completes: "adapter",
                  session_switched: "adapter", run_rebound_to_old_host: "adapter", own_end_not_ended: "adapter",
                  own_end_misattributed: "adapter", other_image: "fixture",
                  own_host_still_live: "fixture", own_host_no_exit_row: "fixture",
                  ended_before_own_exit: "adapter", ended_old_attribution_before_exit: "adapter",
                  no_exit_time: "fixture", arrival_log_truncated: "fixture",
                  own_end_never_observed_after_fence: "fixture", fence_from_another_epoch: "fixture",
                  ended_attributed_to_new: "adapter", old_resurrected: "adapter", old_took_provider: "adapter",
                  provider_lost: "adapter", not_a_replacement: "harness", old_ids_reused: "harness",
                  old_host_never_ended: "harness", wrong_order: "harness"}
        for mutate, component in cases_.items():
            with self.subTest(mutate.__name__):
                obs = valid_obs(); mutate(obs)
                bad = [c for ok, _, c in m.grade_replacement(obs) if not ok]
                self.assertTrue(bad, "must fail")
                self.assertIn(component, bad)

    def test_old_bridge_still_retrying_is_not_graded(self):
        obs = valid_obs(); obs["late_return"]["old_bridge_live_after_window"] = True
        self.assertEqual(failing(obs), [])


class Helpers(unittest.TestCase):
    def test_replacement_argv(self):
        argv = ["/bin/bridge", "--slots", "3", "--instance", "k", "--", "py", "fx", "--control", "/r/c.sock", "--ledger", "/r/n.jsonl"]
        out = m.replacement_argv(argv, ["/r/c.sock", "/r/n.jsonl"], {"/r/c.sock": "/r/c3.sock", "/r/n.jsonl": "/r/l3.jsonl"})
        self.assertEqual(out[-3:], ["/r/c3.sock", "--ledger", "/r/l3.jsonl"]); self.assertEqual(out[:5], argv[:5])
        self.assertIsNone(m.replacement_argv([a for a in argv if a not in ("--instance", "k")], ["/r/c.sock", "/r/n.jsonl"], {}))
        self.assertIsNone(m.replacement_argv(argv, ["/r/other.sock"], {}))
        self.assertIsNone(m.replacement_argv(argv + ["/r/c.sock"], ["/r/c.sock", "/r/n.jsonl"], {}), "path must occur once")

    def test_replacement_group_is_validated_before_any_send(self):
        def row(rid, provider, incarnation, available=True):
            return {"id": rid, "kind": "agent", "provider": provider, "available": available, "data": {"agent": facts(incarnation)}}
        old = ["r2", "r3", "r4"]
        rows = [row("r5", "other-provider", "inc-c"), row("r6", "codex-x", B), row("r7", "codex-x", B), row("r8", "codex-x", B)]
        self.assertEqual([r["id"] for r in m.replacement_group(rows, "codex-x", A, old)], ["r6", "r7", "r8"], "foreign r5 ignored")
        self.assertIsNone(m.replacement_group(rows[:3], "codex-x", A, old), "too few same-provider endpoints")
        mixed = rows + [row("r9", "codex-x", "inc-d")]
        self.assertIsNone(m.replacement_group(mixed, "codex-x", A, old), "mixed new incarnations are not one replacement")
        self.assertIsNone(m.replacement_group([row("r6", "other", B), row("r7", "other", B), row("r8", "other", B)],
                                              "codex-x", A, old), "another provider is never a replacement")

    def test_branch_selection(self):
        self.assertTrue(m.is_many_server({"kind": "server", "endpoints": "many"}, m.host_of(facts(A))))
        process = {"host": {"kind": "process", "owns_lifecycle": True}}
        self.assertFalse(m.is_many_server({"kind": "process", "endpoints": "one"}, m.host_of(process)))
        self.assertFalse(m.is_many_server({"kind": "server", "endpoints": "one"}, m.host_of(facts(A))))
        unowned = facts(A); unowned["host"]["owns_lifecycle"] = False
        self.assertFalse(m.is_many_server({"kind": "server", "endpoints": "many"}, m.host_of(unowned)))

    def test_no_case_registration(self):
        self.assertIsNot(cases.HANDLERS.get("L3"), getattr(m, "many_host_replacement"))
        self.assertIs(cases.HANDLERS.get("L3"), cases.host_lifecycle, "L3 stays the shared handler; K1 delegates")

    def test_wrong_host_is_refused_not_run(self):
        ctx = SimpleNamespace(facts=AsyncMock(return_value={"caps": {"host": {"kind": "process", "endpoints": "one"}},
                                                           "host": {"kind": "process", "owns_lifecycle": True}}))
        with self.assertRaises(HarnessGap):
            asyncio.run(m.many_host_replacement(ctx))


class SimulatedCore:
    """A tiny public-surface simulation for whole-handler controls: two bridges under one provider name, the core's
    one-live-connection rule, and a scenario hook deciding what the old incarnation's late return does."""

    def __init__(self, late_return, own_end=None, foreign=False, native_b_lives=False, before_own_exit=None):
        self.before_own_exit = before_own_exit or (lambda core: None)
        self.late_return, self.native_b_lives, self.sends = late_return, native_b_lives, []
        self.own_end = own_end or SimulatedCore.ends_replacement_endpoints
        self.resources = {"r1": {"id": "r1", "kind": "terminal", "provider": "herdr", "available": True}}
        for rid in ("r2", "r3", "r4"):
            self.resources[rid] = {"id": rid, "kind": "agent", "provider": "codex-x", "available": True,
                                   "data": {"agent": facts(A)}}
        self.provider = {"instance": "x:" + A, "generation": 1, "state": "connected"}
        self.history = []
        self.foreign = foreign
        if foreign:  # an unrelated available agent of another provider, present from the start
            self.resources["r5"] = {"id": "r5", "kind": "agent", "provider": "other-provider", "available": True,
                                    "data": {"agent": facts("inc-c")}}

    @staticmethod
    def ends_replacement_endpoints(core):  # the correct owns_lifecycle end of the replacement host
        for rid in ("r6", "r7", "r8"):
            core.resources[rid]["data"]["agent"]["lifecycle"] = "ended"
            core.publish(rid)

    observers = ()
    seq = 0
    buffered = False   # deliver to observers late, in order (an asynchronous, delayed observer)
    held = ()

    def publish(self, rid):
        self.seq += 1
        self.history.append(("e", self.seq, deepcopy(self.resources[rid]), 1))
        event = {"type": "resource", "epoch": "e", "seq": self.seq, "data": deepcopy(self.resources[rid])}
        if self.buffered:
            self.held = (*self.held, event)
        else:
            for callback in self.observers:  # public resource events reach every subscribed observer, in order
                callback(event)

    def flush(self):
        for event in self.held:
            for callback in self.observers:
                callback(event)
        self.held = ()

    async def call(self, method, params=None):
        params = params or {}
        if method == "command.invoke":
            for rid in ("r2", "r3", "r4"):
                if rid in self.resources:
                    self.resources[rid]["available"] = not params["args"]["on"]
            self.provider["state"] = "disconnected" if params["args"]["on"] else "connected"
            return {}
        if method == "workspace.snapshot":
            return {"epoch": "e", "seq": self.seq,
                    "providers": {"herdr": {"state": "connected"}, "codex-x": dict(self.provider)}}
        if method == "resource.list":
            return {"resources": deepcopy(list(self.resources.values()))}
        if method == "resource.spawn":
            for rid in ("r2", "r3", "r4"):
                self.resources.pop(rid)  # the core retires the old incarnation's identities as replaced
            for rid in ("r6", "r7", "r8"):
                self.resources[rid] = {"id": rid, "kind": "agent", "provider": "codex-x", "available": True,
                                       "data": {"agent": facts(B)}}
            self.provider = {"instance": "x:" + B, "generation": 2, "state": "connected"}
            return {"outcome": "observed", "resource": {"backend_ref": "t2"}}
        raise AssertionError(method)


def run_control(late_return, own_end=None, foreign=False, native_b_lives=False, before_own_exit=None, buffered=False):
    core = SimulatedCore(late_return, own_end, foreign, native_b_lives, before_own_exit)
    core.buffered = buffered
    class Observer:
        def on(self, method, callback): core.observers = (*core.observers, callback)
        async def call(self, method, params=None):
            return {"epoch": "e", "seq": core.seq} if method == "workspace.snapshot" else {}
    async def viewer(): return Observer()
    import signal as _signal
    def signal_owned(process, sig):  # bridge B (pid 98) frozen: nothing it publishes until SIGCONT
        if process and process.get("pid") == 98 and sig == _signal.SIGCONT and getattr(core, "own_end_pending", False):
            core.own_end_pending = False
            core.own_end(core)
            core.flush()  # a delayed observer receives everything, in order, only now
        return True
    ledger = []
    class NativeA:
        endpoint, path = "/r/c.sock", "/r/native.jsonl"
        def mark(self): return len(ledger)
        def records(self, kind=None, after=0): return ledger[after:]
        async def call(self, verb, **kw):
            if verb == "host_exit":
                ledger.append({"kind": "exit"})
                core.late_return(core)
            return {}
    ledger_b = []
    class NativeB:
        def __init__(self, endpoint, path): self.endpoint, self.path = endpoint, path
        def mark(self): return len(ledger_b)
        def records(self, kind=None, after=0): return ledger_b[after:]
        async def call(self, verb, **kw):
            if verb == "state": return {"pid": 99}
            if verb == "host_exit":
                core.before_own_exit(core)  # whatever arrives while the exit request is still in flight
                ledger_b.append({"kind": "exit", "ns": time.monotonic_ns()})
                core.own_end_pending = True  # the bridge publishes its own end once it runs again
            if verb == "turn" and core.resources.get("r6", {}).get("available"):
                agent = core.resources["r6"]["data"]["agent"]
                if agent["state"] == "running":
                    agent.update(state="idle", run=None, completion={"reason": "completed"})
            return {}
    async def wait_facts(predicate, rid=None, timeout=2):
        agent = (core.resources.get(rid) or {}).get("data", {}).get("agent") or {}
        if agent and predicate(agent): return deepcopy(agent)
        raise CheckFailed("timeout")
    async def send(text, rid=None, **kw):
        core.sends.append(rid)
        core.resources[rid]["data"]["agent"].update(state="running", run="turn-1")
    async def lifecycle_wait(ctx, predicate, description):
        if not predicate(): raise CheckFailed(description)
    resource_cache = {}
    async def peer_wait(predicate, timeout=2):
        await listing()
        value = predicate()
        if not value: raise asyncio.TimeoutError()
        return value
    async def listing():
        rows = (await core.call("resource.list"))["resources"]
        resource_cache.update({row["id"]: row for row in rows})
        return rows
    async def get_facts(rid=None):
        return deepcopy(core.resources[rid or "r2"]["data"]["agent"])
    ctx = SimpleNamespace(
        root="/r", argv=["/bin/bridge", "--slots", "3", "--instance", "x", "--", "py", "--control", "/r/c.sock",
                         "--ledger", "/r/native.jsonl"],
        native=NativeA(), agents=[{"id": rid, "provider": "codex-x"} for rid in ("r2", "r3", "r4")], host="r1",
        metadata={"adapter_image": {"pid": 7, "birth": "1", "sha256": "img"}}, evidence=[], resource_history=core.history,
        resource_cache=resource_cache, peer=SimpleNamespace(call=core.call, wait=peer_wait),
        backend=SimpleNamespace(hc=lambda *a: {"pid": 98}, tracked=[]),
        resources=listing, facts=get_facts, ensure_session=AsyncMock(), send=send, wait_facts=wait_facts,
        viewer=viewer)
    async def cache_then(ctx_, predicate, description):
        await listing(); return await lifecycle_wait(ctx_, predicate, description)
    with patch.object(m, "Native", NativeB), patch.object(m, "signal_owned", signal_owned), \
         patch.object(m, "SETUP_S", 0.2), \
         patch.object(m, "executable", lambda process: {"sha256": "img"}), patch.object(m, "ledger_summary", lambda n: {}), \
         patch.object(cases, "lifecycle_wait", cache_then), patch.object(m.asyncio, "sleep", AsyncMock()), \
         patch("pi_provider_e2e.identity", lambda pid: {"pid": pid, "birth": "1"}), \
         patch("pi_provider_e2e.live", lambda process: core.native_b_lives and process.get("pid") == 99):
        try:
            asyncio.run(m.many_host_replacement(ctx))
        finally:
            ctx.sends = core.sends
    return ctx


class WholeHandlerControls(unittest.TestCase):
    """The real L3 handler against the simulated core: only the correct late return is accepted."""

    def accepts(self, late_return, own_end=None, **kw):
        try:
            run_control(late_return, own_end, **kw); return True
        except CheckFailed:
            return False

    def test_controls(self):
        def fenced(core):
            pass  # the old incarnation's late end is refused; nothing on the replacement changes
        def ends_replacement(core):
            for rid in ("r6", "r7", "r8"):
                core.resources[rid]["data"]["agent"]["lifecycle"] = "ended"; core.publish(rid)
        def drops_work(core):
            core.resources["r6"]["data"]["agent"].update(state="idle", run=None); core.publish("r6")
        def takes_provider_back(core):
            core.provider = {"instance": "x:" + A, "generation": 3, "state": "connected"}
            for rid in ("r6", "r7", "r8"):
                core.resources[rid]["available"] = False
        def resurrects_old(core):
            core.resources["r3"] = {"id": "r3", "kind": "agent", "provider": "codex-x", "available": True,
                                    "data": {"agent": facts(A)}}
        def misattributes_end(core):
            ended = deepcopy(core.resources["r6"]); ended["data"]["agent"]["lifecycle"] = "ended"
            core.history.append((None, 99, ended, 1))
        controls = {fenced: True, ends_replacement: False, drops_work: False, takes_provider_back: False,
                    resurrects_old: False, misattributes_end: False}
        for late_return, want in controls.items():
            with self.subTest(late_return.__name__):
                self.assertEqual(self.accepts(late_return), want)
        def own_host_end_ignored(core): pass
        def own_host_end_attributed_to_old(core):
            for rid in ("r6", "r7", "r8"):
                agent = core.resources[rid]["data"]["agent"]
                agent["lifecycle"] = "ended"; agent["host"] = facts(A)["host"]
        for own_end in (own_host_end_ignored, own_host_end_attributed_to_old):
            with self.subTest(own_end.__name__):
                self.assertFalse(self.accepts(fenced, own_end))

    def test_delayed_old_end_in_flight_before_own_exit_is_refused(self):
        def delayed_old_end(core):  # #94 L3 finding: arrives while the own host_exit request is in flight
            for rid in ("r6", "r7", "r8"):
                core.resources[rid]["data"]["agent"]["lifecycle"] = "ended"
                core.publish(rid)
        with self.assertRaises(CheckFailed) as caught:
            run_control(lambda core: None, before_own_exit=delayed_old_end)
        self.assertIn("PUBLISHED before the replacement's own end could exist", str(caught.exception))
        self.assertEqual(getattr(caught.exception, "component", None), "adapter")

    def test_buffered_observer_cannot_hide_a_premature_end(self):
        """vqro-lead-secondary corrections-l3-delay-finding: an observer that receives every event late, in order,
        must still see the early ends as premature (ordered by core publication seq, not arrival)."""
        def delayed_old_end(core):
            for rid in ("r6", "r7", "r8"):
                core.resources[rid]["data"]["agent"]["lifecycle"] = "ended"
                core.publish(rid)
        with self.assertRaises(CheckFailed) as caught:
            run_control(lambda core: None, before_own_exit=delayed_old_end, buffered=True)
        self.assertEqual(getattr(caught.exception, "component", None), "adapter")
        run_control(lambda core: None, buffered=True)  # a healthy but delayed observer still passes the genuine run

    def test_replacement_native_still_live_is_never_green(self):
        with self.assertRaises(CheckFailed) as caught:
            run_control(lambda core: None, native_b_lives=True)
        self.assertEqual(getattr(caught.exception, "component", None), "fixture")

    def test_foreign_agent_is_never_sent_work(self):
        ctx = run_control(lambda core: None, foreign=True)
        self.assertEqual(ctx.sends, ["r6"], "only the validated replacement endpoint receives work")

    def test_evidence_records_the_late_return(self):
        ctx = run_control(lambda core: None)
        row = [e for e in ctx.evidence if e["kind"] == "l3_replacement"][0]
        self.assertEqual((row["new_ids"], row["work_before"]["endpoint"], row["work_before"]["incarnation"],
                          row["work_before"]["run"], row["late_return"]["old_native_rows"]),
                         (["r6", "r7", "r8"], "r6", B, "turn-1", ["exit"]))
        self.assertTrue(all(v["lifecycle"] == "ended" and v["incarnation"] == B for v in row["own_end"].values()))


if __name__ == "__main__":
    unittest.main()
