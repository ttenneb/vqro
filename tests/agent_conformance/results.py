"""Complete case inventory and honest, machine-readable conformance results."""
from dataclasses import asdict, dataclass, field
import json

from .inventory import main_contract


class CheckFailed(AssertionError):
    def __init__(self, message, *, component="adapter"):
        self.component = component
        super().__init__(message)


class HarnessGap(CheckFailed):
    def __init__(self, message):
        super().__init__(message, component="harness")


class FixtureFailed(CheckFailed):
    def __init__(self, message):
        super().__init__(message, component="fixture")


class NotApplicable(Exception):
    """Declaration-selected exclusion, never a missing fixture or handler."""
    def __init__(self, path, actual):
        self.path, self.actual = path, actual
        super().__init__(f"case excluded by declaration: {path} ({actual!r})")


class NotDeclared(Exception):
    """Only an explicit missing optional declaration, never an execution error."""
    def __init__(self, path, actual=None):
        self.path, self.actual = path, actual
        super().__init__(f"optional capability not declared: {path} ({actual!r})")


def check(condition, message, *, component="adapter"):
    if not condition:
        raise CheckFailed(message, component=component)


@dataclass(frozen=True)
class Case:
    id: str
    title: str
    operation: str | None = None
    module: str | None = None


# All normative Part A groups, including unimplemented ones. Their absence from
# the handler map is fail/harness, never a successful or unsupported module.
CASES = (
    Case("C1", "registration and declarations"),
    Case("C2", "resource restrictions and invalid preconditions"),
    Case("C3", "evidenced state and completion transitions"),
    Case("C4", "lost reply and no replay", "agent.send"),
    Case("C5", "failure after acceptance", "agent.send"),
    Case("C6", "bounded facts declare incomplete fields"),
    Case("C7", "core restart and fact resync"),
    Case("C8", "literal text and controlled launch", "agent.send"),
    Case("C9", "qualified causality", "agent.send"),
    Case("C10", "endpoint, session and shared host model"),
    Case("C11", "native optional usage and session facts"),
    Case("C12", "generic bounded causality wire forms", "agent.send"),
    Case("D1", "start at idle and busy", "agent.send"),
    Case("D2", "admit at idle and busy", "agent.send"),
    Case("D3", "boundary at idle and busy", "agent.send"),
    Case("D4", "settle at idle and busy", "agent.send"),
    Case("D5", "native preempt at idle and busy", "agent.send"),
    Case("D6", "undeclared content rejected before native write", "agent.send"),
    Case("D7", "native admission versus correlated insertion", "agent.send"),
    Case("D8", "native key scope and retention", "agent.send"),
    Case("D9", "consume-time batch settings", "agent.send"),
    Case("D10", "interrupt scope, pending input and wait", "agent.interrupt"),
    Case("D11", "guards and concurrent controllers", "agent.send"),
    Case("D12", "idle park never starts work", "agent.send"),
    Case("D13", "serialized start preflight", "agent.send"),
    Case("D14", "hook-consumed input never overclaimed", "agent.send"),
    Case("D15", "origin and model-request authority", "agent.send"),
    Case("D16", "declared content shape preserves block order", "agent.send"),
    Case("D17", "core-bound principal actor initiator and asserted origin", "agent.send"),
    Case("O1", "put append remove algebra"),
    Case("O2", "serialized snapshot cut"),
    Case("O3", "snapshot follow-on overflow is per viewer"),
    Case("O4", "foreign and expired cursor reset"),
    Case("O5", "open stream session-switch epoch fence"),
    Case("O6", "timeline order across viewers"),
    Case("O7", "one stalled viewer does not block others"),
    Case("O8", "byte framing and split UTF-8"),
    Case("O9", "coverage and pruning preserve live state"),
    Case("O10", "declared retained memory bounds"),
    Case("O11", "retention gaps and connection-wide closure"),
    Case("O12", "producer append refusal preserves entity revision and sequence"),
    Case("O13", "native view metadata formats roles and evidence times"),
    Case("O14", "incompatible viewer backoff preserves last-good cursor"),
    Case("R1", "requests in facts and stream with scope", "agent.respond"),
    Case("R2", "first valid answer wins; stale answers reject", "agent.respond"),
    Case("R3", "answer acknowledgement precision", "agent.respond"),
    Case("R4", "unrepresentable requests cancel once", "agent.respond"),
    Case("R5", "cancel is not denial false or empty", "agent.respond"),
    Case("R6", "background and foreground request withdrawal", "agent.respond"),
    Case("R7", "request reconnect and operation scope", "agent.respond"),
    Case("M1", "native queues and per-item capabilities", "agent.queue"),
    Case("M2", "sessionless new switch resume and fork", "agent.session"),
    Case("M3", "observed configuration and caps revisions", "agent.configure"),
    Case("M4", "advertised explicit commands", "agent.command"),
    Case("M5", "frozen history pages to live cursor", "agent.history"),
    Case("M6", "child capabilities and scoped object cancel", module="children"),
    Case("M7", "display terminal bytes and exit", module="terminals"),
    Case("M8", "byte-safe chunk append and snapshot continuation", module="terminals"),
    Case("L1", "interrupt is not endpoint stop", "agent.interrupt"),
    Case("L2", "stop coverage and end evidence", "agent.stop"),
    Case("L3", "declared host lifecycle and incarnation"),
    Case("L4", "disconnect and irreversible end evidence"),
)
BY_ID = {case.id: case for case in CASES}


