#!/usr/bin/env python3
"""Release check: bundled manifests declare what they provide and every cross-extension use.

Static scan (Python `ast`, never imports or runs an extension) of each bundled extension's program and the local
helpers it imports (docs/research/declarations-and-ui-clients.md, section 7):

- string constants equal to another extension's provided command (`command/<name>`) are uses;
- `ext.live.get("x")` / `ext.live["x"]` read live data `data/x`;
- `"doc/..."` and `"view/..."` constants are owned (in own `provides`) or used (provided by another, or by the core);
- `ext.command("x")` registrations and `ext.publish("x", ...)` publications must be in own `provides`;
- every `command/` an extension provides is registered somewhere in its sources;
- a few RPC methods imply abilities (`resource.spawn` -> `ability/vqro.terminals`, ...).

A use must be declared in `needs` or `works_with`. Exit 1 with one line per problem; `--verbose` prints the map.
"""
import ast
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXT = os.path.join(ROOT, "extensions")
CAP = re.compile(r"^[A-Za-z0-9._*-]+/[A-Za-z0-9._/*-]+(@[1-9][0-9]*)?$")
COMMAND = re.compile(r"^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)+$")
CORE_DOCS = {"doc/settings/tui", "doc/settings/theme", "doc/settings/icons", "doc/principals/owner", "doc/principals/delegations/*"}
# Shared conventions documented in core-protocol.md that any extension may read and write.
CORE_DOC_PREFIXES = ("doc/layout/",)  # layout settings `doc/layout/<name>` = {cwd?, title?}
ABILITY_METHODS = {"resource.spawn": "ability/vqro.terminals", "resource.forget": "ability/vqro.resource.forget"}
# Programs that are part of an extension beyond its argv: the Pi launcher starts the bridge in a terminal.
EXTRA_SOURCES = {"pi": ["pi_bridge"]}


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def cap_key(cap):
    return cap.split("@", 1)[0]


def entries(manifest, field):
    out = []
    for item in manifest.get(field) or []:
        cap = item if isinstance(item, str) else (item or {}).get("cap") if isinstance(item, dict) else None
        out.append(cap)
    return out


def sources(name, argv):
    files = [os.path.join(EXT, a) for a in argv if a.endswith(".py")]
    for extra in EXTRA_SOURCES.get(name, []):
        path = os.path.join(EXT, extra)
        files += sorted(os.path.join(path, f) for f in os.listdir(path) if f.endswith(".py"))
    return files


def helper_functions(tree):
    """Functions imported from local helper modules (not the SDK), as (module file, names)."""
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module and node.module not in ("vqro_ext", "vqro_provider"):
            path = os.path.join(EXT, node.module.replace(".", "/") + ".py")
            if os.path.isfile(path):
                yield path, {a.name for a in node.names}


def scan(files):
    """(strings, live reads, registrations, publications, registration prefixes, command references) found in the
    files and the helper functions they use. A command reference is a string in command position: a `"command"`
    value in a dict literal or the first argument of `invoke`."""
    strings, live, registered, published, refs = set(), set(), set(), set(), set()
    kinds = set()  # (view key, kind) of contributed views with a non-base kind and no fallback (D29)
    prefixes = set()  # ext.command("agents." + name): registered names known only by prefix
    seen = set()

    def visit(tree):
        for node in ast.walk(tree):
            if isinstance(node, ast.Constant) and isinstance(node.value, str):
                strings.add(node.value)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.args:
                first = node.args[0]
                if node.func.attr == "command" and isinstance(first, ast.BinOp) and isinstance(first.left, ast.Constant):
                    prefixes.add(first.left.value)
                if isinstance(first, ast.Constant) and isinstance(first.value, str):
                    attr, owner = node.func.attr, node.func.value
                    if attr == "get" and isinstance(owner, ast.Attribute) and owner.attr == "live":
                        live.add(first.value)
                    if attr == "command":
                        registered.add(first.value)
                    if attr == "publish":
                        published.add(first.value)
            if isinstance(node, ast.Dict):
                for k, v in zip(node.keys, node.values):
                    if isinstance(k, ast.Constant) and isinstance(k.value, str) and k.value.startswith("view/") and isinstance(v, ast.Dict):
                        fields = {f.value: x for f, x in zip(v.keys, v.values) if isinstance(f, ast.Constant)}
                        kind = fields.get("kind")
                        if isinstance(kind, ast.Constant) and kind.value not in ("panel", "terminal") and "fallback" not in fields:
                            kinds.add((k.value, kind.value))
                    if isinstance(k, ast.Constant) and k.value == "command" and isinstance(v, ast.Constant) and isinstance(v.value, str):
                        refs.add(v.value)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "invoke" and node.args:
                if isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str):
                    refs.add(node.args[0].value)
            if isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute) and node.value.attr == "live":
                if isinstance(node.slice, ast.Constant) and isinstance(node.slice.value, str):
                    live.add(node.slice.value)

    def load(path, only=None):
        key = (path, frozenset(only or ()))
        if key in seen:
            return
        seen.add(key)
        tree = ast.parse(read(path), path)
        if only is None:
            visit(tree)
        else:
            for node in tree.body:
                if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name in only:
                    visit(node)
        for helper, names in helper_functions(tree):
            load(helper, names)

    for f in files:
        load(f)
    return strings, live, registered, published, prefixes, refs, kinds


