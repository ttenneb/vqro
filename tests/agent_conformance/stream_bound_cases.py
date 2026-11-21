"""Part A O12 (producer append bounds) and O14 (incompatible viewers) handlers.

O14 targets the PRODUCTION viewer: the agents extension's Conversation/Agents.attach over the real vqro_items.ItemView,
fed by a tiny scripted test provider over the public provider protocol. Nothing emulates the viewer's back-off or its
incompatibility report; a product that lacks them fails. O12's producer half is inherited regression evidence and its
adapter half observes a real bridge refusal; either missing is an honest harness gap, never a pass.
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import time

from .cases import case
from .results import HarnessGap, check

# ---------------------------------------------------------------------------------------------------------------------
# O14 pure checkers (unit-tested with negatives in tests/test_agent_conformance_stream_bounds.py)

BACKOFF_WINDOW_S = 10.0     # observation window after the first incompatible record
MAX_OPENS_IN_WINDOW = 3     # the initial open plus at most two bounded retries


def no_reattach_loop(opens, window=BACKOFF_WINDOW_S, max_opens=MAX_OPENS_IN_WINDOW):
    """O14: within the window after the first open the viewer opens at most `max_opens` times (the first open plus a
    bounded retry), never a reattach loop. `opens` are provider-observed monotonic times (seconds)."""
    if not opens:
        return False
    return len([t for t in opens if t - opens[0] <= window]) <= max_opens


def cursor_never_advanced(afters, last_good_seq, epoch):
    """O14: every reopen continues from at most the last good record of the same epoch, or asks for a reset (null);
    it never skips the incompatible record by advancing past `last_good_seq`."""
    for after in afters:
        if after is None:
            continue
        if not isinstance(after, dict) or after.get("epoch") != epoch or not isinstance(after.get("seq"), int):
            return False
        if after["seq"] > last_good_seq:
            return False
    return True


def incompatibility_reported(content, cause_words, limit_words=()):
    """O14: the viewer's visible output names an incompatible stream with its cause (and the limit, when a bound was
    exceeded). Searches the published pane content as text."""
    text = json.dumps(content, ensure_ascii=False).lower()
    return any(word in text for word in ("incompatib", "can't read", "cannot read", "stopped updating")) \
        and all(any(alt in text for alt in word.split("|")) for word in cause_words) \
        and all(any(alt in text for alt in word.split("|")) for word in limit_words)


def no_partial_cut(content, forbidden_markers):
    """O14: nothing from an over-bound or rejected snapshot was installed (its marker text never appears)."""
    text = json.dumps(content, ensure_ascii=False)
    return not any(marker in text for marker in forbidden_markers)


# ---------------------------------------------------------------------------------------------------------------------
# O14 scripted test provider (public provider protocol through the pinned SDK; one agent per scenario)

PROVIDER = r'''
import asyncio, json, sys, time
from copy import deepcopy
sys.path.insert(0, sys.argv[1])
from vqro_provider import Provider, Limits, ProviderError, integer
from vqro_provider_stream import Stream
LOG = sys.argv[2]
EPOCH = "o14-e1"
ENTITY_BYTES = 3000  # deliberately unlike any viewer ceiling, so a reported limit is unambiguous
SCENARIOS = {
    "o14-bad-record": {"live": {"op": "put", "seq": 2, "kind": "message"}},
    "o14-unknown-op": {"live": {"op": "o14-future-op", "seq": 2, "kind": "message", "id": "m2", "rev": 2}},
    "o14-over-bound": {"snapshot_extra": {"op": "put", "kind": "message", "id": "huge", "rev": 1,
                       "value": {"role": "assistant", "content": [{"type": "text", "text": "O14-PARTIAL-CUT-" + "x" * 9000}]}}},
}
GOOD = {"op": "put", "kind": "message", "id": "m1", "rev": 1,
        "value": {"role": "assistant", "position": 1, "final": True, "content": [{"type": "text", "text": "O14-last-good"}]}}
def log(**row):
    with open(LOG, "a") as out: out.write(json.dumps(dict(row, t=time.monotonic())) + "\n")
def line(row): return json.dumps(row, separators=(",", ":")).encode() + b"\n"
class P(Provider):
    async def _open(self, params):
        if params.get("channel") != "items": return await super()._open(params)
        ref = params.get("ref"); ident = integer(params.get("stream"))
        after = params.get("after")
        cont = isinstance(after, dict) and after.get("epoch") == EPOCH and after.get("seq") == 1
        log(kind="open", ref=ref, after=after, initial="continue" if cont else "reset")
        stream = Stream(self, params); self.streams[ident] = stream
        self._work(self.serve(stream, ref, cont))
        return stream, {"initial": "continue" if cont else "reset", "epoch": EPOCH}
    async def serve(self, stream, ref, cont):
        s = SCENARIOS[ref]
        try:
            if not cont:
                await stream.send(line({"op": "snapshot", "phase": "begin", "epoch": EPOCH, "coverage": "all_retained"}))
                await stream.send(line(GOOD))
                if "snapshot_extra" in s: await stream.send(line(s["snapshot_extra"]))
                await stream.send(line({"op": "snapshot", "phase": "end", "seq": 1}))
            if "live" in s:
                await stream.send(line(s["live"]))
            while not stream.closed: await asyncio.sleep(0.2)
        except ProviderError:
            pass
def agent(ref):
    return {"ref": ref, "incarnation": "i:" + ref, "kind": "agent", "state": "live", "name": ref, "status": {"state": "idle"},
            "data": {"agent": {"v": 0, "epoch": "e1", "state": "idle", "lifecycle": "live", "session": {"id": "s1"}, "run": None,
                               "caps": {"rev": 1}, "incomplete": []},
                     "items": {"retain_bytes": 131072, "entities": 512, "entity_bytes": ENTITY_BYTES}}}
async def main():
    caps = {"agent": {"v": 0, "modules": {}}, "items.stream": {"retain_bytes": 131072, "entities": 512, "entity_bytes": ENTITY_BYTES}}
    provider = P("o14-scripted", "o14-instance", caps, limits=Limits(pending=64, max_streams=64),
                 software={"name": "o14-scripted-test-provider", "version": "0"})
    await provider.connect()
    await provider.full_sync([agent(ref) for ref in SCENARIOS])
    log(kind="ready")
    await provider.wait_closed()
asyncio.run(main())
'''


async def poll(predicate, timeout, interval=0.2):
    """Bounded polling of an async or sync predicate; returns its first truthy value or None at the deadline."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if asyncio.iscoroutine(value): value = await value
        if value: return value
        await asyncio.sleep(interval)
    return None


