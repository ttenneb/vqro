"""Bounded, source/run/image-bound shared evidence; never another runtime.

Build manifests are explicit asserted build-source records, not cryptographic
proof of compilation. Their images, immutable source objects, component equality,
exact test definitions and the gate's actual recorded logs are independently
checked. A detached snippet/summary cannot substitute for a missing run artifact.
"""
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess

from .results import CheckFailed, HarnessGap, check

MAX_ARTIFACT = 4 << 20
HARNESS_REPO = Path(__file__).resolve().parents[2]

# Semantic regression identities are harness requirements, not caller-selected labels.
REGRESSIONS = {
    "O12": {
        "append_refusal_atomic": ("provider_client::items::tests::small_appends_past_the_record_bound_are_refused_atomically_for_bytes_and_text", "src/provider_client/items.rs"),
    },
    "O10": {
        "journal_envelope_and_floor": ("provider_client::items::tests::journal_accounting_obeys_retention_buffer_and_one_record_floor", "src/provider_client/items.rs"),
        "cut_versions_and_release": ("provider_client::items::tests::active_cut_accounting_releases_old_versions_on_close_gap_and_epoch_step", "src/provider_client/items.rs"),
    },
    "O3": {
        "snapshot_hold": ("provider_client::items::tests::a_large_snapshot_is_streamed_in_pieces_and_its_post_cut_records_are_held", "src/provider_client/items.rs"),
        "overflow_before_next_piece": ("provider_client::items::tests::overflowing_an_unfinished_cut_gaps_before_more_pieces_and_installs_nothing", "src/provider_client/items.rs"),
        "retention_reset_recovery": ("provider_client::items::tests::a_slow_viewer_gets_a_retention_gap_deterministically_and_recovers_by_reset", "src/provider_client/items.rs"),
    },
    "L3": {
        "stale_source_ignored": ("stale_source_events_and_disconnect_cannot_disable_a_replacement", "tests/providers.rs"),
        "old_queued_effect_rejected": ("queued_effect_from_an_old_native_connection_is_rejected_before_execution", "tests/providers.rs"),
    },
    "O11": {
        "reserved_budget": ("rpc::tests::reserved_calls_neither_count_against_nor_are_refused_by_the_pending_limit", "src/rpc.rs"),
        "opens_outside_effect_limit": ("provider_client::qa2_tests::item_streams_have_their_own_budget_and_opens_are_outside_the_effect_limit", "src/provider_client.rs"),
        "writing_stream_closes": ("slow_consumer_closes_the_writing_stream_and_spares_its_idle_sibling_and_other_connections", "tests/core.rs"),
        "idle_sibling_stays_open": ("slow_consumer_closes_the_writing_stream_and_spares_its_idle_sibling_and_other_connections", "tests/core.rs"),
    },
}
CLOSURES = {
    "O12": {"adapter": {"src/provider_client/items.rs"}},
    "O10": {"adapter": {"src/provider_client/items.rs"}},
    "O3": {"adapter": {"src/provider_client/items.rs"}},
    "L3": {"adapter": {"src/provider_client.rs"}, "core": {"src/provider.rs", "src/provider_wire.rs", "src/server.rs"}},
    "O11": {"adapter": {"src/provider_client.rs", "src/provider_client/items.rs"}, "core": {"src/rpc.rs", "src/server.rs", "src/provider_wire.rs"}},
}


def bounded(path):
    path = Path(path)
    if not path.is_absolute() or not path.is_file() or path.stat().st_size > MAX_ARTIFACT:
        raise HarnessGap("component evidence artifact is absent or exceeds4MiB")
    data = path.read_bytes()
    if len(data) > MAX_ARTIFACT:
        raise HarnessGap("component artifact grew beyond4MiB")
    return data


def artifact(row):
    if not isinstance(row, dict) or not isinstance(row.get("path"), str) or not re.fullmatch(r"[0-9a-f]{64}", str(row.get("sha256", ""))):
        raise HarnessGap("component evidence lacks a pinned artifact")
    data = bounded(row["path"])
    if hashlib.sha256(data).hexdigest() != row["sha256"]:
        raise HarnessGap("component evidence checksum changed: " + row["path"])
    return data


def document(row):
    try:
        value = json.loads(artifact(row))
    except (ValueError, UnicodeError) as error:
        raise HarnessGap("component artifact is not valid JSON") from error
    if not isinstance(value, dict):
        raise HarnessGap("component artifact is not a JSON object")
    return value


def git(repo, *args):
    try:
        return subprocess.run(["git", "--no-pager", "--no-replace-objects", "-C", str(repo), *args],
            env={"PATH": os.defpath, "GIT_CONFIG_NOSYSTEM": "1"}, check=True,
            capture_output=True, timeout=2).stdout
    except (OSError, subprocess.SubprocessError) as error:
        raise HarnessGap("pinned component source object is unavailable") from error


