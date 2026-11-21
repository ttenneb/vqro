"""Part A O12 (producer append bounds) and O14 (incompatible viewers) handlers.

O14 targets the PRODUCTION viewer: the agents extension's Conversation/Agents.attach over the real vqro_items.ItemView,
fed by a tiny scripted test provider over the public provider protocol. Nothing emulates the viewer's back-off or its
incompatibility report; a product that lacks them fails. O12's producer half is inherited regression evidence and its
adapter half observes a real bridge refusal; either missing is an honest harness gap, never a pass.
"""
import asyncio
from collections import deque
from copy import deepcopy
import hashlib
import json
import secrets
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


def production(name, subdir=""):
    """A production extension module from the trusted harness repository (read-only import, never a copy)."""
    import importlib.util, sys
    from .evidence import HARNESS_REPO
    base = Path(HARNESS_REPO) / "extensions"
    path = base / subdir / (name + ".py")
    if not path.is_file():
        raise HarnessGap(f"production module {subdir + '/' if subdir else ''}{name}.py is absent from the harness repository")
    key = "_o12o14_production_" + name
    if key not in sys.modules:
        for extra in (str(base), str(base / subdir)):
            if extra not in sys.path:
                sys.path.append(extra)  # its own stdlib-only sibling imports (vqro_display, ...)
        spec = importlib.util.spec_from_file_location(key, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        sys.modules[key] = module
    return sys.modules[key]


_CLIENT_FEATURES = None


def client_features():
    """The features the rendering client supports: the TUI's SUPPORTED list (src/ui_model/panel.rs), parsed from the
    trusted harness repository, so normalization follows the client that actually draws the pane."""
    global _CLIENT_FEATURES
    if _CLIENT_FEATURES is None:
        import re
        from .evidence import HARNESS_REPO
        source = Path(HARNESS_REPO) / "src" / "ui_model" / "panel.rs"
        match = re.search(r"pub const SUPPORTED: &\[&str\] = &\[([^\]]*)\];", source.read_text()) if source.is_file() else None
        if not match:
            raise HarnessGap("the rendering client's SUPPORTED feature list is absent from src/ui_model/panel.rs")
        _CLIENT_FEATURES = re.findall(r'"([^"]+)"', match.group(1))
    return _CLIENT_FEATURES


def json_equal(a, b):
    """serde_json Value equality: typed, so true != 1, 1 != 1.0 and "1" != 1 (Python's == would say true == 1)."""
    if type(a) is not type(b):
        return False
    if isinstance(a, list):
        return len(a) == len(b) and all(json_equal(x, y) for x, y in zip(a, b))
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(json_equal(a[k], b[k]) for k in a)
    return a == b


def visible_text(content, features=None):
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
    Labels, copy payloads, ids, args, hidden diagnostics and every other field are never visible.
    Rows are first normalized as the production client does (vqro_client_model.expand: `requires` with `fallback`,
    `requires_absent`, chip fallbacks) for `features` (default: the rendering TUI's SUPPORTED); a panel whose own
    `requires` the client lacks shows nothing here (fail closed)."""
    features = client_features() if features is None else features
    model = production("vqro_client_model", "clients")
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
            rows(node["children"])
    def rows(items):
        for index, item in enumerate(items):
            if isinstance(item, dict):
                for shown_row, _ in model.expand(item, str(index), features):
                    row(shown_row)
    def panel(value):
        out.extend(value[key] for key in ("title", "title_right") if isinstance(value.get(key), str))
        rows(value.get("items") if isinstance(value.get("items"), list) else [])
    if isinstance(content, list):
        rows(content)
    elif isinstance(content, dict):
        if model.missing_for(content, features):
            return ""  # the client cannot render this panel at all; nothing in it is visible
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

def adapter_preserves_or_marks(body_texts, emitted_bytes, emitted_sha256, marker_words=("truncated", "omitted", "too large", "too_large"), other_texts=()):
    """O12: after a producer too_large refusal, the adapter's item stream either still carries the whole emitted text,
    or visibly marks the refusal; a shorter text with no marker is silent loss. The two halves never mix:
    - `body_texts`: the RAW text blocks of the freshly emitted message (stream content), used ONLY for whole-text
      preservation: one block, or all of them joined in order, has exactly the native ledger's length and sha256;
    - `other_texts`: ONLY newly SHOWN rows (production content_rows output new since the baseline, from any entity,
      including the emitted message itself), used ONLY for the marker. Raw text never counts as a marker."""
    import hashlib
    def same(text): return len(text.encode()) == emitted_bytes and hashlib.sha256(text.encode()).hexdigest() == emitted_sha256
    whole = any(same(text) for text in body_texts) or same("".join(body_texts))
    marked = any(word in text.lower() for text in other_texts for word in marker_words)
    return whole or marked


O12_TEST = ("provider_client::items::tests::small_appends_past_the_record_bound_are_refused_atomically_for_bytes_and_text",
            "src/provider_client/items.rs")


def display_text(text):
    """Display text of a published string: terminal escape metadata removed (OSC sequences such as hyperlink URIs,
    CSI sequences, other C0 controls except newline and tab). Metadata is never display content."""
    import re
    text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", text)
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    return "".join(c for c in text if c in "\n\t" or not (ord(c) < 32 or ord(c) == 127))


def shown_texts(value):
    """O12 marker evidence is graded on the adapter's PUBLIC ITEM STREAM (vqro-lead ruling), not on what a bundled
    pane renders: the display content of an entity's agent-v0 content blocks (section 3.2): `text {text}` and
    `resource {text}` as display text; `image`, `audio` and `resource_link` carry no marker text. Extra fields and
    `native` metadata never count as text; the standardized entity incomplete flag is graded separately."""
    content = value.get("content") if isinstance(value, dict) else None
    if not isinstance(content, list):
        return []
    return [display_text(block["text"]) for block in content if isinstance(block, dict)
            and block.get("type") in ("text", "resource") and isinstance(block.get("text"), str)]


NOTICES = ("__live_notices__",)  # baseline key for the stream's live notice records (op notice, no seq)
OBSERVATION = ("__o12_observation__",)  # client receipt cut, not a native record/sequence


class ReceivedNotices(deque):
    """The reducer's bounded live-notice cache with receipt bookkeeping: `received` counts every notice ever
    appended, so freshness is by receipt ordinal, never by value (a new notice identical to cached old ones, evicting
    one of them, is still new). Client-side observer bookkeeping only; live notices carry no native seq."""
    RECORD = 4096  # notices recorded since the baseline mark, independent of the reducer's 64-entry cache

    def __init__(self, items=(), maxlen=None):
        super().__init__(items, maxlen)
        self.received = len(self)
        self.since, self.lost = None, False  # recording starts at mark(); lost: the recorder itself overflowed

    def mark(self):
        self.since, self.lost = [], False
        return self.received

    def append(self, item):
        self.received += 1
        if self.since is not None:
            if len(self.since) < self.RECORD:
                self.since.append(item)  # evidence survives the cache evicting it
            else:
                self.lost = True
        super().append(item)

    def extend(self, items):
        for item in items:
            self.append(item)


def track_notices(reducer):
    """Install receipt bookkeeping on this view's notice cache (same bound, same contents); returns the count received."""
    notices = getattr(reducer, "notices", None)
    if isinstance(notices, deque) and not isinstance(notices, ReceivedNotices):
        reducer.notices = notices = ReceivedNotices(notices, notices.maxlen)
    if isinstance(notices, ReceivedNotices):
        return notices.mark()
    return None  # no bounded cache to track (a plain list never evicts)


def notice_value_text(row):
    return display_text(row["text"]) if isinstance(row, dict) and isinstance(row.get("text"), str) else None


def notice_texts(reducer):
    """Display text of the live notice records the item stream delivered (agent-v0: notice {value}, live only)."""
    return [text for text in map(notice_value_text, list(getattr(reducer, "notices", []) or [])) if text is not None]


def fresh_notice_texts(reducer, before):
    """Notices received since the baseline. With receipt bookkeeping: exactly the last (received now - received at
    baseline) entries still held, by ordinal. Without it (a plain growing list): the value multiset difference."""
    notices = getattr(reducer, "notices", None)
    received_before = before.get("received") if isinstance(before, dict) else None
    if isinstance(notices, ReceivedNotices) and type(received_before) is int and notices.since is not None:
        # Every notice received since the baseline, recorded on arrival: an evicted one is still evidence.
        return [text for text in map(notice_value_text, notices.since) if text is not None]
    old = list((before or {}).get("shown", []))
    fresh = []
    for text in notice_texts(reducer):
        if text in old:
            old.remove(text)
        else:
            fresh.append(text)
    return fresh


def body_texts(value):
    """The message body as the stream carries it: its non-blank text blocks, in order (for whole-text preservation;
    only text blocks, never another block's extra field)."""
    content = value.get("content") if isinstance(value, dict) else None
    return [block["text"] for block in content or [] if isinstance(block, dict) and block.get("type") == "text"
            and isinstance(block.get("text"), str) and block["text"].strip()]


def body_fingerprint(value):
    """The raw body's identity: sha256 of the JSON list of its text blocks. Unambiguous: block boundaries and any
    character inside a block (NUL included) are distinct, so one block "a\x00b" never collides with ["a", "b"]."""
    import hashlib
    return hashlib.sha256(json.dumps(body_texts(value), ensure_ascii=False).encode()).hexdigest()


def added_lines(text, emitted, needle):
    """The display lines an adapter ADDED around the emitted text: `text` with every maximal run that is a substring
    of `emitted` removed, then split into non-blank lines. A warning on its own line or inline after (or before) the
    body inside the same block survives; the emitted text itself never does. Without `emitted`, lines carrying
    `needle` are treated as body."""
    if not emitted:
        return [line for line in text.split("\n") if line.strip() and needle not in line]
    out, i, window = [], 0, 32
    while i < len(text):
        j = text.find(needle, i)
        start = emitted.find(text[j:j + window]) if j >= 0 else -1
        if j < 0 or start < 0:
            if j < 0:
                out.append(text[i:])
                break
            out.append(text[i:j + len(needle)])
            i = j + len(needle)
            continue
        b, es = j, start  # extend the emitted run backwards, then forwards, character by character
        while b > i and es > 0 and text[b - 1] == emitted[es - 1]:
            b, es = b - 1, es - 1
        out.append(text[i:b])
        length = 0
        while b + length < len(text) and es + length < len(emitted) and text[b + length] == emitted[es + length]:
            length += 1
        out.append("\n")  # the removed run separates what was added before it from what was added after it
        i = b + length
    return [line.strip() for line in "".join(out).split("\n") if line.strip()]


def marker_lines(texts, emitted, needle):
    return [line for text in texts for line in added_lines(text, emitted, needle)]


class AppliedFlagPuts:
    """Case-local bounded evidence of installed LIVE puts, never raw debug receipts."""
    LIMIT = 128 << 10
    MAX = 128

    def __init__(self, reducer):
        self.reducer, self.records, self.bytes, self.received, self.lost = reducer, [], 0, 0, False
        self.original = getattr(reducer, "accept", None)
        self.known = callable(self.original)
        self.hook = self.accept
        if self.known:
            reducer.accept = self.hook

    def accept(self, row):
        before_epoch, before_seq = self.reducer.epoch, self.reducer.sequence
        live = self.reducer.staged is None
        result = self.original(row)  # Preserve the original return/exception, not an application receipt.
        if (not live or row.get("op") != "put" or type(before_seq) is not int
                or self.reducer.epoch != before_epoch or self.reducer.sequence != row.get("seq")
                or self.reducer.sequence <= before_seq):
            return result
        key = (row.get("kind"), row.get("id"))
        installed = self.reducer.entities.get(key)
        if (not installed or installed.get("rev") != row.get("rev") or (row.get("value") or {}).get("incomplete") is not True
                or (installed.get("value") or {}).get("incomplete") is not True):
            return result
        self.received += 1
        try:
            size = len(json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode())
            if len(self.records) >= self.MAX or self.bytes + size > self.LIMIT:
                self.lost = True
                return result
            recorded = deepcopy(row)
        except Exception:
            self.lost = True  # Observation failure must not alter decoder behavior.
            return result
        self.records.append({"receipt": self.received, "epoch": self.reducer.epoch, "put": recorded})
        self.bytes += size
        return result

    def restore(self):
        if self.known and self.reducer.accept == self.hook:
            self.reducer.accept = self.original


def shown_baseline(reducer, emitted=None, needle="O12-", *, applied_flags=None):
    """The item stream before native emission: per-entity display/body, incomplete flag, revision and epoch,
    plus live notices already delivered under NOTICES. `emitted` is the exact text about to be
    emitted, so the baseline's display lines are reduced exactly as the later ones are."""
    baseline = {key: {"shown": marker_lines(shown_texts(entity.get("value") or {}), emitted, needle),
                      "body": body_fingerprint(entity.get("value") or {}),
                      "body_text_sha256": hashlib.sha256("".join(body_texts(entity.get("value") or {})).encode()).hexdigest(),
                      "incomplete": (entity.get("value") or {}).get("incomplete") is True,
                      "rev": entity.get("rev"), "epoch": getattr(reducer, "epoch", None)}
                for key, entity in reducer.entities.items()}
    received = track_notices(reducer)
    baseline[NOTICES] = {"shown": marker_lines(notice_texts(reducer), emitted, needle), "body": "",
                         **({"received": received} if received is not None else {})}
    baseline[OBSERVATION] = {"shown": [], "body": "", "epoch": getattr(reducer, "epoch", None),
                             "applied_flags": applied_flags,
                             "applied_received": applied_flags.received if applied_flags else None}
    return baseline


def message_texts(reducer, needle, baseline=None, emitted=None):
    """The emitted message's evidence from CURRENT output only, given shown_baseline(reducer, emitted) taken before
    the emission. Returns (body_texts, fresh_markers):
    - body_texts: the raw text blocks of every message entity whose raw body carries `needle` and changed since the
      baseline (new entity or new body fingerprint), in timeline (position) order: whole-text preservation only;
    - fresh_markers: every display line an adapter ADDED (added_lines: item-stream display content with the emitted
      text itself removed; live notices included) that is new since the baseline, from any entity: marker only. A
      warning on its own line or inline in the same block as the body counts; the emitted text never does.
    A revision bump that changes neither the raw body nor the added lines is history and contributes nothing."""
    baseline = baseline or {}
    if any(not isinstance(row, dict) or not isinstance(row.get("shown"), list) or not isinstance(row.get("body"), str)
           for row in baseline.values()):
        raise ValueError("message_texts baseline is shown_baseline() ({key: {shown, body}}), not a revision map")
    bodies, fresh = [], []
    def timeline(item):  # the message's own position, never the opaque entity id; unpositioned entities last
        position = (item[1].get("value") or {}).get("position")
        return (0, position, str(item[0])) if isinstance(position, int) and not isinstance(position, bool) else (1, 0, str(item[0]))
    def new_lines(lines, before):
        before = list(before)
        for line in lines:  # multiset difference against what was already there
            if line in before:
                before.remove(line)
            else:
                fresh.append(line)
    for key, entity in sorted(reducer.entities.items(), key=timeline):
        value = entity.get("value") or {}
        before = baseline.get(key)
        body = body_texts(value) if key[0] == "message" else []
        if any(needle in text for text in body) and (before is None or before["body"] != body_fingerprint(value)):
            bodies += body
        new_lines(marker_lines(shown_texts(value), emitted, needle), before["shown"] if before else [])
    fresh += marker_lines(fresh_notice_texts(reducer, baseline.get(NOTICES)), emitted, needle)  # by receipt, not value
    return bodies, fresh


def o12_flag_put(reducer, baseline, key):
    """A qualifying put must have advanced the live cursor and installed its value."""
    observed = baseline.get(OBSERVATION, {})
    recorder, cut = observed.get("applied_flags"), observed.get("applied_received")
    if recorder is None or not recorder.known or type(cut) is not int:
        return None
    for entry in reversed(recorder.records):
        row = entry["put"]
        if entry["receipt"] > cut and entry["epoch"] == reducer.epoch and (row.get("kind"), row.get("id")) == key:
            return deepcopy(row)
    return None


def o12_affected_entities(reducer, baseline, emitted, needle="O12-", *, snapshot_publication=False):
    """Bind flags to the controlled message body, never an unrelated/global flag.

    An empty omitted entity needs a separate correlation witness; freshness alone
    cannot identify it as the native message. Existing shown notices still work.
    """
    rows = []
    epoch = getattr(reducer, "epoch", None)
    for key, entity in reducer.entities.items():
        value = entity.get("value") or {}
        body = "".join(body_texts(value))
        if (key[0] != "message" or value.get("role") != "assistant" or not body.startswith("0:" + needle)
                or not emitted.startswith(body)):
            continue
        fingerprint = body_fingerprint(value)
        text_hash = hashlib.sha256(body.encode()).hexdigest()
        # An epoch/ID/block-boundary replay is not current emission evidence.
        # The challenge is unique; reject identical baseline text explicitly too.
        if any(k[0] == "message" and (old.get("body") == fingerprint or old.get("body_text_sha256") == text_hash)
               for k, old in baseline.items()):
            continue
        before = baseline.get(key)
        new_epoch = bool(before and isinstance(epoch, str) and isinstance(before.get("epoch"), str)
                         and epoch != before["epoch"])
        fresh_record = before is None or new_epoch or (
            type(entity.get("rev")) is int and type(before.get("rev")) is int and entity["rev"] > before["rev"])
        flagged = value.get("incomplete") is True
        flag_put = o12_flag_put(reducer, baseline, key) if flagged else None
        snapshot_flag = snapshot_publication and type(entity.get("rev")) is int and fresh_record
        fresh_flag = flagged and fresh_record and (before is None or new_epoch or before.get("incomplete") is not True
                                                   or flag_put is not None or snapshot_flag)
        body_changed = before is None or before["body"] != fingerprint
        if body_changed and not fresh_record:
            raise HarnessGap("O12 correlation gap: body changed without a new public revision")
        if body_changed or fresh_flag:
            rows.append({"kind": key[0], "id": key[1], "epoch": epoch, "rev": entity.get("rev"),
                         "fresh_incomplete": fresh_flag, "baseline_incomplete": before.get("incomplete") if before else None,
                         "baseline_rev": before.get("rev") if before else None,
                         "baseline_epoch": before.get("epoch") if before else None,
                         "flag_put": flag_put, "snapshot_publication": bool(flagged and snapshot_flag),
                         "value": deepcopy(value)})
    return rows


def o12_observation(view, error):
    """Retain client observation state before deciding adapter versus harness."""
    reducer, peer = view.reducer, getattr(view, "peer", None)
    def failure(value):
        return None if value is None else {"type": type(value).__name__, "message": str(value)}
    task = getattr(peer, "task", None)
    read_done = getattr(task, "done", None)
    return {"wait_error": failure(error), "stream": getattr(view, "stream", None),
            "view_known": hasattr(view, "closed") and hasattr(view, "close_notice"),
            "view_closed": getattr(view, "closed", None), "terminal": deepcopy(getattr(view, "close_notice", None)),
            "peer_known": peer is not None and hasattr(peer, "failure") and hasattr(peer, "closed") and callable(read_done),
            "peer_closed": getattr(peer, "closed", None), "peer_failure": failure(getattr(peer, "failure", None)),
            "peer_read_done": read_done() if callable(read_done) else None,
            "reducer_known": hasattr(reducer, "failure") and hasattr(reducer, "staged"),
            "reducer_failure": failure(getattr(reducer, "failure", None)),
            "cursor": deepcopy(getattr(reducer, "cursor", None)), "epoch": getattr(reducer, "epoch", None),
            "sequence": getattr(reducer, "sequence", None), "snapshots": getattr(reducer, "snapshots", None),
            "staged": getattr(reducer, "staged", None) is not None,
            "pending_bytes": len(getattr(reducer, "pending", b"")),
            "trace_dropped": getattr(reducer, "trace_dropped", None)}


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
    recorder = AppliedFlagPuts(view.reducer)
    try:
        await o12_adapter_result(ctx, view, recorder)
    finally:
        recorder.restore()


def o12_emission_chunks():
    """A fresh native-body challenge; preserve the 70x1024-byte workload exactly."""
    needle = "O12-" + secrets.token_hex(16) + ":"
    return [needle + "x" * (1024 - len(needle)) for _ in range(70)], needle


async def o12_adapter_result(ctx, view, recorder):
    chunks, needle = o12_emission_chunks()
    expected = "0:" + "".join(chunks)  # seam 5d9b0590: "<index>:" + chunks
    baseline = shown_baseline(view.reducer, expected, needle, applied_flags=recorder)
    mark = ctx.native.mark()
    # Fixture seam 5d9b0590 (prim-final-review): retain with chunks emits one assistant message as len(chunks)
    # native deltas, text "0:" + "".join(chunks), and one native_emit ledger row (Pi message_end, Codex item/completed).
    await ctx.native.call("retain", count=1, chunks=chunks, timeout=10)
    emitted = [row for row in ctx.native.records("native_emit", after=mark)
               if row.get("type") in ("message_end", "item/completed") and row.get("role") == "assistant"]
    if not emitted or not isinstance(emitted[-1].get("text_bytes"), int) or not isinstance(emitted[-1].get("text_sha256"), str):
        raise HarnessGap("O12 adapter half needs the native fixture's ledger witness of the emitted message "
                         "(native_emit message_end with text_bytes and text_sha256)")
    witness = emitted[-1]
    check(witness["text_bytes"] == len(expected.encode()) and witness["text_sha256"] == hashlib.sha256(expected.encode()).hexdigest(),
          "O12 native emission did not match the controlled body challenge", component="fixture")
    deadline = time.monotonic() + 15  # Original observation budget, shared with the one possible snapshot read.
    first = await o12_capture(ctx, view, recorder, baseline, expected, needle, witness, deadline=deadline)
    retention = o12_retention_gap(first["observation"])
    needs_frontier = (o12_healthy(first["observation"]) and not first["notice_evidence_lost"]
                      and not first["disclosure_observed"]
                      and first["affected_entities"]
                      and all(row["value"].get("final") is True for row in first["affected_entities"]))
    if retention or needs_frontier:
        recorder.restore()
        await o12_snapshot_once(ctx, view, baseline, expected, needle, witness, first, deadline,
                                trigger="retention" if retention else "negative_frontier")
    else:
        o12_grade(first)


def o12_healthy(health):
    return (health["wait_error"] is None and health.get("evidence_error") is None
            and health["applied_flags_known"] and health["view_known"] and health["peer_known"] and health["reducer_known"]
            and health["peer_closed"] is False and health["peer_failure"] is None and health["peer_read_done"] is False
            and health["reducer_failure"] is None and not health["staged"] and health["view_closed"] is None
            and health["terminal"] is None and not health["applied_flag_evidence_lost"])


def o12_retention_gap(health):
    terminal = health.get("terminal") or {}
    reason = terminal.get("reason")
    retention = (isinstance(reason, dict) and
                 (reason.get("reason") == "retention" or
                  reason.get("reason") == "gap" and reason.get("gap") == "retention"))
    return (terminal.get("method") == "stream.gap" and retention
            and health.get("view_closed") == "retention"
            and health.get("reducer_failure") == {"type": "str", "message": "retention"}
            and health.get("view_known") and health.get("reducer_known") and not health.get("staged")
            and health.get("peer_known") and health.get("peer_closed") is False
            and health.get("peer_failure") is None and health.get("peer_read_done") is False
            and health.get("applied_flags_known") and not health.get("applied_flag_evidence_lost")
            and health.get("evidence_error") is None)


def o12_notice_witnesses(reducer, witness):
    out = []
    for index, value in enumerate(getattr(reducer.notices, "since", None) or [], 1):
        text = notice_value_text(value)
        if text is not None and adapter_preserves_or_marks([], witness["text_bytes"], witness["text_sha256"], other_texts=[text]):
            out.append({"window_index": index, "value": deepcopy(value)})
            if len(out) == 16:
                break
    return out


def o12_start_notice_window(reducer):
    """Bootstrap receipts delivered before attach returned, then observe appends during the snapshot wait.
    Healthy op:notice records are applied before sequence handling; unlike puts they cannot be ignored duplicates.
    A missing trace prefix makes negative inference unsafe, even when the cache still looks full/healthy.
    """
    cache = list(reducer.notices)
    trace = [deepcopy(row.get("value")) for row in reducer.trace if row.get("op") == "notice" and "seq" not in row]
    complete = (reducer.trace_dropped == 0 and reducer.failure is None
                and len(cache) <= len(trace) and json_equal(cache, trace[-len(cache):] if cache else []))
    receipts = trace if complete else cache
    window = ReceivedNotices(receipts, maxlen=getattr(reducer.notices, "maxlen", None) or 64)
    window.received = len(receipts)  # Known client receipts, not a native sequence; missing prefix is explicit below.
    window.since = receipts[:window.RECORD]
    window.lost = not complete or len(receipts) > window.RECORD
    reducer.notices = window
    return {"bootstrap_trace_notices": len(trace), "bootstrap_cache_notices": len(cache),
            "bootstrap_trace_dropped": reducer.trace_dropped, "bootstrap_complete": complete}


async def o12_snapshot_once(ctx, old, baseline, expected, needle, witness, first, deadline, *, trigger):
    recovery = {"kind": "o12_snapshot_observation", "trigger_kind": trigger, "attempt": 1,
                "trigger": deepcopy(first["observation"]), "attach": {"after": None, "seed": None},
                "old_stream": old.stream, "new_stream": None}
    ctx.evidence.append(recovery)
    view = recorder = None
    try:
        try:
            # A fresh same-peer attach, not a native retry or continuation seeded with the broken prefix.
            view = await asyncio.wait_for(ctx.attach(peer=old.peer), timeout=max(0, min(2, deadline - time.monotonic())))
            recovery["new_stream"] = view.stream
            if view is old or view.reducer is old.reducer or view.initial != "reset":
                raise HarnessGap("retention reattach did not supply a fresh snapshot reducer")
            recorder = AppliedFlagPuts(view.reducer)
            recovery["notice_window"] = o12_start_notice_window(view.reducer)
            await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0,
                                timeout=max(0, min(2, deadline - time.monotonic())))
            if view.reducer.snapshots < 1 or view.reducer.staged is not None:
                raise HarnessGap("reattach has not installed a complete snapshot")
            if view.reducer.epoch != old.reducer.epoch:
                raise HarnessGap("snapshot observation changed the controlled emission's epoch")
        except Exception as error:
            recovery["error"] = {"type": type(error).__name__, "message": str(error)}
            recovery["observation"] = o12_observation(view, error) if view is not None else None
            recovery["held_messages"] = ([{"kind": key[0], "id": key[1], **deepcopy(entity)}
                                           for key, entity in view.reducer.entities.items() if key[0] == "message"]
                                          if view is not None else [])
            raise HarnessGap("O12 snapshot observation gap: reattach failed or changed epoch; no healthy installed snapshot") from error
        # Keep ORIGINAL pre-emission entity/body/revision baselines. All notices on this new,
        # unseeded stream arrived after the emission; their receipt numbering starts at zero.
        recovered_baseline = dict(baseline)
        recovery["notice_window"].update(received=view.reducer.notices.received,
                                          recorded=len(view.reducer.notices.since), lost=view.reducer.notices.lost)
        recovered_baseline[NOTICES] = {"shown": [], "body": "", "received": 0}
        recovered_baseline[OBSERVATION] = {**baseline[OBSERVATION], "applied_flags": recorder, "applied_received": 0}
        recovery["snapshot_installed"] = True
        recovery["frontier"] = {"epoch": view.reducer.epoch, "cursor": deepcopy(view.reducer.cursor),
                                "snapshots": view.reducer.snapshots,
                                "snapshot_records": [deepcopy(row) for row in view.reducer.trace if row.get("op") == "snapshot"]}
        record = await o12_capture(ctx, view, recorder, recovered_baseline, expected, needle, witness,
                                   recovered=True, phase=trigger + "_snapshot")
        record["notice_evidence_lost"] = record["notice_evidence_lost"] or first["notice_evidence_lost"]
        terminal = record["observation"].get("terminal") or {}
        if terminal.get("method") == "stream.gap":
            raise HarnessGap("O12 snapshot observation gap: second/reattached stream gap")
        o12_grade(record)  # No second reattach, output wait, native call or deadline extension.
    finally:
        if recorder is not None:
            recorder.restore()


async def o12_capture(ctx, view, recorder, baseline, expected, needle, witness, *, recovered=False,
                      phase="initial", deadline=None):
    def observed(reducer):
        texts, others = message_texts(reducer, needle, baseline, expected)
        affected = o12_affected_entities(reducer, baseline, expected, needle, snapshot_publication=recovered)
        positive = (adapter_preserves_or_marks(texts, witness["text_bytes"], witness["text_sha256"], other_texts=others)
                    or any(row["fresh_incomplete"] for row in affected))
        return texts, others, affected, positive
    def settled(reducer):
        _, _, affected, positive = observed(reducer)
        # A completed partial entity must be graded FAIL rather than waiting for
        # a positive verdict and misclassifying the resulting timeout as loss.
        return positive or bool(affected) and all(row["value"].get("final") is True for row in affected)
    wait_error = None
    if not recovered:
        try:
            await ctx.wait_view(view, settled, timeout=max(0, deadline - time.monotonic()))
        except Exception as error:
            wait_error = error  # Only a typed retention gap below permits one public snapshot recovery.
    health = o12_observation(view, wait_error)
    health["applied_flags_known"] = recorder.known
    health["applied_flags_baseline"] = baseline[OBSERVATION]["applied_received"]
    health["applied_flags_received"] = recorder.received
    health["applied_flag_evidence_lost"] = recorder.lost
    try:
        texts, others, affected, verdict = observed(view.reducer)
    except Exception as error:
        health["evidence_error"] = {"type": type(error).__name__, "message": str(error)}
        texts, others, affected, verdict = [], [], [], False
    record = {"kind": "o12_adapter", "phase": phase, "public_frontier": recovered,
                         "emitted": witness, "stream_text_bytes": [len(t.encode()) for t in texts],
                         "other_new_entity_strings": [t[:200] for t in others][:16],
                         "affected_entities": affected,
                         "applied_flag_puts": deepcopy(recorder.records),
                         "incomplete_witnesses": [row for row in affected if row["fresh_incomplete"]],
                         "disclosure_observed": verdict, "notice_evidence_lost": getattr(view.reducer.notices, "lost", False),
                         "notice_witnesses": o12_notice_witnesses(view.reducer, witness),
                         "observation": health}
    ctx.evidence.append(record)
    return record


def o12_grade(record):
    health, verdict, affected = record["observation"], record["disclosure_observed"], record["affected_entities"]
    if health["applied_flag_evidence_lost"]:
        raise HarnessGap("O12 flag observation gap: applied flag evidence exceeded the bounded recorder")
    if not o12_healthy(health):
        raise HarnessGap("O12 observation gap: wait/view/peer/reducer health does not establish an uninterrupted public witness")
    if not verdict and record["notice_evidence_lost"]:
        raise HarnessGap("O12 notice observation gap: more live notices arrived since the baseline than the recorder "
                         "keeps, so a refusal marker may have been received and cannot be excluded")
    if not verdict and (not affected or any(row["value"].get("final") is not True for row in affected)):
        raise HarnessGap("O12 observation gap: only a held prefix/no completed affected entity was observed")
    if not verdict and not record["public_frontier"]:
        raise HarnessGap("O12 observation gap: no public frontier establishes completion of disclosure observation")
    check(verdict,
          "after the producer refusal the adapter keeps the whole emitted text or publicly marks the affected "
          "output incomplete (no silent loss)")
