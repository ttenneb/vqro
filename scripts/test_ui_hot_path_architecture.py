from __future__ import annotations

import re
import unittest
from collections import Counter
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parent.parent
ALL_SRC_SOURCES = tuple(sorted((PROJECT_ROOT / "src").rglob("*.rs")))
HOT_PATH_SOURCES = (
    PROJECT_ROOT / "src" / "ui.rs",
    *sorted((PROJECT_ROOT / "src" / "ui").rglob("*.rs")),
    PROJECT_ROOT / "src" / "server" / "render_stream.rs",
)
APP_SERVER_SOURCES = (
    *sorted((PROJECT_ROOT / "src" / "app").rglob("*.rs")),
    *sorted((PROJECT_ROOT / "src" / "server").rglob("*.rs")),
)
RECOVERY_MODEL_SOURCE = PROJECT_ROOT / "src" / "client" / "shell" / "recovery_browser.rs"
EXTENSION_STATE_ACTOR_SOURCE = PROJECT_ROOT / "src" / "app" / "api" / "extension_state.rs"
COMPOSITE_HOST_READ_REFERENCE = re.compile(
    r"\b(?:HostReadInvocationAuthorization|HostReadInvocationPurpose|"
    r"stage_host_read_authorization|invoke_with_host_reads)\b"
)
COMPOSITE_HOST_READ_MODULE = PROJECT_ROOT / "src" / "extension_host_invocation.rs"
DOCUMENT_SHADOW_AUDIT_SOURCE = PROJECT_ROOT / "src" / "app" / "api" / "document_shadow_audit.rs"
COLLECTIONS_CONFORMANCE_SOURCE = PROJECT_ROOT / "src" / "extension_runtime" / "collections_package_conformance.rs"
COMPOSITE_HOST_READ_APP_MODULE = (
    PROJECT_ROOT / "src" / "app" / "api" / "extension_invocation.rs"
)
# Composite host reads are production authority only for the exact-Git Collections package.
# Keep this list explicit: adding another package or generic host path must update the reviewed
# architecture boundary rather than silently broadening it.
COLLECTIONS_COMPOSITE_HOST_READ_SOURCES = frozenset(
    {
        PROJECT_ROOT / "src" / "app" / "api" / "collections_authority.rs",
        PROJECT_ROOT / "src" / "app" / "api" / "collections_candidate.rs",
        PROJECT_ROOT / "src" / "app" / "api" / "collections_create.rs",
        PROJECT_ROOT / "src" / "extension_candidate.rs",
        PROJECT_ROOT / "src" / "extension_service.rs",
    }
)
COMPOSITE_HOST_READ_BOUNDARY_SOURCES = frozenset(
    {
        PROJECT_ROOT / "src" / "main.rs",
        PROJECT_ROOT / "src" / "app" / "api.rs",
        PROJECT_ROOT / "src" / "extension_runtime" / "mod.rs",
        COMPOSITE_HOST_READ_MODULE,
        COMPOSITE_HOST_READ_APP_MODULE,
        PROJECT_ROOT / "src" / "app" / "api" / "document_shadow_audit.rs",
        COLLECTIONS_CONFORMANCE_SOURCE,
        *COLLECTIONS_COMPOSITE_HOST_READ_SOURCES,
    }
)
DOCUMENT_V2_SOURCES = (
    PROJECT_ROOT / "src" / "host_document_v2.rs",
    PROJECT_ROOT / "src" / "quarantined_audit_cache.rs",
)
DOCUMENT_V2_PATH_ROOT = re.compile(
    r"(?<![A-Za-z0-9_:])(?:::)?\b(?!use\b)([A-Za-z_][A-Za-z0-9_]*)\s*::(?!\s*<)"
)
LOCAL_ROOT_IMPORT = re.compile(r"\buse\s+(?:crate|super|self)\b")
CRATE_ROOT_ALIAS = re.compile(
    r"\bextern\s+crate\s+self\s+as\s+[A-Za-z_][A-Za-z0-9_]*"
    r"|\buse\s+crate\s+as\s+[A-Za-z_][A-Za-z0-9_]*"
    r"|\buse\s+crate\s*::\s*\{\s*self\s+as\s+[A-Za-z_][A-Za-z0-9_]*"
)
DOCUMENT_V2_REFERENCE = re.compile(
    r"\b(?:host_document_v2|quarantined_audit_cache|HostDocumentV2|DocumentNodeV2|"
    r"DocumentProducerFenceV2|DocumentScopeV2|DocumentDependencyV2|"
    r"QuarantinedAuditCache|AuditCacheKey|AuditTicket|document_shadow_audit|"
    r"DocumentShadowAuditCoordinator)\b"
)
DOCUMENT_V2_INTERNAL_PATHS = frozenset(
    (*DOCUMENT_V2_SOURCES, DOCUMENT_SHADOW_AUDIT_SOURCE, COLLECTIONS_CONFORMANCE_SOURCE)
)
# Document v2 remains unavailable to generic extensions. These three files are the complete,
# reviewed production bridge for Collections profile auditing and atomic labeled creation.
COLLECTIONS_DOCUMENT_V2_AUTHORITY_SOURCES = frozenset(
    {
        PROJECT_ROOT / "src" / "collections_extension_contract.rs",
        PROJECT_ROOT / "src" / "app" / "api" / "collections_candidate.rs",
        PROJECT_ROOT / "src" / "app" / "api" / "collections_create.rs",
    }
)
DOCUMENT_V2_DECLARATION_SOURCE = PROJECT_ROOT / "src" / "main.rs"
DOCUMENT_V2_SCHEMA_SOURCE = PROJECT_ROOT / "src" / "extension_spec.rs"
DOCUMENT_V2_ALLOWED_PATH_ROOTS = {
    PROJECT_ROOT / "src" / "host_document_v2.rs": frozenset(
        {
            "CappedSerializationError",
            "DocumentNodeV2",
            "HashMap",
            "HashSet",
            "Self",
            "Sha256",
            "TerminalId",
            "TerminalScopePolicy",
            "char",
            "schemars",
            "serde",
            "serde_json",
            "sha2",
            "std",
        }
    ),
    PROJECT_ROOT / "src" / "quarantined_audit_cache.rs": frozenset(
        {
            "AuditCacheError",
            "BTreeMap",
            "CappedSerializationError",
            "Self",
            "serde",
            "std",
        }
    ),
}
DOCUMENT_V2_ALLOWED_IMPORTS = {
    PROJECT_ROOT / "src" / "host_document_v2.rs": (
        "use crate::terminal::TerminalId;",
        "use sha2::{Digest, Sha256};",
    ),
    PROJECT_ROOT / "src" / "quarantined_audit_cache.rs": (
        "use crate::host_document_v2::CappedSerializationError;",
        "use crate::host_document_v2::DocumentDependencyV2;",
        "use crate::host_document_v2::DocumentProducerFenceV2;",
        "use crate::host_document_v2::DocumentProducerV2;",
        "use crate::host_document_v2::DocumentScopeV2;",
        "use crate::host_document_v2::serialized_len_capped;",
        "use crate::host_document_v2::validate_dependencies;",
        "use crate::host_document_v2::validate_producer_fence;",
        "use crate::host_document_v2::validate_scope;",
    ),
}
DOCUMENT_V2_ALLOWED_DECLARATION_LINES = Counter(
    {
        "mod host_document_v2;": 1,
        "pub mod quarantined_audit_cache;": 1,
    }
)
# These are compared after comments and string literals have been masked.
DOCUMENT_V2_ALLOWED_SCHEMA_LINES = Counter(
    [
        "pub const HOST_DOCUMENT_SCHEMA_ID_V2: &str = crate::host_document_v2::HOST_DOCUMENT_SCHEMA_ID_V2;",
        "crate::host_document_v2::HOST_DOCUMENT_RENDER_SCHEMA_ID_V2;",
        "crate::host_document_v2::HostDocumentV2",
        "crate::host_document_v2::HostDocumentRenderParamsV2",
        "serde_json::Value::String(crate::host_document_v2::HOST_DOCUMENT_RENDER_CONTRACT_V2.into()),",
        ": crate::host_document_v2::MAX_DOCUMENT_RENDER_V2_BYTES,",
        ": crate::host_document_v2::HOST_DOCUMENT_RENDER_CONTRACT_V2,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
        "serde_json::Value::String(crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2.into()),",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_BYTES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_NODES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_DEPTH,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_DEPENDENCIES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_TEXT_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_TEXT_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_TOTAL_TEXT_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS,",
        ": crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2,",
        "serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_DEPENDENCIES);",
        "serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);",
        "serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);",
        "serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);",
        "serde_json::json!(crate::host_document_v2::MAX_DOCUMENT_V2_NODES);",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
        ": crate::host_document_v2::MAX_DOCUMENT_V2_IDENTIFIER_BYTES,",
    ]
)
RECOVERY_FORBIDDEN_REFERENCES = (
    ("agents", re.compile(r"\.agents\b")),
    ("collections", re.compile(r"\.collections\b")),
    ("delegations", re.compile(r"\.delegations\b")),
    ("host_documents", re.compile(r"\bhost_documents\b")),
    ("structure", re.compile(r"\bstructure\b")),
    ("ExtensionRuntimeDispatcher", re.compile(r"\bExtensionRuntimeDispatcher\b")),
    ("ExtensionServiceSupervisor", re.compile(r"\bExtensionServiceSupervisor\b")),
)
CFG_ATTRIBUTE_START = re.compile(r"#(!?)\[\s*cfg\s*\(")
INPUT_STATE_CALL = re.compile(r"(?:\.|::)input_state\b")
KEYBOARD_STATE_ANSI_CALL = re.compile(
    r"(?:\.|::)(?:keyboard_state_ansi|kitty_keyboard_state_ansi)\b"
)
AGGREGATE_STATE_CALLS = (
    (INPUT_STATE_CALL, "aggregate terminal input state; add a narrow accessor"),
    (KEYBOARD_STATE_ANSI_CALL, "formatted keyboard state"),
)
FORBIDDEN_CALLS = (
    *AGGREGATE_STATE_CALLS,
    (
        re.compile(r"(?:\.|::)screen_text_snapshot\b"),
        "formatted terminal screen snapshot",
    ),
    (
        re.compile(r"\bforeground_job\s*\("),
        "process-tree inspection",
    ),
    (
        re.compile(
            r"\b(?:ExtensionRuntimeDispatcher|ExtensionServiceSupervisor)\b"
            r"|(?:\.|::)request_cancellable\s*\("
        ),
        "extension RPC from a render or frame-fanout hot path",
    ),
)