def select_cases(names=None, *, inventory=CASES):
    if not names:
        return list(inventory)
    selected = set(names)
    known = {case.id for case in inventory}
    unknown = selected - known
    if unknown:
        raise ValueError(f"unknown conformance cases: {sorted(unknown)}")
    return [case for case in inventory if case.id in selected]


def declared(caps, operation):
    if operation not in caps:
        raise NotDeclared(operation)
    value = caps[operation]
    if not isinstance(value, dict):
        raise CheckFailed(f"{operation} declaration is not an object")
    return value


def declared_module(caps, name):
    modules = caps.get("agent", {}).get("modules", {})
    check(isinstance(modules, dict), "agent.modules is not the canonical descriptor map")
    if name not in modules:
        raise NotDeclared("agent.modules." + name)
    value = modules[name]
    check(isinstance(value, dict) and type(value.get("v")) is int and value["v"] == 0,
          "optional module lacks its v0 descriptor")
    return value


@dataclass
class Result:
    case: str
    title: str
    status: str
    component: str | None = None
    detail: str = ""
    evidence: list = field(default_factory=list)
    declarations: dict = field(default_factory=dict)
    duration_ms: int = 0
    fixture_root: str | None = None
    reason: str | None = None

    def __post_init__(self):
        if self.status not in ("pass", "fail", "unsupported-declared", "not-applicable"):
            raise ValueError("unknown conformance result status")
        if self.status == "unsupported-declared" and not self.declarations:
            raise ValueError("unsupported-declared needs the omitted capability declaration")
        if self.status == "not-applicable":
            exclusion = self.declarations.get("exclusion")
            if not isinstance(exclusion, dict) or not exclusion.get("path") or "actual" not in exclusion:
                raise ValueError("not-applicable needs its explicit excluding declaration")

    def json(self):
        return asdict(self)


def report(implementation, results, selected, *, part="A", inventory=CASES, contract=None):
    if part not in ("A", "B"):
        raise ValueError("conformance part must be A or B")
    catalog = [case.id for case in inventory]
    contract = (contract or main_contract()) if part == "A" else None
    expected = contract.ids if contract is not None else catalog
    inventory_matches = len(catalog) == len(set(catalog)) and set(catalog) == set(expected)
    executed = [row.case for row in results]
    selected_ids = [case.id for case in selected]
    verified = contract.metadata()["main_verified"] if contract is not None else True
    complete = verified and inventory_matches and selected_ids == expected and executed == expected
    failed = any(row.status == "fail" for row in results)
    # Fixtures and partial selections are never complete adapter certification.
    return {"schema": "vqro-conformance-result-v0", "part": part, "implementation": implementation,
            "complete_inventory": complete, "selected": selected_ids, "normative_cases": expected,
            "normative_contract": contract.metadata() if contract is not None else None,
            "inventory_missing": [ident for ident in expected if ident not in catalog],
            "inventory_extra": [ident for ident in catalog if ident not in expected],
            "not_selected": [ident for ident in expected if ident not in selected_ids],
            "fixture_native": True, "real_runtime_certification": False,
            "summary": {status: sum(row.status == status for row in results)
                        for status in ("pass", "fail", "unsupported-declared", "not-applicable")},
            "passed_selection": inventory_matches and any(row.status == "pass" for row in results) and not failed and executed == selected_ids,
            "results": [row.json() for row in results]}


def markdown(document):
    clean = lambda value: str(value).replace("|", "\\|").replace("\n", " ")
    rows = [f"# Agent conformance (Part {document['part']})", "", "Fixtures only; not real-runtime certification.",
            f"Complete inventory executed: {document['complete_inventory']}. Missing selection: "
            + (", ".join(document["not_selected"]) or "none") + ".", "",
            "| Case | Result | Component | Detail |", "|---|---|---|---|"]
    for result in document["results"]:
        rows.append("| " + " | ".join(clean(result[key] or "—") for key in ("case", "status", "component", "detail")) + " |")
    rows += ["", "Implementation / version / limits / provenance:", "", "```json",
             json.dumps(document["implementation"], indent=2, ensure_ascii=False), "```", ""]
    return "\n".join(rows)