def provider_log(path):
    rows = []
    if Path(path).exists():
        for raw in Path(path).read_text().splitlines():
            try: rows.append(json.loads(raw))
            except ValueError: pass
    return rows


async def start_scripted_provider(ctx, sdk_dir):
    root = Path(ctx.root)
    script, log = root / "o14_provider.py", root / "o14_provider.jsonl"
    script.write_text(PROVIDER)
    env = dict(ctx.backend.env)
    process = subprocess.Popen(["/usr/bin/python3", str(script), str(sdk_dir), str(log)], env=env,
                               stdout=open(root / "o14_provider.log", "ab"), stderr=subprocess.STDOUT)
    return process, log


SCENARIO_GRADES = {
    "o14-bad-record": {"cause": ["bad_record|malformed|invalid record|unreadable|can't read"], "limit": []},
    "o14-unknown-op": {"cause": ["unknown|newer|unsupported"], "limit": []},
    "o14-over-bound": {"cause": ["larger|over_bound|too large|exceed"], "limit": ["3000"]},
}


async def install_production_viewer(ctx):
    """Copy the pinned production agents extension into the private fixture and enable it by ordinary configuration."""
    import hashlib, shutil
    source = ctx.manifest.get("viewer_extensions")
    pinned = ctx.manifest.get("viewer_sha256")
    revision = ctx.manifest.get("viewer_revision")
    if not source or not Path(source).is_absolute() or not (Path(source) / "agents.py").is_file():
        raise HarnessGap("O14 needs the pinned production agents viewer source (manifest viewer_extensions with "
                         "agents.py, vqro_items.py and their SDK); none was declared")
    # TEST-ONLY path input, never a capability: only a kit-pinned viewer is conformance authority.
    if not isinstance(pinned, dict) or not pinned or not isinstance(revision, str) or not revision:
        raise HarnessGap("O14 viewer source is not pinned (manifest viewer_revision and viewer_sha256 for every copied "
                         "*.py are required; an arbitrary caller-edited viewer is not accepted)")
    ext = Path(ctx.root) / "ext"
    hashes = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted(Path(source).glob("*.py"))}
    if hashes != pinned:
        mismatched = sorted(name for name in set(hashes) | set(pinned) if hashes.get(name) != pinned.get(name))
        raise HarnessGap("O14 viewer source differs from its pinned kit hashes: " + ", ".join(mismatched[:8]))
    for name in hashes:
        data = (Path(source) / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != pinned[name]:
            raise HarnessGap("O14 viewer file changed while copying: " + name)
        (ext / name).write_bytes(data)
    ctx.evidence.append({"kind": "o14_production_viewer", "source": str(source), "viewer_revision": revision,
                         "sha256": hashes, "verified_against_pin": True, "test_only_path": True})
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/agents",
        "value": {"enabled": True, "argv": ["/usr/bin/python3", str(ext / "agents.py")]}}]})
    return ext