def blank_non_newlines(chars: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if chars[index] != "\n":
            chars[index] = " "


def mask_comments_and_literals(source: str) -> str:
    chars = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth > 0:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank_non_newlines(chars, index, end)
            index = end
            continue

        if source[index] == "r":
            quote = index + 1
            while quote < len(source) and source[quote] == "#":
                quote += 1
            if quote < len(source) and source[quote] == '"':
                suffix = '"' + "#" * (quote - index - 1)
                end = source.find(suffix, quote + 1)
                end = len(source) if end == -1 else end + len(suffix)
                blank_non_newlines(chars, index, end)
                index = end
                continue

        if source[index] == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            blank_non_newlines(chars, index, min(end, len(source)))
            index = end
            continue

        if source[index] == "'":
            end = index + 2
            if index + 1 < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                end += 1
                blank_non_newlines(chars, index, end)
                index = end
                continue

        index += 1

    return "".join(chars)


def _cfg_attributes(code: str):
    for match in CFG_ATTRIBUTE_START.finditer(code):
        depth = 1
        index = match.end()
        while index < len(code) and depth:
            if code[index] == "(": depth += 1
            elif code[index] == ")": depth -= 1
            index += 1
        if depth or index >= len(code) or code[index] != "]":
            continue
        yield match, index + 1, code[match.end():index - 1]


def _cfg_possible_without_test(expression: str) -> set[bool]:
    tokens = re.findall(r"[A-Za-z_][A-Za-z0-9_]*|[(),]", expression)
    position = 0

    def parse() -> set[bool]:
        nonlocal position
        if position >= len(tokens):
            return {False, True}
        name = tokens[position]
        position += 1
        if position >= len(tokens) or tokens[position] != "(":
            return {False} if name == "test" else {False, True}
        position += 1
        args = []
        while position < len(tokens) and tokens[position] != ")":
            args.append(parse())
            if position < len(tokens) and tokens[position] == ",":
                position += 1
        if position < len(tokens):
            position += 1
        if name == "all":
            values = {True}
            for arg in args:
                values = {left and right for left in values for right in arg}
            return values
        if name == "any":
            values = {False}
            for arg in args:
                values = {left or right for left in values for right in arg}
            return values
        if name == "not" and len(args) == 1:
            return {not value for value in args[0]}
        return {False, True}

    return parse()


def production_code(source: str) -> str:
    """Mask comments/literals and every item or field directly guarded by cfg(test).

    This intentionally handles semicolon modules, block items, struct fields, and enum
    variants rather than recognizing only the narrow `mod tests {}` spelling.
    """
    code = mask_comments_and_literals(source)
    chars = list(code)
    attributes = list(_cfg_attributes(code))
    if any(match.group(1) == "!" and True not in _cfg_possible_without_test(expr)
           for match, _end, expr in attributes):
        blank_non_newlines(chars, 0, len(code))
        return "".join(chars)
    search_from = 0
    while True:
        attribute = next((item for item in _cfg_attributes(code)
                          if item[0].start() >= search_from
                          and item[0].group(1) != "!"
                          and True not in _cfg_possible_without_test(item[2])), None)
        if attribute is None:
            break
        match, attribute_end, _expression = attribute
        index = attribute_end
        while index < len(code) and code[index].isspace():
            index += 1
        paren = bracket = angle = 0
        end = index
        while end < len(code):
            char = code[end]
            if char == "(": paren += 1
            elif char == ")": paren = max(0, paren - 1)
            elif char == "[": bracket += 1
            elif char == "]": bracket = max(0, bracket - 1)
            elif char == "<" and paren == bracket == 0: angle += 1
            elif char == ">" and paren == bracket == 0: angle = max(0, angle - 1)
            elif char == "{" and paren == bracket == angle == 0:
                depth = 1
                end += 1
                while end < len(code) and depth:
                    if code[end] == "{": depth += 1
                    elif code[end] == "}": depth -= 1
                    end += 1
                while end < len(code) and code[end].isspace() and code[end] != "\n":
                    end += 1
                if end < len(code) and code[end] in ";,": end += 1
                break
            elif char in ";," and paren == bracket == angle == 0:
                end += 1
                break
            end += 1
        blank_non_newlines(chars, match.start(), end)
        code = "".join(chars)
        search_from = end
    return code


def recovery_forbidden_references(source: str) -> list[str]:
    code = production_code(source)
    return [name for name, pattern in RECOVERY_FORBIDDEN_REFERENCES if pattern.search(code)]


def find_violations(paths, rules) -> list[str]:
    violations: list[str] = []
    for path in paths:
        code = production_code(path.read_text(encoding="utf-8"))
        for pattern, description in rules:
            for match in pattern.finditer(code):
                line = code.count("\n", 0, match.start()) + 1
                relative_path = path.relative_to(PROJECT_ROOT)
                violations.append(f"{relative_path}:{line}: {description}")
    return violations


def _reference_lines(code: str) -> Counter[str]:
    return Counter(
        line.strip()
        for line in code.splitlines()
        if DOCUMENT_V2_REFERENCE.search(line)
    )


def _counter_mismatches(
    path: Path, actual: Counter[str], expected: Counter[str], description: str
) -> list[str]:
    violations: list[str] = []
    for line, count in (actual - expected).items():
        violations.extend(f"{path.name}: {description}: {line}" for _ in range(count))
    for line, count in (expected - actual).items():
        violations.extend(
            f"{path.name}: missing expected {description}: {line}" for _ in range(count)
        )
    return violations


def document_v2_wiring_violations_for_sources(
    sources: list[tuple[Path, str]],
) -> list[str]:
    violations: list[str] = []
    for path, source in sources:
        if path in DOCUMENT_V2_INTERNAL_PATHS or path in COLLECTIONS_DOCUMENT_V2_AUTHORITY_SOURCES:
            continue
        code = production_code(source)
        actual = _reference_lines(code)
        if path == DOCUMENT_V2_DECLARATION_SOURCE:
            violations.extend(
                _counter_mismatches(
                    path,
                    actual,
                    DOCUMENT_V2_ALLOWED_DECLARATION_LINES,
                    "document-v2 declaration",
                )
            )
            continue
        if path == DOCUMENT_V2_SCHEMA_SOURCE:
            violations.extend(
                _counter_mismatches(
                    path,
                    actual,
                    DOCUMENT_V2_ALLOWED_SCHEMA_LINES,
                    "document-v2 schema reference",
                )
            )
            continue
        for line, count in actual.items():
            violations.extend(
                f"{path.name}: document-v2 runtime wiring: {line}" for _ in range(count)
            )
    return violations


def document_v2_wiring_violations(paths) -> list[str]:
    return document_v2_wiring_violations_for_sources(
        [(path, path.read_text(encoding="utf-8")) for path in paths]
    )


def crate_root_alias_violations_for_sources(
    sources: list[tuple[Path, str]],
) -> list[str]:
    violations: list[str] = []
    for path, source in sources:
        code = mask_comments_and_literals(source)
        for match in CRATE_ROOT_ALIAS.finditer(code):
            line_number = code.count("\n", 0, match.start()) + 1
            violations.append(f"{path.name}:{line_number}: crate-root alias declaration")
    return violations


def crate_root_alias_violations(paths) -> list[str]:
    return crate_root_alias_violations_for_sources(
        [(path, path.read_text(encoding="utf-8")) for path in paths]
    )


def document_v2_isolation_violations_for_sources(
    sources: list[tuple[Path, str]],
) -> list[str]:
    violations = crate_root_alias_violations_for_sources(sources)
    for path, source in sources:
        if path in DOCUMENT_V2_INTERNAL_PATHS:
            violations.extend(document_v2_dependency_violations(source, path))
    return violations


def document_v2_dependency_violations(source: str, path: Path) -> list[str]:
    code = production_code(source)
    violations: list[str] = []
    chars = list(code)
    for allowed in DOCUMENT_V2_ALLOWED_IMPORTS.get(path, ()):
        starts = [
            match.start()
            for match in re.finditer(re.escape(allowed), code)
        ]
        if len(starts) != 1:
            violations.append(
                f"{path.name}: expected exactly one whitelisted import: {allowed}"
            )
            if not starts:
                continue
        blank_non_newlines(chars, starts[0], starts[0] + len(allowed))
    code = "".join(chars)
    allowed_roots = DOCUMENT_V2_ALLOWED_PATH_ROOTS.get(path, frozenset())
    for match in DOCUMENT_V2_PATH_ROOT.finditer(code):
        root = match.group(1)
        if root in allowed_roots:
            continue
        line_number = code.count("\n", 0, match.start()) + 1
        line = code.splitlines()[line_number - 1].strip()
        violations.append(
            f"{path.name}:{line_number}: non-whitelisted path root {root}: {line}"
        )
    for pattern, description in (
        (LOCAL_ROOT_IMPORT, "non-whitelisted local-root import"),
        (re.compile(r"\bextern\s+crate\b"), "extern-crate dependency"),
    ):
        for match in pattern.finditer(code):
            line_number = code.count("\n", 0, match.start()) + 1
            violations.append(f"{path.name}:{line_number}: {description}")
    return violations


def rust_function_body(source: str, function_name: str) -> str:
    code = production_code(source)
    match = re.search(rf"\bfn\s+{re.escape(function_name)}\s*\(", code)
    if match is None:
        raise AssertionError(f"missing Rust function {function_name}")
    start = code.find("{", match.end())
    if start == -1:
        raise AssertionError(f"missing body for Rust function {function_name}")
    depth = 0
    for index in range(start, len(code)):
        if code[index] == "{":
            depth += 1
        elif code[index] == "}":
            depth -= 1
            if depth == 0:
                return code[start : index + 1]
    raise AssertionError(f"unterminated body for Rust function {function_name}")


class UiHotPathArchitectureTests(unittest.TestCase):
    def test_no_crate_root_alias_declarations_anywhere_in_src(self) -> None:
        self.assertEqual(crate_root_alias_violations(ALL_SRC_SOURCES), [])

    def test_crate_root_alias_scanner_rejects_equivalent_forms(self) -> None:
        synthetic = PROJECT_ROOT / "src" / "synthetic_alias.rs"
        for source in (
            "extern crate self as root;",
            "use crate as root;",
            "pub(crate) use crate as root;",
            "use crate::{self as root};",
        ):
            with self.subTest(source=source):
                self.assertTrue(
                    crate_root_alias_violations_for_sources([(synthetic, source)])
                )

    def test_document_v2_modules_have_only_approved_roots_and_imports(self) -> None:
        violations: list[str] = []
        for path in DOCUMENT_V2_SOURCES:
            violations.extend(
                document_v2_dependency_violations(path.read_text(encoding="utf-8"), path)
            )
        self.assertEqual(violations, [])

    def test_document_shadow_audit_is_test_only_and_finalizer_is_synchronous(self) -> None:
        app_api = (PROJECT_ROOT / "src" / "app" / "api.rs").read_text(encoding="utf-8")
        self.assertRegex(app_api, r"#\[cfg\(test\)\]\s*pub\(crate\) mod document_shadow_audit;")
        runtime = (PROJECT_ROOT / "src" / "extension_runtime" / "mod.rs").read_text(encoding="utf-8")
        self.assertRegex(runtime, r"#\[cfg\(test\)\]\s*mod collections_package_conformance;")
        source = DOCUMENT_SHADOW_AUDIT_SOURCE.read_text(encoding="utf-8")
        body = rust_function_body(source, "finalize")
        for forbidden in (".await", "spawn(", "std::fs", "Command::", "invoke(", "callback"):
            self.assertNotIn(forbidden, body)

    def test_production_code_masks_all_cfg_test_item_shapes(self) -> None:
        source = """
#[cfg(test)]
mod named;
struct S {
    #[cfg(test)]
    field: Pair<String, String>,
    live: usize,
}
#[cfg(all(test, unix))]
fn helper() { forbidden::call(); }
#[cfg(any(test))]
fn equivalent() { also_forbidden::call(); }
#[cfg(any(test, unix))]
fn not_test_only() { remains::call(); }
fn live() { allowed::call(); }
"""
        code = production_code(source)
        self.assertNotIn("named", code)
        self.assertNotIn("field", code)
        self.assertNotIn("forbidden", code)
        self.assertNotIn("also_forbidden", code)
        self.assertIn("remains::call", code)
        self.assertIn("allowed::call", code)
        self.assertNotIn("hidden", production_code("#![cfg(test)]\nfn hidden() {}"))
        self.assertNotIn(
            "hidden", production_code("#![cfg(all(test, unix))]\nfn hidden() {}")
        )

    def test_document_shadow_scanner_rejects_aliases_and_reexports(self) -> None:
        synthetic = PROJECT_ROOT / "src" / "synthetic_shadow.rs"
        for source in (
            "pub use crate::app::api::document_shadow_audit as audit;",
            "use crate::app::api as hidden; fn leak() { hidden::document_shadow_audit::leak(); }",
            "pub use crate::host_document_v2::HostDocumentV2 as ShadowValue;",
        ):
            with self.subTest(source=source):
                self.assertTrue(document_v2_wiring_violations_for_sources([(synthetic, source)]))

    def test_document_v2_runtime_wiring_is_confined_to_collections_authority(self) -> None:
        violations = document_v2_wiring_violations(ALL_SRC_SOURCES)
        self.assertEqual(
            violations,
            [],
            "Only schema/declaration modules and the explicit Collections authority bridge may "
            "reference document-v2/cache symbols:\n" + "\n".join(violations),
        )
        for path in COLLECTIONS_DOCUMENT_V2_AUTHORITY_SOURCES:
            code = production_code(path.read_text(encoding="utf-8"))
            self.assertRegex(
                code,
                DOCUMENT_V2_REFERENCE,
                f"stale Collections document-v2 allowlist entry: {path}",
            )

    def test_document_v2_dependency_scanner_denies_indirection(self) -> None:
        contract_path, cache_path = DOCUMENT_V2_SOURCES
        cases = (
            (contract_path, "use crate::{terminal::TerminalId};"),
            (contract_path, "use super::terminal::TerminalId;"),
            (contract_path, "use crate as root;"),
            (contract_path, "use crate::terminal::TerminalId as StableTerminalId;"),
            (contract_path, "pub use crate::terminal::TerminalId;"),
            (contract_path, "extern crate self as root;"),
            (
                cache_path,
                "use crate::host_document_v2::{DocumentScopeV2 as Scope};",
            ),
            (cache_path, "use self::private_alias::DocumentScopeV2;"),
            (contract_path, "use arbitrary_root::TerminalId;"),
            (contract_path, "use ::arbitrary_root::TerminalId;"),
        )
        for path, addition in cases:
            with self.subTest(path=path.name, addition=addition):
                source = path.read_text(encoding="utf-8") + "\n" + addition + "\n"
                self.assertTrue(document_v2_dependency_violations(source, path))

    def test_document_v2_dependency_scanner_rejects_compiling_cross_file_alias(self) -> None:
        contract_path = DOCUMENT_V2_SOURCES[0]
        sources = [
            (
                DOCUMENT_V2_DECLARATION_SOURCE,
                DOCUMENT_V2_DECLARATION_SOURCE.read_text(encoding="utf-8")
                + "\npub(crate) use terminal as t;\n",
            ),
            (
                contract_path,
                contract_path.read_text(encoding="utf-8")
                + "\nuse t::TerminalId;\n",
            ),
        ]
        violations = document_v2_isolation_violations_for_sources(sources)
        self.assertTrue(any("path root t" in violation for violation in violations))

    def test_document_v2_dependency_scanner_allows_approved_external_roots(self) -> None:
        contract_path = DOCUMENT_V2_SOURCES[0]
        base = contract_path.read_text(encoding="utf-8")
        controls = (
            "use ::std::mem;",
            "use ::serde_json::Value;",
            "fn control() { let _ = ::std::mem::size_of::<u8>(); }",
            "fn control() { let _ = ::serde_json::Value::Null; }",
            "fn control() { let _ = std::mem::size_of::<u8>(); }",
            "fn control() { let _ = serde_json::Value::Null; }",
        )
        for control in controls:
            with self.subTest(control=control):
                source = base + "\n" + control + "\n"
                self.assertEqual(
                    document_v2_dependency_violations(source, contract_path), []
                )

    def test_document_v2_activation_scanner_denies_extension_spec_additions(self) -> None:
        source = DOCUMENT_V2_SCHEMA_SOURCE.read_text(encoding="utf-8")
        additions = (
            "fn activate(_: crate::host_document_v2::HostDocumentV2) {}",
            "use crate::{host_document_v2 as producer_docs};",
            "use super::host_document_v2::HostDocumentV2;",
            "use crate as root; use root::host_document_v2::HostDocumentV2;",
            "use crate::host_document_v2::HostDocumentV2 as RuntimeDocument;",
            "use crate::quarantined_audit_cache::QuarantinedAuditCache;",
        )
        for addition in additions:
            with self.subTest(addition=addition):
                violations = document_v2_wiring_violations_for_sources(
                    [(DOCUMENT_V2_SCHEMA_SOURCE, source + "\n" + addition + "\n")]
                )
                self.assertTrue(violations)

    def test_composite_host_read_modules_and_dispatch_entry_are_production_fenced(self) -> None:
        main = production_code(
            (PROJECT_ROOT / "src" / "main.rs").read_text(encoding="utf-8")
        )
        app_api = production_code(
            (PROJECT_ROOT / "src" / "app" / "api.rs").read_text(encoding="utf-8")
        )
        runtime = production_code(
            (PROJECT_ROOT / "src" / "extension_runtime" / "mod.rs").read_text(
                encoding="utf-8"
            )
        )
        self.assertRegex(main, r"\bmod extension_host_invocation;")
        self.assertRegex(app_api, r"\bmod extension_invocation;")
        self.assertRegex(runtime, r"pub\(crate\) fn invoke_with_host_reads\(")
        self.assertRegex(
            runtime,
            r"Composite\(Box<crate::extension_host_invocation::",
        )
        self.assertRegex(
            runtime,
            r"Some\(\(HostInvocationAuthorization::Composite",
        )

    def test_composite_host_read_symbols_stay_inside_collections_boundary(self) -> None:
        violations = []
        for path in ALL_SRC_SOURCES:
            if path in COMPOSITE_HOST_READ_BOUNDARY_SOURCES:
                continue
            code = production_code(path.read_text(encoding="utf-8"))
            if COMPOSITE_HOST_READ_REFERENCE.search(code):
                violations.append(str(path.relative_to(PROJECT_ROOT)))
        self.assertEqual(
            violations,
            [],
            "Composite symbols must not escape the exact Collections authority boundary",
        )

    def test_extension_state_actor_handler_never_invokes_extension_runtime(self) -> None:
        body = rust_function_body(
            EXTENSION_STATE_ACTOR_SOURCE.read_text(encoding="utf-8"),
            "handle_extension_host_state_call",
        )
        forbidden = (
            "ExtensionRuntimeDispatcher",
            "ExtensionServiceSupervisor",
            "extension_services",
            "host_document_services",
            ".invoke(",
            ".request_cancellable(",
        )
        found = [token for token in forbidden if token in body]
        self.assertEqual(
            found,
            [],
            "The App host-state handler must only service actor requests; it must never "
            "call back into an extension runtime.",
        )

    def test_render_hot_paths_avoid_known_expensive_runtime_queries(self) -> None:
        violations = find_violations(HOT_PATH_SOURCES, FORBIDDEN_CALLS)

        self.assertEqual(
            violations,
            [],
            "Render/layout code must not perform pane-scaled expensive reads:\n"
            + "\n".join(violations),
        )

    def test_recovery_model_uses_only_cached_neutral_snapshot_facts(self) -> None:
        violations = recovery_forbidden_references(
            RECOVERY_MODEL_SOURCE.read_text(encoding="utf-8")
        )
        self.assertEqual(
            violations,
            [],
            "Recovery navigation must not depend on feature/provider state: "
            + ", ".join(violations),
        )

    def test_recovery_reference_scanner_uses_real_word_boundaries(self) -> None:
        self.assertEqual(
            recovery_forbidden_references(
                "fn projection() { let agents = provider.agents; }"
            ),
            ["agents"],
        )
        self.assertEqual(
            recovery_forbidden_references(
                "fn projection() { let agents_extra = 1; }"
            ),
            [],
        )

    def test_app_and_server_avoid_aggregate_terminal_state(self) -> None:
        self.assertTrue(APP_SERVER_SOURCES, "No app/server Rust sources were discovered")
        violations = find_violations(APP_SERVER_SOURCES, AGGREGATE_STATE_CALLS)

        self.assertEqual(
            violations,
            [],
            "App/server code must use narrow terminal-state accessors:\n"
            + "\n".join(violations),
        )

    def test_scanner_ignores_non_production_references(self) -> None:
        source = '''
// runtime.input_state()
const EXAMPLE: &str = "runtime.input_state()";
#[cfg(test)]
mod tests {
    fn aggregate_state_test() { runtime.input_state(); }
}
fn production_after_tests() {}
'''
        code = production_code(source)
        self.assertNotRegex(code, FORBIDDEN_CALLS[0][0])
        self.assertIn("fn production_after_tests()", code)
        self.assertEqual(code.count("\n"), source.count("\n"))

    def test_scanner_checks_production_after_test_modules(self) -> None:
        source = '''
#[cfg(test)]
mod tests {
    const BRACES: &str = "}}";
}
fn render() { TerminalRuntime::input_state; }
'''
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[0][0])

    def test_scanner_catches_each_aggregate_state_call(self) -> None:
        cases = (
            ("fn render() { runtime.input_state(); }", INPUT_STATE_CALL),
            ("fn render() { runtime.keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
            ("fn render() { runtime.kitty_keyboard_state_ansi(); }", KEYBOARD_STATE_ANSI_CALL),
        )
        for source, pattern in cases:
            with self.subTest(source=source):
                self.assertRegex(production_code(source), pattern)

    def test_scanner_catches_imported_process_query(self) -> None:
        source = "fn render() { foreground_job(pid); }"
        self.assertRegex(production_code(source), FORBIDDEN_CALLS[3][0])


if __name__ == "__main__":
    unittest.main()