def main(argv=None):
    global EXT
    argv = sys.argv[1:] if argv is None else argv
    verbose = "--verbose" in argv
    if "--extensions" in argv:
        EXT = argv[argv.index("--extensions") + 1]
    problems = []
    defaults = {e["name"]: e for e in json.loads(read(os.path.join(EXT, "defaults.json")))["extensions"]}
    manifests = {}
    for f in sorted(os.listdir(os.path.join(EXT, "manifests"))):
        m = json.loads(read(os.path.join(EXT, "manifests", f)))
        manifests[m["name"]] = m
    for name in sorted(set(defaults) - set(manifests)):
        problems.append(f"{name}: bundled default has no manifest")
    provided = {}  # cap key -> [extension]
    for name, m in manifests.items():
        for field in ("provides", "needs", "works_with"):
            for cap in entries(m, field):
                if not isinstance(cap, str) or not CAP.match(cap):
                    problems.append(f"{name}: invalid capability in {field}: {cap!r}")
        for cap in entries(m, "provides"):
            if isinstance(cap, str):
                provided.setdefault(cap_key(cap), []).append(name)

    def owner_of(cap):
        if cap in CORE_DOCS or any(family.endswith("*") and cap.startswith(family[:-1]) for family in CORE_DOCS) or cap.startswith(("command/vqro.",) + CORE_DOC_PREFIXES):
            return ["vqro"]
        if cap in provided:
            return provided[cap]
        for family, owners in provided.items():
            if family.endswith("*") and cap.startswith(family[:-1]):
                return owners
        return []

    scans = {}
    for name in sorted(manifests):
        files = sources(name, (defaults.get(name) or {}).get("argv") or [])
        if files:  # native programs (providers): manifest syntax only
            scans[name] = scan(files)
    # Commands registered as `ext.command("agents." + name)` are known only by prefix: a reference inside such a
    # family is a use of that extension's command, which its manifest must provide.
    families = {prefix: owner for owner, sc in scans.items() for prefix in sc[4] if prefix.endswith(".")}

    for name, m in sorted(manifests.items()):
        if name not in scans:
            continue
        strings, live, registered, published, prefixes, refs, kinds = scans[name]
        for key, kind in sorted(kinds):
            problems.append(f"{name}: {key} has kind {kind}, which base clients can't render, and no fallback view")
        own = {cap_key(c) for c in entries(m, "provides") if isinstance(c, str)}
        declared = {cap_key(c) for f in ("needs", "works_with") for c in entries(m, f) if isinstance(c, str)}
        uses = set()
        for s in strings:
            if "/" not in s and ("command/" + s) in provided:
                uses.add("command/" + s)
            elif s.startswith(("doc/", "view/")) and len(s) > 5:
                uses.add(s)
            elif s in ABILITY_METHODS:
                uses.add(ABILITY_METHODS[s])
        uses |= {"data/" + x for x in live}
        for ref in sorted(refs):
            family = next((owner for prefix, owner in families.items() if ref.startswith(prefix) and COMMAND.match(ref)), None)
            if family is None:
                continue
            if "command/" + ref not in provided:
                problems.append(f"{name}: uses command {ref} of {family}'s registered family, which {family}'s manifest does not provide")
            elif family != name:
                uses.add("command/" + ref)
        for cmd in sorted(registered):
            if "command/" + cmd not in own:
                problems.append(f"{name}: registers command {cmd} but does not list command/{cmd} in provides")
        for cap in sorted(c for c in own if c.startswith("command/")):
            if cap[8:] not in registered and not cap[8:].startswith(tuple(prefixes)):
                problems.append(f"{name}: provides {cap} but never registers it")
        for view in sorted(published):
            if not ({"data/" + view, "view/" + view} & own) and not any(o.endswith("*") and ("view/" + view).startswith(o[:-1]) for o in own):
                problems.append(f"{name}: publishes {view} but provides neither data/{view} nor view/{view}")
        for cap in sorted(uses):
            if cap in own or any(o.endswith("*") and cap.startswith(o[:-1]) for o in own):
                continue
            owners = owner_of(cap)
            if owners == ["vqro"] or name in owners:
                continue
            if not owners:
                if cap.startswith(("doc/", "view/")):
                    problems.append(f"{name}: uses {cap}, which no manifest provides (declare it in provides if {name} owns it)")
                elif cap.startswith("data/"):
                    problems.append(f"{name}: reads live data {cap[5:]}, which no manifest provides")
                continue
            if cap not in declared and not any(d.endswith("*") and cap.startswith(d[:-1]) for d in declared):
                problems.append(f"{name}: uses {cap} (provided by {', '.join(owners)}) without declaring it in needs or works_with")
        if verbose:
            print(f"{name}: uses {sorted(uses - own)}")
    for p in problems:
        print(p)
    if not problems:
        print(f"manifest dependencies: {len(manifests)} manifests, every cross-extension use declared")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