@case("O14")
async def incompatible_viewers(ctx):
    """Repeated bad_record or over_bound at the same continued position, and an over-bound entity in a reset snapshot:
    the production viewer backs off, reports an incompatible stream with cause and limit, and never loops, skips,
    advances its cursor, relaxes bounds or installs a partial cut; an unknown newer op is handled the same way."""
    ext = await install_production_viewer(ctx)
    views = {}
    watcher = await ctx.viewer()
    def on_event(event):
        data = event.get("data") if isinstance(event.get("data"), dict) else event
        if event.get("type") == "view.content" and isinstance(data, dict):
            views[data.get("view")] = data.get("content")
    watcher.on("event", on_event)
    await watcher.call("events.subscribe", {"view_content": {"prefixes": ["agents"]}})
    process, log = await start_scripted_provider(ctx, ext)
    try:
        # Bounded setup (the handler deadline is 45 s): the provider registers its 3 agents and the production viewer
        # opens each stream at least once; then one back-off window is observed from the first open.
        async def registered():
            listed = (await ctx.peer.call("resource.list", {"provider": "o14-scripted"}))["resources"]
            return {row["backend_ref"]: row["id"] for row in listed} if len(listed) >= len(SCENARIO_GRADES) else None
        ids = await poll(registered, 10)
        if not ids:
            raise HarnessGap("O14 scripted provider did not register its agents within the bounded setup")
        def first_opens():
            refs = {row.get("ref") for row in provider_log(log) if row.get("kind") == "open"}
            return refs >= set(SCENARIO_GRADES) or None
        await poll(first_opens, 10)  # a stream the viewer never opened is graded below (opened == False)
        await asyncio.sleep(BACKOFF_WINDOW_S)
        rows = provider_log(log)
        results = {}
        for ref, grade in SCENARIO_GRADES.items():
            opens = [row for row in rows if row.get("kind") == "open" and row.get("ref") == ref]
            content = views.get("agents-" + ids.get(ref, "?"))
            results[ref] = {
                "opened": bool(opens),
                "no_loop": no_reattach_loop([row["t"] for row in opens]),
                "cursor_kept": cursor_never_advanced([row.get("after") for row in opens], 1, "o14-e1"),
                "reported": incompatibility_reported(content, grade["cause"], grade["limit"]),
                "no_partial_cut": no_partial_cut(content, ["O14-PARTIAL-CUT-"]),
                "opens_in_window": len([r for r in opens if r["t"] - opens[0]["t"] <= BACKOFF_WINDOW_S]) if opens else 0,
                "pane": content,
            }
        ctx.evidence.append({"kind": "o14_results", "results": results, "resources": ids})
        for ref, result in results.items():
            check(result["opened"], f"{ref}: the production viewer attached the scripted stream", component="harness")
        for ref, result in results.items():
            check(result["cursor_kept"] and result["no_partial_cut"],
                  f"{ref}: the viewer never advanced past the incompatible record nor installed a partial cut", component="viewer")
            check(result["no_loop"], f"{ref}: the viewer backs off instead of an immediate reattach loop "
                  f"({result['opens_in_window']} opens in {BACKOFF_WINDOW_S:.0f} s)", component="viewer")
            check(result["reported"], f"{ref}: the viewer visibly reports an incompatible stream with its cause and limit",
                  component="viewer")
    finally:
        process.terminate()
        try: process.wait(5)
        except subprocess.TimeoutExpired: process.kill()


