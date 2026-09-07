#!/usr/bin/env python3
"""Reference vqro declarative pane-inspector service."""

import json
import os
import sys

SERVICE_ID = os.environ.get("VQRO_EXTENSION_SERVICE_ID", "vqro.terminal-inspector.telemetry")
print(json.dumps({"type": "hello", "protocol": 1, "service_id": SERVICE_ID}), flush=True)


def value_or_dash(value):
    return "—" if value is None else str(value)


def bytes_label(value):
    if value is None:
        return "—"
    for suffix in ("B", "KiB", "MiB", "GiB"):
        if value < 1024 or suffix == "GiB":
            return f"{value:.1f}{suffix}" if isinstance(value, float) else f"{value}{suffix}"
        value /= 1024
    return "—"


def render(telemetry):
    agent = telemetry.get("display_agent") or telemetry.get("agent") or "none"
    status = telemetry.get("agent_status", "unknown")
    labels = telemetry.get("state_labels") or {}
    agent_items = [
        {"key": "Agent", "value": agent, "tone": "accent"},
        {"key": "Activity", "value": status, "tone": "default"},
    ]
    agent_items.extend({"key": key, "value": value, "tone": "muted"} for key, value in sorted(labels.items()))

    rows = []
    for process in telemetry.get("processes", []):
        cpu = process.get("cpu_tenths_percent")
        cpu_label = "—" if cpu is None else f"{cpu / 10:.1f}%"
        rows.append([
            str(process.get("pid", "—")),
            process.get("name", "—"),
            cpu_label,
            bytes_label(process.get("resident_bytes")),
            value_or_dash(process.get("thread_count")),
        ])

    availability = telemetry.get("availability", "unavailable")
    process_blocks = []
    if availability == "available":
        process_blocks.append({
            "type": "table",
            "columns": ["PID", "PROCESS", "CPU", "MEMORY", "THREADS"],
            "rows": rows,
        })
    else:
        process_blocks.append({
            "type": "text",
            "text": telemetry.get("detail") or f"Telemetry {availability}",
            "tone": "warning",
        })

    return {
        "title": "Terminal intelligence",
        "subtitle": f"{telemetry.get('pane_id', 'pane')} · supervised extension",
        "sections": [
            {"id": "agent", "title": "Agent activity", "blocks": [{"type": "key_value", "items": agent_items}]},
            {"id": "processes", "title": "Process telemetry", "blocks": process_blocks},
        ],
        "actions": [{"id": "refresh", "label": "Refresh", "destructive": False}],
    }


for line in sys.stdin:
    request = {}
    try:
        request = json.loads(line)
        if request.get("type") != "request" or request.get("method") != "inspector.render":
            raise ValueError("unsupported request")
        response = {"type": "response", "id": request["id"], "result": render(request.get("params") or {})}
    except Exception as error:
        response = {"type": "response", "id": request.get("id", 0) if isinstance(request, dict) else 0, "error": str(error)}
    print(json.dumps(response, separators=(",", ":")), flush=True)
