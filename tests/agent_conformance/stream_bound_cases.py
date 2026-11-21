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
    it never skips the incompatible record by advancing past `last_good_seq`. A `last_good_seq` of None means no
    snapshot was ever committed (the only one offered was rejected), so every reopen must be a reset."""
    for after in afters:
        if after is None:
            continue
        if last_good_seq is None:
            return False
        if not isinstance(after, dict) or after.get("epoch") != epoch or not isinstance(after.get("seq"), int):
            return False
        if after["seq"] > last_good_seq:
            return False
    return True


def json_equal(a, b):
    """serde_json Value equality: typed, so true != 1, 1 != 1.0 and "1" != 1 (Python's == would say true == 1)."""
    if type(a) is not type(b):
        return False
    if isinstance(a, list):
        return len(a) == len(b) and all(json_equal(x, y) for x, y in zip(a, b))
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(json_equal(a[k], b[k]) for k in a)
    return a == b


def visible_text(content):
    """The text a published pane actually draws, projected per row kind exactly as the production renderer does
    (src/tui/panel.rs at main 31c157b2, draw_row :680-:833, kind() defaulting to "item" in src/ui_model/panel.rs:107):
    - the panel title and title_right; a paged panel shows its first page (title and rows);
    - composer: nothing; separator: only a line, nothing textual (:730);
    - chips: each chip's text, never the row's own text (:692-:710, chip_label :78);
    - select: the row text and the label of the first option whose value equals the row value by typed JSON
      equality (json_equal), else the value when it is a string (:711-:729);
    - header and text: the row text, only the first `fold` lines when folded (:731, :732-:754, body_lines :139);
    - message: title (else text) on top, then body (else text); "else" only when the field is absent or not a
      string, so an empty title or body stays empty (Option::or, block_lines :149-:161, body_lines :141);
    - every other kind ("item", "toggle", ...): text, detail, badge texts, action texts that have a command, and
      description (:755-:833);
    - children only when the row is expanded.
    Labels, copy payloads, ids, args, hidden diagnostics and every other field are never visible."""
    out = []
    def text(node, key="text"):
        return node[key] if isinstance(node.get(key), str) else None
    def either(node, key, fallback):
        # Rust `a.as_str().or(b.as_str())`: falls back only when `key` is absent or not a string; "" stays "".
        return node[key] if isinstance(node.get(key), str) else text(node, fallback)
    def folded(value, node):
        n = node.get("fold")
        # Rust `as_u64().map(|n| n.clamp(1, 10_000))`: only a non-negative integer folds (0 shows one line).
        if isinstance(n, int) and not isinstance(n, bool) and n >= 0 and value is not None:
            return "\n".join(value.split("\n")[:max(1, min(n, 10000))])
        return value
    def row(node):
        if not isinstance(node, dict):
            return
        kind = node.get("type") if isinstance(node.get("type"), str) else "item"
        shown = []
        if kind in ("composer", "separator"):
            pass
        elif kind == "chips":
            shown += [chip["text"] for chip in node.get("chips", []) if isinstance(chip, dict) and isinstance(chip.get("text"), str)]
        elif kind == "select":
            value = node.get("value")
            # Rust `item["options"].as_array()...find(|x| x["value"] == value).and_then(|x| x["label"].as_str())`:
            # indexing a missing field or a non-object option yields Null, and a non-array `options` has no options.
            options = node.get("options") if isinstance(node.get("options"), list) else []
            chosen = next((option for option in options
                           if json_equal(option.get("value") if isinstance(option, dict) else None, value)), None)
            label = chosen.get("label") if isinstance(chosen, dict) else None
            shown += [text(node), label if isinstance(label, str) else value if isinstance(value, str) else None]
        elif kind in ("header", "text"):
            shown.append(folded(text(node), node))
        elif kind == "message":
            shown += [either(node, "title", "text"), folded(either(node, "body", "text"), node)]
        else:
            shown += [text(node), text(node, "detail"), text(node, "description")]
            shown += [badge.get("text") for badge in node.get("badges", []) or [] if isinstance(badge, dict)]
            shown += [action.get("text") for action in node.get("actions", []) or []
                      if isinstance(action, dict) and isinstance(action.get("command"), str)]
        out.extend(value for value in shown if isinstance(value, str))
        if node.get("expanded") is True and isinstance(node.get("children"), list):
            for child in node["children"]:
                row(child)
    def panel(value):
        out.extend(value[key] for key in ("title", "title_right") if isinstance(value.get(key), str))
        for node in value.get("items", []) if isinstance(value.get("items"), list) else []:
            row(node)
    if isinstance(content, list):
        for node in content:
            row(node)
    elif isinstance(content, dict):
        pages = content.get("pages")
        if isinstance(pages, list) and pages and isinstance(pages[0], dict) and not isinstance(content.get("items"), list):
            out.extend(content[key] for key in ("title", "title_right") if isinstance(content.get(key), str))
            panel(pages[0])
        else:
            panel(content)
    return "\n".join(out)


def incompatibility_reported(content, cause_words, limit_words=()):
    """O14: the viewer's visible output names an incompatible stream with its cause (and the limit, when a bound was
    exceeded). Only visible pane text counts (visible_text)."""
    text = visible_text(content).lower()
    return any(word in text for word in ("incompatib", "can't read", "cannot read", "stopped updating")) \
        and all(any(alt in text for alt in word.split("|")) for word in cause_words) \
        and all(any(alt in text for alt in word.split("|")) for word in limit_words)


def no_partial_cut(content, forbidden_markers):
    """O14: nothing from an over-bound or rejected snapshot was installed: no text of ANY record of the rejected cut
    appears anywhere in the published pane (visible or not)."""
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
SPEC = json.load(open(sys.argv[3]))
NAME, EPOCH = SPEC["provider"], SPEC["epoch"]
SCENARIOS = SPEC["scenarios"]  # ref -> {"snapshot": [records], "live": [records], "entity_bytes": n}
def log(**row):
    with open(LOG, "a") as out: out.write(json.dumps(dict(row, t=time.monotonic())) + "\n")
def line(row): return json.dumps(row, separators=(",", ":")).encode() + b"\n"
class P(Provider):
    async def _open(self, params):
        if params.get("channel") != "items": return await super()._open(params)
        ref = params.get("ref"); ident = integer(params.get("stream"))
        after = params.get("after")
        row = SCENARIOS.get(ref, {}); end = row.get("end_seq", len(row.get("snapshot", [])))
        cont = isinstance(after, dict) and after.get("epoch") == EPOCH and after.get("seq") == end
        log(kind="open", ref=ref, after=after, initial="continue" if cont else "reset")
        stream = Stream(self, params); self.streams[ident] = stream
        self._work(self.serve(stream, ref, cont))
        return stream, {"initial": "continue" if cont else "reset", "epoch": EPOCH}
    async def serve(self, stream, ref, cont):
        s = SCENARIOS[ref]
        try:
            if not cont:
                await stream.send(line({"op": "snapshot", "phase": "begin", "epoch": EPOCH, "coverage": "all_retained"}))
                for record in s.get("snapshot", []): await stream.send(line(record))
                await stream.send(line({"op": "snapshot", "phase": "end", "seq": s.get("end_seq", len(s.get("snapshot", [])))}))
            for record in s.get("live", []): await stream.send(line(record))
            while not stream.closed: await asyncio.sleep(0.2)
        except ProviderError:
            pass
def agent(ref):
    return {"ref": ref, "incarnation": "i:" + ref, "kind": "agent", "state": "live", "name": ref, "status": {"state": "idle"},
            "data": {"agent": {"v": 0, "epoch": "e1", "state": "idle", "lifecycle": "live", "session": {"id": "s1"}, "run": None,
                               "caps": {"rev": 1}, "incomplete": []},
                     "items": {"retain_bytes": 131072, "entities": 512, "entity_bytes": SCENARIOS[ref].get("entity_bytes", 65536)}}}
async def main():
    bound = max([row.get("entity_bytes", 65536) for row in SCENARIOS.values()] or [65536])
    caps = {"agent": {"v": 0, "modules": {}}, "items.stream": {"retain_bytes": 131072, "entities": 512, "entity_bytes": bound}}
    provider = P(NAME, NAME + "-instance", caps, limits=Limits(pending=64, max_streams=64),
                 software={"name": NAME + "-test-provider", "version": "0"})
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


async def start_item_provider(ctx, sdk_dir, name, scenarios, epoch=None):
    """Generic public item producer for Part A handlers: a separate process speaking only the public provider protocol
    through the pinned SDK. It registers one kind:agent resource per ref (items.stream declared, entity_bytes per ref)
    and, on each items open, serves a reset snapshot of exactly scenarios[ref]["snapshot"] (public item records, seq
    as given; the snapshot end carries "end_seq", default the record count) and then scenarios[ref]["live"]; an after cursor at the snapshot end continues instead. Every
    open is logged (ref, after, initial, monotonic t). No store writes and no authority: just a provider.
    Returns (process, log path); the caller terminates the process."""
    root = Path(ctx.root)
    script, log, spec = root / (name + "_provider.py"), root / (name + "_provider.jsonl"), root / (name + "_scenarios.json")
    script.write_text(PROVIDER)
    spec.write_text(json.dumps({"provider": name, "epoch": epoch or name + "-e1", "scenarios": scenarios}))
    with open(root / (name + "_provider.log"), "ab") as output:  # the parent's handle is closed once the child has it
        process = subprocess.Popen(["/usr/bin/python3", str(script), str(sdk_dir), str(log), str(spec)],
                                   env=dict(ctx.backend.env), stdout=output, stderr=subprocess.STDOUT)
    ctx.evidence.append({"kind": "owned_provider", "name": name, "pid": process.pid, "birth": process_birth(process.pid)})
    return process, log


def process_birth(pid):
    """Best-effort process start identity (Linux /proc start time in clock ticks); None where unavailable."""
    try:
        return int(Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def stop_provider(ctx, process):
    """Terminate, then kill on timeout, and always reap; the outcome is recorded as owned-child cleanup evidence."""
    process.terminate()
    try:
        process.wait(5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(5)
    ctx.evidence.append({"kind": "owned_provider_reaped", "pid": getattr(process, "pid", None),
                         "returncode": getattr(process, "returncode", None)})


O14_ENTITY_BYTES = 3000  # deliberately unlike any viewer ceiling, so a reported limit is unambiguous
O14_GOOD = {"op": "put", "kind": "message", "id": "m1", "rev": 1,
            "value": {"role": "assistant", "position": 1, "final": True, "content": [{"type": "text", "text": "O14-last-good"}]}}
O14_SCENARIOS = {
    "o14-bad-record": {"snapshot": [O14_GOOD], "live": [{"op": "put", "seq": 2, "kind": "message"}], "entity_bytes": O14_ENTITY_BYTES, "end_seq": 1},
    "o14-unknown-op": {"snapshot": [O14_GOOD], "entity_bytes": O14_ENTITY_BYTES, "end_seq": 1,
                       "live": [{"op": "o14-future-op", "seq": 2, "kind": "message", "id": "m2", "rev": 2}]},
    "o14-over-bound": {"entity_bytes": O14_ENTITY_BYTES, "end_seq": 1, "snapshot": [{
        "op": "put", "kind": "message", "id": "m1", "rev": 1, "value": {"role": "assistant", "position": 1, "final": True,
        "content": [{"type": "text", "text": "O14-REJECTED-CUT-SIBLING"}]}}, {
        "op": "put", "kind": "message", "id": "huge", "rev": 1,
        "value": {"role": "assistant", "content": [{"type": "text", "text": "O14-PARTIAL-CUT-" + "x" * 9000}]}}]},
}


async def start_scripted_provider(ctx, sdk_dir):
    return await start_item_provider(ctx, sdk_dir, "o14-scripted", O14_SCENARIOS, epoch="o14-e1")


# Per scenario: visible cause/limit words, the last committed seq a reopen may continue from (None: the only snapshot
# offered is the rejected one, so only resets are allowed), and text from records that must never be installed.
SCENARIO_GRADES = {
    "o14-bad-record": {"cause": ["bad_record|malformed|invalid record|unreadable|can't read"], "limit": [],
                       "last_good_seq": 1, "forbidden": []},
    "o14-unknown-op": {"cause": ["unknown|newer|unsupported"], "limit": [], "last_good_seq": 1, "forbidden": []},
    "o14-over-bound": {"cause": ["larger|over_bound|too large|exceed"], "limit": ["3000"], "last_good_seq": None,
                       "forbidden": ["O14-REJECTED-CUT-SIBLING", "O14-PARTIAL-CUT-"]},
}


async def install_production_viewer(ctx):
    """Install the production agents extension into the private fixture and enable it by ordinary configuration.
    viewer_extensions is a TEST-ONLY path input, never a capability. It is conformance authority only when bound to
    trusted source: viewer_revision is a full commit reachable from the trusted harness main, and every *.py in the
    directory equals both its viewer_sha256 pin and its blob under extensions/ at that commit. Everything is read and
    verified in memory before anything is written, so a HarnessGap leaves no copied file behind."""
    import hashlib
    from . import evidence
    source = ctx.manifest.get("viewer_extensions")
    pinned = ctx.manifest.get("viewer_sha256")
    revision = ctx.manifest.get("viewer_revision")
    if not source or not Path(source).is_absolute() or not (Path(source) / "agents.py").is_file():
        raise HarnessGap("O14 needs the pinned production agents viewer source (manifest viewer_extensions with "
                         "agents.py, vqro_items.py and their SDK); none was declared")
    if not isinstance(pinned, dict) or not pinned or not isinstance(revision, str):
        raise HarnessGap("O14 viewer source is not pinned (manifest viewer_revision and viewer_sha256 for every copied "
                         "*.py are required; an arbitrary caller-edited viewer is not accepted)")
    revision = evidence.commit(evidence.HARNESS_REPO, revision)  # full immutable id, resolves in the trusted repository
    trusted_main = evidence.git(evidence.HARNESS_REPO, "rev-parse", "--verify", "refs/heads/main^{commit}").decode().strip()
    try:
        evidence.git(evidence.HARNESS_REPO, "merge-base", "--is-ancestor", revision, trusted_main)
    except HarnessGap as error:
        raise HarnessGap("O14 viewer_revision is not reachable from trusted harness main") from error
    files = {path.name: path.read_bytes() for path in sorted(Path(source).glob("*.py"))}
    hashes = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
    if hashes != pinned:
        mismatched = sorted(name for name in set(hashes) | set(pinned) if hashes.get(name) != pinned.get(name))
        raise HarnessGap("O14 viewer source differs from its pinned hashes: " + ", ".join(mismatched[:8]))
    for name, digest in hashes.items():
        try:
            blob = evidence.git(evidence.HARNESS_REPO, "show", f"{revision}:extensions/{name}")
        except HarnessGap as error:
            raise HarnessGap(f"O14 viewer file {name} is not in extensions/ at viewer_revision") from error
        if hashlib.sha256(blob).hexdigest() != digest:
            raise HarnessGap(f"O14 viewer file {name} differs from extensions/{name} at viewer_revision")
    ext = Path(ctx.root) / "ext"
    for name, data in files.items():  # only verified bytes are written; no check can fail past this point
        staged = ext / (name + ".o14-staged")
        staged.write_bytes(data)
        staged.replace(ext / name)
    ctx.evidence.append({"kind": "o14_production_viewer", "source": str(source), "viewer_revision": revision,
                         "trusted_main": trusted_main, "sha256": hashes, "verified_against_revision": True,
                         "test_only_path": True})
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
                "cursor_kept": cursor_never_advanced([row.get("after") for row in opens], grade["last_good_seq"], "o14-e1"),
                "reported": incompatibility_reported(content, grade["cause"], grade["limit"]),
                "no_partial_cut": no_partial_cut(content, grade["forbidden"]),
                "visible": visible_text(content)[:2000],
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
        stop_provider(ctx, process)


# ---------------------------------------------------------------------------------------------------------------------
# O12 adapter-side checker (unit-tested with negatives)

def adapter_preserves_or_marks(stream_texts, emitted_bytes, emitted_sha256, marker_words=("truncated", "omitted", "too large", "too_large"), other_texts=()):
    """O12: after a producer too_large refusal, the adapter's item stream either still carries the whole emitted text
    (some text has exactly the native ledger's length and sha256, or the body texts joined in order do) or marks the
    refusal; a shorter text with no marker is silent loss. `stream_texts` are the text blocks of the correlated message
    entities (body and any separate marker block); `other_texts` are texts of other entities the message produced
    (an omission-only entity), searched for the marker only."""
    import hashlib
    def same(text): return len(text.encode()) == emitted_bytes and hashlib.sha256(text.encode()).hexdigest() == emitted_sha256
    body = [text for text in stream_texts if not any(word in text.lower() for word in marker_words)]
    whole = any(same(text) for text in stream_texts) or same("".join(body)) or same("".join(stream_texts))
    marked = any(word in text.lower() for text in list(stream_texts) + list(other_texts) for word in marker_words)
    return whole or marked


O12_TEST = ("provider_client::items::tests::small_appends_past_the_record_bound_are_refused_atomically_for_bytes_and_text",
            "src/provider_client/items.rs")


def shown_texts(value):
    """The user-facing text of an item entity: its content text blocks and a top-level `text`. Nested native or other
    metadata is never a visible marker."""
    texts = [value["text"]] if isinstance(value.get("text"), str) else []
    if isinstance(value.get("content"), list):
        texts += [block["text"] for block in value["content"] if isinstance(block, dict) and isinstance(block.get("text"), str)]
    return texts


def message_texts(reducer, needle, baseline=None):
    """Correlate the emitted message once, from CURRENT output only: entities unchanged since `baseline` (a {key: rev}
    map taken before the native emission) are history and never count. Returns (texts, other_texts): `texts` are ALL
    shown text blocks (body and any marker block) of every new or changed message entity carrying `needle`, in
    timeline (position) order; `other_texts` are the shown text (shown_texts) of every other new or changed entity,
    e.g. an omission-only notice. Hidden metadata never counts."""
    texts, others = [], []
    def timeline(item):  # the message's own position, never the opaque entity id; unpositioned entities last
        position = (item[1].get("value") or {}).get("position")
        return (0, position, str(item[0])) if isinstance(position, int) and not isinstance(position, bool) else (1, 0, str(item[0]))
    for key, entity in sorted(reducer.entities.items(), key=timeline):
        if baseline is not None and key in baseline and baseline[key] == entity.get("rev"):
            continue  # unchanged history, not output of this emission
        kind, value = key[0], entity.get("value") or {}
        blocks = [block["text"] for block in value.get("content", []) if isinstance(block, dict)
                  and isinstance(block.get("text"), str)] if kind == "message" and isinstance(value.get("content"), list) else []
        if any(needle in text for text in blocks):
            texts += blocks
        else:
            others += shown_texts(value)
    return texts, others


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
    baseline = {key: entity.get("rev") for key, entity in view.reducer.entities.items()}
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
        texts, others = message_texts(reducer, "O12-", baseline)
        return texts and adapter_preserves_or_marks(texts, witness["text_bytes"], witness["text_sha256"], other_texts=others)
    try:
        await ctx.wait_view(view, settled, timeout=15)
    except Exception:
        pass  # graded below from what the stream actually holds
    texts, others = message_texts(view.reducer, "O12-", baseline)
    ctx.evidence.append({"kind": "o12_adapter", "emitted": witness, "stream_text_bytes": [len(t.encode()) for t in texts],
                         "other_new_entity_strings": [t[:200] for t in others][:16]})
    check(adapter_preserves_or_marks(texts, witness["text_bytes"], witness["text_sha256"], other_texts=others),
          "after the producer refusal the adapter keeps the whole emitted text or visibly marks the refusal "
          "(no silent loss)")