def commit(repo, value, *, full=True):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}" if full else r"[0-9a-f]{7,40}", value):
        raise HarnessGap("component source/receipt commit is not an immutable identifier")
    actual = git(repo, "rev-parse", "--verify", value + "^{commit}").decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", actual):
        raise HarnessGap("component commit did not resolve")
    return actual


def source(repo, revision, path):
    if not isinstance(path, str) or not path.startswith(("src/", "tests/")) or ":" in path or ".." in PurePosixPath(path).parts:
        raise HarnessGap("component source path is not a bounded repository source file")
    try:
        size = int(git(repo, "cat-file", "-s", revision + ":" + path))
    except ValueError as error:
        raise HarnessGap("invalid component source size") from error
    if not 0 < size <= MAX_ARTIFACT:
        raise HarnessGap("component source exceeds its evidence bound")
    return git(repo, "show", revision + ":" + path)


def gate_log(row, receipt, name, key):
    steps = [step for step in receipt["steps"] if step["name"] == name]
    if len(steps) != 1 or not isinstance(steps[0].get("log"), str):
        raise HarnessGap("shared receipt lacks its unique recorded log: " + name)
    data = artifact(row.get(key))
    # An archive is allowed only while its identity can be checked against the
    # actual log recorded by this receipt, not an independently selected snippet.
    if data != bounded(steps[0]["log"]):
        raise HarnessGap("component artifact is not this gate run's log: " + name)
    return data


def component(ctx, ident):
    row = ctx.manifest.get("component_evidence", {}).get(ident)
    if not isinstance(row, dict):
        raise HarnessGap(ident + " requires image-bound shared-component evidence")
    if row.get("adapter_image_sha256") != ctx.metadata.get("adapter_image", {}).get("sha256"):
        raise HarnessGap("component evidence is not linked to the executed adapter image")
    if row.get("known_counterexamples"):
        raise CheckFailed("shared component has unresolved counterexamples: " + json.dumps(row["known_counterexamples"]), component="producer")
    repo = Path(row.get("source_repo", HARNESS_REPO))
    if not repo.is_absolute() or not repo.is_dir():
        raise HarnessGap("component source repository is absent")
    producer = commit(repo, row.get("producer_source"))
    # The manifest may locate an image's source checkout, not choose which
    # producer history we trust. Pin our own main and prove ancestry there.
    trusted_main = git(HARNESS_REPO, "rev-parse", "--verify", "refs/heads/main^{commit}").decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", trusted_main):
        raise HarnessGap("trusted harness main did not resolve to an immutable commit")
    try:
        git(HARNESS_REPO, "merge-base", "--is-ancestor", producer, trusted_main)
    except HarnessGap as error:
        raise HarnessGap("producer source is not reachable from trusted harness main") from error
    receipt = document(row.get("receipt"))
    steps = receipt.get("steps")
    if (receipt.get("kind") != "vqro.check-receipt" or receipt.get("result") != "pass"
        or receipt.get("mode") != "full" or receipt.get("failed") != []
        or type(receipt.get("finished_unix")) is not int or receipt["finished_unix"] <= 0
        or not isinstance(steps, list) or not steps
        or any(not isinstance(step, dict) or not isinstance(step.get("name"), str)
               or not step["name"] or step.get("result") != "PASS" for step in steps)
        or len({step["name"] for step in steps}) != len(steps)):
        raise HarnessGap("shared-component full-gate receipt is malformed or not passing")
    if commit(repo, receipt.get("commit"), full=False) != producer:
        raise HarnessGap("gate receipt commit differs from the pinned producer source")
    build = document(row.get("build_manifest"))
    if build.get("kind") != "vqro.component-build-v1":
        raise HarnessGap("shared component lacks its immutable build-source/image record")
    files = row.get("source_files")
    if not isinstance(files, dict) or not files:
        raise HarnessGap("shared component does not identify its source closure")
    if any(not isinstance(files.get(role), list) or not paths <= set(files[role]) for role, paths in CLOSURES[ident].items()):
        raise HarnessGap("shared component omits a required runtime source dependency")
    for role, paths in files.items():
        if role not in ("adapter", "core") or not isinstance(paths, list) or not paths or len(paths) != len(set(paths)):
            raise HarnessGap("invalid component source closure")
        image = build.get(role, {})
        actual_hash = (ctx.metadata.get("adapter_image", {}).get("sha256") if role == "adapter"
                       else ctx.metadata.get("core_image_sha256"))
        if not re.fullmatch(r"[0-9a-f]{64}", str(actual_hash)) or image.get("sha256") != actual_hash:
            raise HarnessGap("build record does not identify the executed " + role + " image")
        image_source = commit(repo, image.get("source_commit"))
        for path in paths:
            if source(HARNESS_REPO, producer, path) != source(repo, image_source, path):
                raise HarnessGap("executed image component differs from tested source: " + path)
    if "adapter" not in files:
        raise HarnessGap("producer inheritance does not identify the adapter's component")
    if ident in ("L3", "O11") and ("core" not in files or row.get("core_image_sha256") != ctx.metadata.get("core_image_sha256")):
        raise HarnessGap("shared core evidence is not linked to the executed core component")
    ctx.evidence.append({"kind": "trusted_producer_main", "repo": str(HARNESS_REPO),
                         "main": trusted_main, "producer_source": producer})
    return row, receipt, HARNESS_REPO, producer


