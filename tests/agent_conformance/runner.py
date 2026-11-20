"""Execute isolated public-wire cases and publish a complete omission inventory."""
import asyncio
import json
from pathlib import Path
import time
import traceback

from .cases import HANDLERS
from .core import PublicImplementation
from .results import CheckFailed, HarnessGap, NotDeclared, Result, declared, declared_module, markdown, report


def validate_manifest(value):
    if not isinstance(value, dict) or not isinstance(value.get("name"), str):
        raise ValueError("implementation needs a name")
    argv = value.get("argv")
    if not isinstance(argv, list) or not argv or not all(isinstance(arg, str) for arg in argv):
        raise ValueError("implementation argv must be a nonempty array of literal strings")
    if value.get("wire", "pi") not in ("pi", "codex", "acp"):
        raise ValueError("unknown scripted native wire")
    count = value.get("endpoints", 1)
    if not isinstance(count, int) or isinstance(count, bool) or not 1 <= count <= 200:
        raise ValueError("fixture endpoint count must be 1..200")
    return value


async def run_case(case, root, manifest, vqro, hc, *, factory=PublicImplementation, handlers=None):
    """Factory injection is for harness self-tests, never an adapter profile."""
    start, ctx, declarations, evidence = time.monotonic(), None, {}, []
    status, component, detail = "fail", "harness", "case did not complete"
    try:
        script = ({"script": [], "consume_input": True} if case.id == "D14" else
                  {"script": [], "abort_inserts_steering": True} if case.id == "D10" else None)
        ctx = factory(Path(root), manifest, vqro, hc, script=script)
        await ctx.start()
        facts = await ctx.facts()
        declarations = {"provider": ctx.provider.get("capabilities", {}), "resource": facts.get("caps", {}),
                        "host": facts.get("host"), "versions": ctx.metadata}
        provider_version = declarations["provider"].get("agent", {}).get("v")
        if case.id != "C1" and (type(facts.get("v")) is not int or facts["v"] != 0
                                 or type(provider_version) is not int or provider_version != 0):
            raise CheckFailed("not a declared agent-v0 implementation; no capability omission inferred")
        if case.operation:
            declared(facts.get("caps", {}), case.operation)
        if getattr(case, "module", None):
            declared_module(facts.get("caps", {}), case.module)
        handler = (HANDLERS if handlers is None else handlers).get(case.id)
        if handler is None:
            raise HarnessGap("normative case has no executable runner yet")
        if case.operation == "agent.send" or (case.operation == "agent.respond" and case.id != "R7") or case.id.startswith("O") or case.id in ("C3", "C7", "L1", "M1") or (case.id == "C2" and "agent.send" in facts.get("caps", {})):
            facts = await ctx.ensure_session()
            declarations["resource"] = facts.get("caps", {})
        await handler(ctx)
        status, component, detail = "pass", None, "public-wire assertions passed"
    except NotDeclared as error:
        status, component, detail = "unsupported-declared", None, str(error)
        declarations["omission"] = {"path": error.path, "actual": error.actual}
    except CheckFailed as error:
        status, component, detail = "fail", error.component, str(error)
    except Exception as error:
        status, component, detail = "fail", "harness", f"{type(error).__name__}: {error}"
        Path(root).mkdir(parents=True, exist_ok=True)
        (Path(root) / "harness-error.txt").write_text(traceback.format_exc())
    finally:
        if ctx is not None:
            evidence.extend(ctx.evidence)
            try:
                await ctx.close()
            except Exception as error:
                original = {"status": status, "component": component, "detail": detail}
                evidence.append({"kind": "result_before_cleanup_failure", **original})
                status, component, detail = "fail", "cleanup", f"owned-root cleanup failed: {error}"
    result = Result(case.id, case.title, status, component, detail, evidence, declarations,
                    round((time.monotonic() - start) * 1000), str(root))
    Path(root).mkdir(parents=True, exist_ok=True)
    (Path(root) / "result.json").write_text(json.dumps(result.json(), ensure_ascii=False, indent=2) + "\n")
    return result


async def run_selection(root, manifest, selected, vqro, hc):
    rows = []
    for index, case in enumerate(selected):
        row = await run_case(case, Path(root) / f"{index:02}", manifest, vqro, hc)
        rows.append(row)
        # Receipted progress is a real case outcome, not a heartbeat.
        print(json.dumps({"case": row.case, "status": row.status, "component": row.component,
                          "detail": row.detail}), flush=True)
    metadata = {key: value for key, value in manifest.items() if key != "argv"}
    metadata["launch_argv_template"] = manifest["argv"]
    metadata["versions_and_limits"] = [row.declarations.get("versions", {}) for row in rows if row.declarations]
    document = report(metadata, rows, selected)
    (Path(root) / "results.json").write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    (Path(root) / "results.md").write_text(markdown(document))
    return document