# ---------------------------------------------------------------------------------------------------------------------
# O12 adapter-side checker (unit-tested with negatives)

def adapter_preserves_or_marks(stream_texts, emitted_bytes, emitted_sha256, marker_words=("truncated", "omitted", "too large", "too_large")):
    """O12: after a producer too_large refusal, the adapter's item stream either still carries the whole emitted text
    (some message entity's text has exactly the native ledger's length and sha256, possibly split across entities in
    order) or visibly marks the refusal; a shorter text with no marker is silent loss."""
    import hashlib
    joined = "".join(stream_texts)
    whole = any(len(text.encode()) == emitted_bytes and hashlib.sha256(text.encode()).hexdigest() == emitted_sha256
                for text in stream_texts) or (len(joined.encode()) == emitted_bytes
                                              and hashlib.sha256(joined.encode()).hexdigest() == emitted_sha256)
    marked = any(word in text.lower() for text in stream_texts for word in marker_words)
    return whole or marked


O12_TEST = ("provider_client::items::tests::small_appends_past_the_record_bound_are_refused_atomically_for_bytes_and_text",
            "src/provider_client/items.rs")


def message_texts(reducer, needle):
    """Text blocks of message entities that carry `needle`, in entity order."""
    texts = []
    for (kind, _), entity in sorted(reducer.entities.items(), key=lambda item: str(item[0])):
        value = entity.get("value") or {}
        if kind == "message" and isinstance(value.get("content"), list):
            texts += [block["text"] for block in value["content"]
                      if isinstance(block, dict) and isinstance(block.get("text"), str) and needle in block["text"]]
    return texts


@case("O12")
async def producer_append_bounds(ctx):
    """A small append whose resulting whole snapshot put exceeds the record bound returns too_large with entity,
    revision and seq unchanged (text and decoded bytes); adapters record how they handle the refusal without
    silently losing content."""
    from . import evidence
    # Producer half: the exact regression, inherited from the pinned gate run (as O3/O10/O11).
    if "O12" not in evidence.REGRESSIONS:
        raise HarnessGap("O12 producer regression is not registered in evidence.REGRESSIONS (needs " + O12_TEST[0] +
                         " in " + O12_TEST[1] + ")")
    row, receipt, repo, producer = evidence.component(ctx, "O12")
    evidence.regressions(row, receipt, repo, producer, "O12")
    evidence.inherited(ctx, "O12", row, receipt)
    # Adapter half: an oversized assistant message streamed through the actual bridge.
    from .cases import fresh_view
    view = await fresh_view(ctx)
    mark = ctx.native.mark()
    chunks = ["O12-" + "x" * 1020 for _ in range(70)]  # about 70 KiB of text_delta appends in one message
    # Fixture seam 5d9b0590 (prim-final-review): retain with chunks emits one assistant message as len(chunks)
    # native deltas, text "0:" + "".join(chunks), and one native_emit ledger row (Pi message_end, Codex item/completed).
    await ctx.native.call("retain", count=1, chunks=chunks, timeout=10)
    emitted = [row for row in ctx.native.records("native_emit", after=mark)
               if row.get("type") in ("message_end", "item/completed") and row.get("role") == "assistant"]
    if not emitted or not isinstance(emitted[-1].get("text_bytes"), int) or not isinstance(emitted[-1].get("text_sha256"), str):
        raise HarnessGap("O12 adapter half needs the native fixture's ledger witness of the emitted message "
                         "(native_emit message_end with text_bytes and text_sha256)")
    witness = emitted[-1]
    def settled(reducer):
        texts = message_texts(reducer, "O12-")
        return texts and adapter_preserves_or_marks(texts, witness["text_bytes"], witness["text_sha256"])
    try:
        await ctx.wait_view(view, settled, timeout=15)
    except Exception:
        pass  # graded below from what the stream actually holds
    texts = message_texts(view.reducer, "O12-")
    ctx.evidence.append({"kind": "o12_adapter", "emitted": witness, "stream_text_bytes": [len(t.encode()) for t in texts]})
    check(adapter_preserves_or_marks(texts, witness["text_bytes"], witness["text_sha256"]),
          "after the producer refusal the adapter keeps the whole emitted text or visibly marks the refusal "
          "(no silent loss)")