def regressions(row, receipt, repo, producer, ident):
    expected = REGRESSIONS[ident]
    names, locations = row.get("required_tests"), row.get("test_sources")
    if names != {key: name for key, (name, _) in expected.items()} or not isinstance(locations, dict):
        raise HarnessGap("shared evidence lacks the required exact semantic regression identities")
    text = gate_log(row, receipt, "cargo-test", "unit_log").decode()
    if not re.search(r"^test result: ok\. [0-9]+ passed; 0 failed;", text, re.MULTILINE) or re.search(r"test result: FAILED|\.\.\. FAILED", text):
        raise HarnessGap("recorded cargo-test log does not confirm a successful run")
    binary, passed = None, set()
    for line in text.splitlines():
        running = re.search(r"Running (?:unittests )?(\S+\.rs) \(", line)
        if running:
            binary = running[1]
        elif line.startswith("test ") and line.endswith(" ... ok"):
            passed.add((binary, line[5:-7]))
    for name, path in expected.values():
        if locations.get(name) != path:
            raise HarnessGap("exact regression source locator differs from its required source")
        leaf = name.split("::")[-1]
        definition = r"#\[(?:test|tokio::test)(?:\([^\]]*\))?\]\s*(?:#\[[^\]]*\]\s*)*(?:async\s+)?fn\s+" + re.escape(leaf) + r"\s*\("
        if not re.search(definition, source(repo, producer, path).decode()):
            raise HarnessGap("regression is not a test at the pinned commit: " + name)
        binary = path if path.startswith("tests/") else "src/lib.rs"
        check((binary, name) in passed,
              "exact regression did not pass in the recorded gate binary: " + name, component="producer")


def inherited(ctx, ident, row, receipt):
    ctx.evidence.append({"kind": "inherited_component", "case": ident, "component": row.get("component"),
        "producer_source": row["producer_source"], "receipt_commit": receipt["commit"],
        "receipt_finished_unix": receipt["finished_unix"], "required_tests": row["required_tests"],
        "build_source_linkage": "asserted build manifest; images, source equality and recorded run checked",
        "artifacts": {key: value for key, value in row.items() if isinstance(value, dict) and "sha256" in value},
        "not_a_separate_runtime": True})


def producer_overflow(ctx):
    row, receipt, repo, producer = component(ctx, "O3")
    regressions(row, receipt, repo, producer, "O3")
    inherited(ctx, "O3", row, receipt)


def retained_memory_accounting(ctx):
    row, receipt, repo, producer = component(ctx, "O10")
    regressions(row, receipt, repo, producer, "O10")
    inherited(ctx, "O10", row, receipt)
    ctx.evidence.append({"kind": "producer_memory_accounting", "journal": "encoded retained record bytes",
        "active_cuts": "encoded shared entity versions per cut; weak Arc release witnesses",
        "release": ["close", "retention_gap", "next_step_after_epoch"],
        "process_transport_allocator_overhead": "separate, not charged to these encoded-byte declarations"})


def host_generation_fence(ctx):
    row, receipt, repo, producer = component(ctx, "L3")
    regressions(row, receipt, repo, producer, "L3")
    inherited(ctx, "L3", row, receipt)


def connection_scale(ctx):
    row, receipt, repo, producer = component(ctx, "O11")
    regressions(row, receipt, repo, producer, "O11")
    log = gate_log(row, receipt, "e2e-item-streams", "scale_log").decode()
    runs = []
    for line in log.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("result") == "pass" and value.get("failures") == [] and isinstance(value.get("out"), str):
            runs.append(value)
    if len(runs) != 1:
        raise HarnessGap("scale gate log does not identify its unique successful output root")
    summary = document(row.get("summary"))
    if artifact(row["summary"]) != bounded(Path(runs[0]["out"]) / "summary.json"):
        raise HarnessGap("scale summary is not from the recorded gate run")
    check(summary.get("stalled_attached") == 200, "stalled connection did not hold200 streams", component="core")
    for name in ("v1", "v2", "v3"):
        viewer = summary.get(name, {})
        check(viewer.get("agents") == viewer.get("identical_to_runtime") == 200, "scale viewer diverged", component="core")
        counters = viewer.get("counters", {})
        check(all(counters.get(key) == 0 for key in ("bad_json", "bad_record", "bad_utf8", "duplicate", "missing", "stale_base", "unknown_entity")),
              "scale reconstruction has integrity errors", component="core")
        check(counters.get("gaps_retention", 0) > 0, "producer retention recovery was not exercised", component="core")
    inherited(ctx, "O11", row, receipt)
