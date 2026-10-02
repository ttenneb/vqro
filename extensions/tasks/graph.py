"""Bounded text/timing checks and completion-DAG validation; no host or I/O policy."""
import datetime

META_FIELDS = {"done_when", "brief_path", "estimate_work_ms", "estimate_wait_ms", "actual_work_ms", "actual_wait_ms", "due_at"}
MILLISECONDS = META_FIELDS - {"done_when", "brief_path", "due_at"}
MAX_PLAN_NODES = 100
MAX_PLAN_EDGES = 400


def metadata(payload, required=False):
    result = {key: payload[key] for key in META_FIELDS if key in payload}
    if required and "done_when" not in result:
        raise ValueError("done_when is required on every new task")
    if "done_when" in result:
        value = result["done_when"]
        if not isinstance(value, str) or len(value.encode()) > 1024:
            raise ValueError("done_when must be at most 1024 UTF-8 bytes")
        lines = [line.rstrip() for line in value.rstrip().split("\n")]
        value = "\n".join(lines)
        if not 1 <= len(lines) <= 3 or any(not line.strip() or any(not char.isprintable() for char in line) for line in lines) or len(set(lines)) != len(lines):
            raise ValueError("done_when requires 1-3 distinct nonempty observable lines")
        result["done_when"] = value
    if "brief_path" in result:
        value = result["brief_path"]
        if value is not None and (not isinstance(value, str) or not value.strip() or len(value.encode()) > 512 or any(not char.isprintable() for char in value)):
            raise ValueError("brief_path must be one nonempty line, at most 512 bytes")
    for key in MILLISECONDS & result.keys():
        if type(result[key]) is not int or not 0 <= result[key] <= (1 << 53) - 1:
            raise ValueError(f"{key} must be a nonnegative integer of milliseconds")
    if "due_at" in result and result["due_at"] is not None:
        value = result["due_at"]
        if not isinstance(value, str) or len(value) > 64:
            raise ValueError("due_at must be an ISO-8601 time with timezone")
        try:
            parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
            if parsed.tzinfo is None:
                raise ValueError("timezone missing")
            result["due_at"] = parsed.astimezone(datetime.timezone.utc).isoformat(timespec="milliseconds")
        except (ValueError, OverflowError) as error:
            raise ValueError("due_at must be an ISO-8601 time with timezone") from error
    return result


def evidence(criteria, text):
    if not isinstance(text, str) or not text.strip() or len(text.encode()) > 4096:
        raise ValueError("evidence must be nonempty and at most 4096 bytes including quotes")
    if criteria is None:
        raise ValueError("legacy task needs done_when before reporting; update its active metadata")
    expected = {"> " + line for line in criteria.split("\n")}
    seen, current, has_evidence = set(), None, False
    for line in text.split("\n"):
        line = line.rstrip()
        if line.startswith(">"):
            if line not in expected or line in seen or (current is not None and not has_evidence):
                raise ValueError("quote each done_when line exactly once with evidence below it")
            seen.add(line)
            current, has_evidence = line, False
        elif line.strip() and current is not None:
            has_evidence = True
    if seen != expected or not has_evidence:
        raise ValueError("quote each done_when line exactly once with evidence below it")
    return text


def dependencies(value):
    if not isinstance(value, list) or len(value) > MAX_PLAN_EDGES or any(not isinstance(item, str) or not 1 <= len(item) <= 128 for item in value) or len(set(value)) != len(value):
        raise ValueError("depends_on must be a distinct list of at most 400 task references")
    return value


def check_cycles(parents, dependencies):
    # Precedence direction: prerequisite -> dependent; a child must finish before its parent.
    # Parent edges constrain completion cycles, not readiness of a parent.
    outgoing = {task: set() for task in parents}
    incoming = dict.fromkeys(parents, 0)
    edges = [(child, parent) for child, parent in parents.items() if parent is not None]
    edges.extend((prerequisite, dependent) for dependent, prerequisite in dependencies)
    for source, target in edges:
        if source not in parents or target not in parents:
            raise ValueError("graph reference not found")
        if target not in outgoing[source]:
            outgoing[source].add(target)
            incoming[target] += 1
    ready = [task for task, count in incoming.items() if count == 0]
    visited = 0
    while ready:
        source = ready.pop()
        visited += 1
        for target in outgoing[source]:
            incoming[target] -= 1
            if incoming[target] == 0:
                ready.append(target)
    if visited != len(parents):
        raise ValueError("dependency or parent-completion cycle rejected")
