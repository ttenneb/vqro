"""Bounded, image-bound inherited component evidence (not another runtime).

The program permits shared producer/transport checks to be inherited instead of
starting another 200-agent benchmark for every adapter. Missing linkage, stale
receipts and known counterexamples remain failures, never capability omissions.
"""
import hashlib
import json
from pathlib import Path

from .results import CheckFailed, HarnessGap, check


def artifact(row):
    if not isinstance(row, dict) or not isinstance(row.get("path"), str) or not isinstance(row.get("sha256"), str):
        raise HarnessGap("component evidence lacks a pinned artifact")
    path = Path(row["path"])
    if not path.is_absolute() or not path.is_file() or path.stat().st_size > 4 << 20:
        raise HarnessGap("component evidence artifact is absent or exceeds4MiB")
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != row["sha256"]:
        raise HarnessGap("component evidence checksum changed: " + str(path))
    return data


def component(ctx, ident):
    row = ctx.manifest.get("component_evidence", {}).get(ident)
    if not isinstance(row, dict):
        raise HarnessGap(ident + " requires image-bound shared-component evidence")
    if row.get("adapter_image_sha256") != ctx.metadata.get("adapter_image", {}).get("sha256") or not row.get("producer_source"):
        raise HarnessGap("component evidence is not linked to the executed adapter image/producer source")
    receipt = json.loads(artifact(row.get("receipt")))
    if receipt.get("kind") != "vqro.check-receipt" or receipt.get("result") != "pass" or receipt.get("mode") != "full":
        raise HarnessGap("shared-component full-gate receipt is not passing")
    ctx.evidence.append({"kind": "inherited_component", "case": ident, "component": row.get("component"),
                         "producer_source": row["producer_source"], "receipt_commit": receipt.get("commit"),
                         "artifacts": {key: value for key, value in row.items() if isinstance(value, dict) and "sha256" in value},
                         "not_a_separate_runtime": True})
    if row.get("known_counterexamples"):
        raise CheckFailed("shared component has unresolved counterexamples: " + json.dumps(row["known_counterexamples"]), component="producer")
    return row, receipt


def producer_overflow(ctx):
    row, receipt = component(ctx, "O3")
    check(any(step["name"] == "cargo-test" and step["result"] == "PASS" for step in receipt.get("steps", [])),
          "shared producer tests did not pass", component="producer")
    text = artifact(row.get("unit_log")).decode()
    names = row.get("required_tests", {})
    if not isinstance(names, dict) or not {"snapshot_hold", "overflow_before_next_piece", "retention_reset_recovery"} <= names.keys():
        raise HarnessGap("O3 needs snapshot hold, unfinished-cut overflow and reset-recovery proofs")
    for name in names.values():
        check(any(name in line and line.rstrip().endswith("... ok") for line in text.splitlines()),
              "required producer regression did not pass: " + name, component="producer")


def host_generation_fence(ctx):
    row, _ = component(ctx, "L3")
    if row.get("core_image_sha256") != ctx.metadata.get("core_image_sha256") or not row.get("core_image_sha256"):
        raise HarnessGap("L3 fence evidence is not linked to the executed core image")
    names = row.get("required_tests", {})
    if not isinstance(names, dict) or not {"stale_source_ignored", "old_queued_effect_rejected"} <= names.keys():
        raise HarnessGap("L3 requires stale-source and old-queued-effect fence regressions")
    text = artifact(row.get("unit_log")).decode()
    for name in names.values():
        check(any(name in line and line.rstrip().endswith("... ok") for line in text.splitlines()),
              "host incarnation fence regression did not pass: " + name, component="core")


def connection_scale(ctx):
    row, receipt = component(ctx, "O11")
    if row.get("core_image_sha256") != ctx.metadata.get("core_image_sha256") or not row.get("core_image_sha256"):
        raise HarnessGap("O11 transport evidence is not linked to the executed core image")
    check(any(step["name"] == "e2e-item-streams" and step["result"] == "PASS" for step in receipt.get("steps", [])),
          "shared item-stream scale gate did not pass", component="core")
    summary = json.loads(artifact(row.get("summary")))
    check(summary.get("stalled_attached") == 200, "stalled connection did not hold200 streams", component="core")
    for name in ("v1", "v2", "v3"):
        viewer = summary.get(name, {})
        check(viewer.get("agents") == viewer.get("identical_to_runtime") == 200, "scale viewer diverged", component="core")
        counters = viewer.get("counters", {})
        check(all(counters.get(key) == 0 for key in ("bad_json", "bad_record", "bad_utf8", "duplicate", "missing", "stale_base", "unknown_entity")),
              "scale reconstruction has integrity errors", component="core")
        check(counters.get("gaps_retention", 0) > 0, "producer retention recovery was not exercised", component="core")
    text = artifact(row.get("unit_log")).decode()
    names = row.get("required_tests", {})
    if not isinstance(names, dict) or not {"reserved_budget", "opens_outside_effect_limit", "writing_stream_closes", "idle_sibling_stays_open"} <= names.keys():
        raise HarnessGap("O11 requires per-stream shared-budget and idle-sibling regression evidence")
    for name in names.values():
        check(any(name in line and line.rstrip().endswith("... ok") for line in text.splitlines()),
              "connection budget/closure regression did not pass: " + name, component="core")
