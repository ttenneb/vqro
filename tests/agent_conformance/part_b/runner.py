# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Receipted Part B runner; omissions/fixture failures cannot become successes."""
import hashlib
import json
from pathlib import Path
import shutil
import re
import time
import traceback

from ..results import CheckFailed, FixtureFailed, HarnessGap, NotApplicable, Result, markdown, report
from ..wire import RpcError
from .catalog import CASES
from .cases import HANDLERS
from .context import PublicCoordination


def validate_manifest(value):
    if not isinstance(value, dict) or not isinstance(value.get("name"), str) or not value["name"]:
        raise ValueError("coordination implementation name required")
    source = value.get("extensions")
    if not isinstance(source, str) or not Path(source).is_absolute():
        raise ValueError("absolute target extension directory required")
    count = value.get("agents", 2)
    if type(count) is not int or not 2 <= count <= 200:
        raise ValueError("Part B baseline requires 2..200 declared fake endpoints")
    specs=value.get("specs",{})
    if specs and (not isinstance(specs,dict) or set(specs)!={"agent","coordination","conformance"}
            or not all(isinstance(path,str) and Path(path).is_absolute() for path in specs.values())):
        raise ValueError("specs must name the three explicit absolute contract paths")
    driver = value.get("core_driver")
    if driver is not None and (not isinstance(driver, list) or not driver or
            not all(isinstance(arg, str) for arg in driver) or not Path(driver[0]).is_absolute()):
        raise ValueError("core_driver must be an explicit literal argv with absolute executable")
    return value


async def run_case(case, root, manifest, vqro, hc=None, *, factory=PublicCoordination, handlers=None):
    start = time.monotonic()
    context, evidence, declarations = None, [], {}
    status, component, detail, reason = "fail", "harness", "case did not execute", None
    try:
        handler = (HANDLERS if handlers is None else handlers).get(case.id)
        if handler is None:
            raise HarnessGap("normative Part B case has no executable probe yet")
        context = factory(Path(root), manifest, vqro, hc)
        await context.start()
        declarations = {"coordination": context.status, "versions": context.metadata}
        await handler(context)
        if context.assertions == 0:
            raise HarnessGap("handler made no applicable policy assertions; setup is not conformance")
        status, component, detail = "pass", None, "public-core policy assertions passed"
    except NotApplicable as error:
        status,component,detail="not-applicable",None,str(error)
        declarations["exclusion"]={"path":error.path,"actual":error.actual}
    except HarnessGap as error:
        status,component,detail,reason="fail","harness",str(error),"harness_gap"
    except CheckFailed as error:
        status, component, detail = "fail", error.component, str(error)
    except RpcError as error:
        status, component, detail = "fail", "coordination", f"unexpected public RPC refusal: {error.kind}: {error}"
    except Exception as error:
        status, component, detail = "fail", "harness", f"{type(error).__name__}: {error}"
        Path(root).mkdir(parents=True, exist_ok=True)
        (Path(root) / "harness-error.txt").write_text(traceback.format_exc())
    finally:
        if context is not None:
            evidence.extend(context.evidence)
            try:
                await context.close()
                # Cleanup evidence is appended by close, and is not optional proof.
                evidence = list(context.evidence)
            except Exception as error:
                evidence.append({"kind": "result_before_cleanup_failure", "status": status,
                                 "component": component, "detail": detail})
                status, component, detail = "fail", "cleanup", f"owned fixture cleanup failed: {error}"
    result = Result(case.id, case.title, status, component, detail, evidence, declarations,
                    round((time.monotonic() - start) * 1000), str(root), reason=reason)
    Path(root).mkdir(parents=True, exist_ok=True)
    (Path(root) / "result.json").write_text(json.dumps(result.json(), indent=2, ensure_ascii=False) + "\n")
    return result


def freeze(root, manifest, vqro):
    """One source/binary pin for the entire table, never a moving checkout per row."""
    root = Path(root); source = Path(manifest["extensions"]); frozen = root/"target"
    paths = [*sorted(source.glob("*.py")), source/"manifests/coord.json"]
    if not paths[-1].is_file(): raise FixtureFailed("target coord manifest absent")
    hashes = {}
    for path in paths:
        relative = path.relative_to(source); destination = frozen/relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        data = path.read_bytes(); destination.write_bytes(data)
        hashes[str(relative)] = hashlib.sha256(data).hexdigest()
    if any(hashlib.sha256(path.read_bytes()).hexdigest() != hashes[str(path.relative_to(source))] for path in paths):
        raise FixtureFailed("target checkout changed while capturing immutable fixture")
    binaries = root/"bin"; binaries.mkdir()
    def binary(path, name):
        destination=binaries/name; shutil.copy2(path,destination)
        if hashlib.sha256(Path(path).read_bytes()).digest()!=hashlib.sha256(destination.read_bytes()).digest():
            raise FixtureFailed("core binary changed during immutable capture")
        return str(destination)
    pinned = {**manifest,"source_extensions":str(source),"extensions":str(frozen),"target_sha256":hashes}
    pinned_vqro = binary(vqro,"vqro")
    if manifest.get("core_driver"):
        pinned["core_driver"]=[binary(manifest["core_driver"][0],"core-driver"),*manifest["core_driver"][1:]]
    native=root/"native.py"; shutil.copy2(Path(__file__).with_name("native.py"),native)
    pinned["native_fixture"]=str(native)
    pinned["specs"]={}
    for name,path in manifest.get("specs",{}).items():
        data=Path(path).read_bytes();destination=root/"specs"/(name+".md")
        destination.parent.mkdir(exist_ok=True);destination.write_bytes(data)
        pinned["specs"][name]={"path":str(destination),"sha256":hashlib.sha256(data).hexdigest()}
        if name=="conformance":
            text=data.decode()
            if "# Part B:" not in text or "## Running" not in text: raise HarnessGap("pinned conformance specification lacks Part B boundaries")
            text=text.split("# Part B:",1)[1].split("## Running",1)[0]
            normative=set(re.findall(r"\b[IHGTL][0-9]+\b",text))|{"S1"}
            if normative!={case.id for case in CASES}:
                raise HarnessGap("Part B inventory differs from pinned normative specification: "+str(sorted(normative.symmetric_difference({case.id for case in CASES}))))
    return pinned,pinned_vqro


async def run_selection(root, manifest, selected, vqro, hc=None):
    rows = []; capture_error = None
    try: manifest, vqro = freeze(root,manifest,vqro)
    except (CheckFailed,OSError) as error: capture_error = error
    for index, case in enumerate(selected):
        if capture_error is None:
            row = await run_case(case, Path(root) / f"{index:02}", manifest, vqro, hc)
        else:
            row = Result(case.id,case.title,"fail",getattr(capture_error,"component","fixture"),
                f"immutable capture failed: {capture_error}",[],{},0,str(root),
                reason="harness_gap" if isinstance(capture_error,HarnessGap) else None)
        rows.append(row)
        print(json.dumps({"case": row.case, "status": row.status, "component": row.component,
                          "detail": row.detail}), flush=True)
    metadata = {**manifest, "real_runtime_certification": False, "fixture_native": True}
    document = report(metadata, rows, selected, part="B", inventory=CASES)
    document["subcase_not_applicable"] = sum(e.get("applicability") == "not-applicable"
        for row in rows for e in row.evidence)
    document["qualification_complete"] = (document["complete_inventory"] and document["passed_selection"]
        and set(manifest.get("specs",{}))=={"agent","coordination","conformance"})
    (Path(root) / "results.json").write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    (Path(root) / "results.md").write_text(markdown(document))
    return document
