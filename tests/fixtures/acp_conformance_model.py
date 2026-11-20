"""P6 research model, not an ACP adapter or production vqro implementation.

The codec names typed neutral operations/fields; the reducer exercises the hard
lifecycle distinctions. No runtime processes, endpoints, models or third-party SDKs.
"""
from copy import deepcopy
from dataclasses import dataclass, field
import re


class Rejected(ValueError):
    """Definitely no new effect; research-model validation only."""


class Unsupported(Rejected):
    pass


OPERATIONS = {
    "initialize": "protocol.negotiate", "authenticate": "auth.login", "auth/login": "auth.login",
    "logout": "auth.logout", "auth/logout": "auth.logout", "session/new": "session.create",
    "session/load": "session.restore_history", "session/resume": "session.resume",
    "session/list": "session.list", "session/close": "session.close", "session/delete": "session.delete",
    "session/prompt": "input.admit", "session/cancel": "work.interrupt",
    "session/set_mode": "config.select_mode", "session/set_config_option": "config.set",
    "tool_call/cancel": "object.cancel_tool", "terminal/cancel": "object.cancel_terminal",
    "session/request_permission": "interaction.permission", "session/update": "observation.update",
    "fs/read_text_file": "service.file.read", "fs/write_text_file": "service.file.write",
    "terminal/create": "service.terminal.create", "terminal/output": "service.terminal.output",
    "terminal/release": "service.terminal.release", "terminal/wait_for_exit": "service.terminal.wait",
    "terminal/kill": "service.terminal.stop", "elicitation/create": "interaction.elicit",
    "elicitation/complete": "interaction.complete", "$/cancel_request": "operation.cancel",
}

# Arbitrary business data belongs in a standard typed field, not a protocol blob.
# Everything else must be a field in the pinned schema, or an explicit _meta bag.
DATA_FIELDS = {"rawInput", "rawOutput", "requestedSchema", "content", "_meta", "annotations"}


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


class Codec:
    """Schema-backed field projection, reversible without a native payload escape."""

    def __init__(self, schemas):
        fields = set()

        def visit(value):
            if isinstance(value, dict):
                fields.update(value.get("properties", {}))
                for child in value.values():
                    visit(child)
            elif isinstance(value, list):
                for child in value:
                    visit(child)
        for schema in schemas.values():
            visit(schema)
        self.update_tags = {}
        for version, schema in schemas.items():
            self.update_tags[int(version[-1])] = set()
            for branch in schema["$defs"]["SessionUpdate"].get("anyOf", schema["$defs"]["SessionUpdate"].get("oneOf", [])):
                tag = branch.get("properties", {}).get("sessionUpdate", {}).get("const")
                if tag is not None:
                    self.update_tags[int(version[-1])].add(tag)
        self.forward = {name: ("extensions" if name == "_meta" else snake(name)) for name in fields}
        self.backward = {value: key for key, value in self.forward.items()}
        if len(self.backward) != len(self.forward):
            raise AssertionError("neutral field-name collision")

    def fields(self, value, reverse=False, arbitrary=False):
        if arbitrary:
            return deepcopy(value)
        if isinstance(value, list):
            return [self.fields(child, reverse) for child in value]
        if not isinstance(value, dict):
            return value
        mapping = self.backward if reverse else self.forward
        result = {}
        for key, child in value.items():
            if key not in mapping:
                raise Unsupported(f"unmapped standard field: {key}")
            out = mapping[key]
            wire_key = out if reverse else key
            # Content recurses through typed blocks, except form answers, whose
            # user-defined property names are ordinary result data.
            raw = wire_key in DATA_FIELDS - {"content"}
            if wire_key == "content" and not isinstance(child, list):
                raw = not (isinstance(child, dict) and "type" in child)
            result[out] = self.fields(child, reverse, arbitrary=raw)
        return result

    def decode(self, version, method, params):
        if method not in OPERATIONS:
            raise Unsupported(method)
        if params.get("mcpServers"):
            raise Unsupported("no MCP: explicit program decision")
        if method == "session/update" and params.get("update", {}).get("sessionUpdate") not in self.update_tags[version]:
            if version != 2:
                raise Unsupported("v1 closed update union")
            return {"operation": OPERATIONS[method], "arguments": {
                "session_id": params["sessionId"], "future_update": deepcopy(params["update"])}}
        return {"operation": OPERATIONS[method], "arguments": self.fields(params)}

    def encode(self, version, method, neutral):
        if neutral["operation"] != OPERATIONS[method]:
            raise Rejected("wrong neutral operation")
        # Only explicitly unrecognized future variants may carry a preserved raw
        # value. No currently required standard field takes this escape hatch.
        if "future_update" in neutral["arguments"]:
            arguments = neutral["arguments"]
            return {"jsonrpc": "2.0", "method": method, "params": {
                "sessionId": arguments["session_id"], "update": deepcopy(arguments["future_update"])}}
        # The selected endpoint's version determines the native method spelling.
        return {"jsonrpc": "2.0", "method": method,
                "params": self.fields(neutral["arguments"], reverse=True)}


@dataclass
class Session:
    parent: str | None = None
    child_cancel: bool = False
    active: bool = True
    foreground: str = "idle"
    background: list | None = None
    entities: dict = field(default_factory=dict)
    keys: dict = field(default_factory=dict)
    pending: dict = field(default_factory=dict)
    requests: dict = field(default_factory=dict)
    completions: list = field(default_factory=list)
    epoch: int = 1
    sequence: int = 0
    replaying: bool = False
    replay_complete: bool = False
    retained: dict = field(default_factory=dict)
    staged: dict = field(default_factory=dict)
    live_after_replay: list = field(default_factory=list)
    waiting_on: set = field(default_factory=set)


class Model:
    """Bounded logical admission + typed entities + ACP facade lifecycle oracle."""

    def __init__(self, max_pending=128, max_bytes=32768):
        self.sessions = {}
        self.operations = {}
        self.provider_replies = {}
        self.facade_replies = {}
        self.native_writes = []
        self.native_answers = []
        self.native_controls = []
        self.notices = []
        self.max_pending = max_pending
        self.max_bytes = max_bytes
        self.clock = 0
        self._ids = 0

    @staticmethod
    def negotiate(requested, supported, dialect=2):
        if isinstance(requested, bool) or not isinstance(requested, int) or requested < 0:
            raise Rejected("-32602: malformed protocol version")
        if dialect == 1:
            return requested if requested in supported else max(supported)
        choices = [version for version in supported if version <= requested]
        if not choices:
            raise Rejected("-32022: unsupported protocol version")
        return max(choices)

    @staticmethod
    def supports(capabilities, path):
        value = capabilities
        for part in path.split("."):
            if not isinstance(value, dict) or part not in value:
                return False
            value = value[part]
        return isinstance(value, dict) or value is True

    def new(self, name, parent=None, child_cancel=False):
        if name in self.sessions:
            raise Rejected("identity cannot be reused")
        if parent is not None and parent not in self.sessions:
            raise Rejected("child must be announced under a known parent")
        self.sessions[name] = Session(parent, child_cancel)

    def allowed(self, session, operation):
        state = self.sessions[session]
        if not state.active:
            raise Rejected("inactive session")
        if state.parent is not None:
            if operation not in ("observe", "answer") and not (operation == "interrupt" and state.child_cancel):
                raise Rejected("runtime-restricted child: workspace grants cannot add native capabilities")

    def prompt(self, version, session, operation, content, key=None, insert=False):
        self.allowed(session, "send")
        if version == 1 and key is not None:
            raise Unsupported("v1 does not define insertion keys")
        state = self.sessions[session]
        if operation in self.operations:
            raise Rejected("an operation id is not a fresh invocation")
        if len(content.encode()) > self.max_bytes or len(state.pending) >= self.max_pending:
            raise Rejected("bounded admission")
        if key is not None and (session, key) in state.keys:
            original = state.keys[(session, key)]
            if original["content"] != content:
                raise Rejected("same native key with different content")
            self.operations[operation] = original
            if original["stage"] == "inserted":
                self.facade_replies[operation] = {"messageId": original["message"]}
            else:
                original["waiters"].append(operation)
            self.provider_replies[operation] = {"outcome": "dispatched", "operation": operation}
            return
        item = {"session": session, "content": content, "stage": "dispatched", "version": version,
                "waiters": [operation], "message": None, "key": key}
        self.operations[operation] = item
        state.pending[operation] = item
        if key is not None:
            state.keys[(session, key)] = item
        self.native_writes.append((session, content))
        # Generic provider request returns promptly even if native ACP RPC is held.
        self.provider_replies[operation] = {"outcome": "dispatched", "operation": operation}
        if insert:
            self.insert(operation)

    def advance(self, seconds):
        self.clock += seconds
        # Native ACP held requests are not generic provider pending requests.
        # A read timeout is not negative input-delivery evidence.

    def insert(self, operation):
        item = self.operations[operation]
        if item["stage"] == "inserted":
            return
        self._ids += 1
        item["message"] = f"message-{self._ids}"
        item["stage"] = "inserted"
        state = self.sessions[item["session"]]
        state.pending.pop(item["waiters"][0], None)
        self.put(item["session"], "user_message", item["message"], {"content": item["content"]})
        if item["version"] == 2:
            for waiter in item["waiters"]:
                self.facade_replies[waiter] = {"messageId": item["message"]}

    def complete(self, session, reason="end_turn", operation=None):
        state = self.sessions[session]
        state.foreground = "idle"
        state.completions.append({"completion": len(state.completions) + 1, "reason": reason})
        if operation is not None and self.operations[operation]["version"] == 1:
            self.facade_replies[operation] = {"stopReason": reason}

    def _cancel_pending(self, session):
        state = self.sessions[session]
        for operation, item in list(state.pending.items()):
            item["stage"] = "cancelled_before_insertion"
            for waiter in item["waiters"]:
                self.facade_replies[waiter] = {"error": -32800}
            if item["key"] is not None:
                state.keys.pop((session, item["key"]), None)
            del state.pending[operation]

    def interrupt(self, session):
        self.allowed(session, "interrupt")
        self._stop_foreground(session)

    def _stop_foreground(self, session):
        state = self.sessions[session]
        self._cancel_pending(session)
        for request in state.requests.values():
            if request["status"] == "pending" and not ({"background", "operation"} & request["scope"].keys()):
                request["status"] = "cancelled"
        if state.foreground in ("running", "requires_action"):
            self.complete(session, "cancelled")
        for child in state.waiting_on:
            self._stop_foreground(child)
        # Listed background and unrelated interactions continue. Idle cancellation
        # does not fabricate a new foreground completion.

    def cancel_object(self, session, kind, entity_id):
        state = self.sessions[session]
        value = state.entities.get((kind, entity_id), {})
        if not state.active or not self.supports(value.get("capabilities", {}), "cancel"):
            raise Rejected("object does not advertise cancel")
        self.native_controls.append((session, kind, entity_id, "cancel"))
        # Native acceptance is not observed object termination.
        return {"outcome": "accepted"}

    def close(self, session):
        self.allowed(session, "close")
        for name in list(self.sessions):
            ancestor = self.sessions[name].parent
            while ancestor is not None and ancestor != session:
                ancestor = self.sessions[ancestor].parent
            if name == session or ancestor == session:
                state = self.sessions[name]
                self._cancel_pending(name)
                state.active = False
                state.foreground = "idle"
                state.background = []
                for request in state.requests.values():
                    request["status"] = "cancelled"

    def prune_retained_message(self, session, key):
        # ACP v2 keys last as long as the retained message. No independent TTL.
        # Explicit native retention loss is still not an authorization to retry.
        state = self.sessions[session]
        item = state.keys.pop((session, key), None)
        if item is not None:
            state.entities.pop(("user_message", item["message"]), None)

    def lookup(self, operation):
        item = self.operations.get(operation)
        return "unknown" if item is None else item["stage"]

    def request(self, session, request, method, params, revision=1, scope=None):
        self.allowed(session, "observe")
        state = self.sessions[session]
        if request in state.requests:
            raise Rejected("standard request identity cannot be reused in this epoch")
        state.requests[request] = {"epoch": state.epoch, "revision": revision, "method": method,
                                   "parameters": deepcopy(params), "scope": scope or {"session": session},
                                   "status": "pending", "consented": False, "completed": False}

    def answer(self, session, request, epoch, revision, result):
        self.allowed(session, "answer")
        value = self.sessions[session].requests[request]
        if value["status"] != "pending" or (epoch, revision) != (value["epoch"], value["revision"]):
            raise Rejected("stale/competing answer")
        result = deepcopy(result)
        if value["method"] in ("form", "url"):
            if result.get("action") not in ("accept", "decline", "cancel"):
                result = {"action": "cancel"}
            elif result["action"] != "accept":
                result.pop("content", None)
        if value["method"] == "permission" and "choice" in result:
            if result["choice"] not in value["parameters"].get("choices", []):
                raise Rejected("not an advertised permission choice")
        if value["method"] == "form" and result.get("action") == "accept":
            schema = value["parameters"].get("schema", {})
            content = result.get("content", {})
            if not self.valid_form(schema, content):
                raise Rejected("form does not satisfy the presented schema")
        value["status"] = "answered"
        value["response"] = deepcopy(result)
        self.native_answers.append((session, request, deepcopy(result)))
        value["consented"] = value["method"] == "url" and result.get("action") == "accept"

    @staticmethod
    def valid_form(schema, value):
        kind = schema.get("type")
        valid_types = {"object": isinstance(value, dict), "string": isinstance(value, str),
                       "integer": isinstance(value, int) and not isinstance(value, bool),
                       "number": isinstance(value, (int, float)) and not isinstance(value, bool),
                       "boolean": isinstance(value, bool), "array": isinstance(value, list)}
        if kind is not None and not valid_types.get(kind, False):
            return False
        if "enum" in schema and value not in schema["enum"]:
            return False
        if "const" in schema and value != schema["const"]:
            return False
        if "oneOf" in schema and sum(Model.valid_form(child, value) for child in schema["oneOf"]) != 1:
            return False
        if "anyOf" in schema and not any(Model.valid_form(child, value) for child in schema["anyOf"]):
            return False
        if isinstance(value, dict):
            if any(key not in value for key in schema.get("required", [])):
                return False
            return all(Model.valid_form(child, value[key]) for key, child in schema.get("properties", {}).items() if key in value)
        if isinstance(value, list):
            return len(value) >= schema.get("minItems", 0) and len(value) <= schema.get("maxItems", len(value)) and all(
                Model.valid_form(schema.get("items", {}), child) for child in value)
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            return schema.get("minimum", value) <= value <= schema.get("maximum", value)
        return True

    def external_complete(self, session, request):
        value = self.sessions[session].requests[request]
        if value["method"] != "url" or not value["consented"]:
            raise Rejected("unrelated external completion")
        value["completed"] = True

    def put(self, session, kind, entity_id, changes):
        self.allowed(session, "observe")
        state = self.sessions[session]
        key = (kind, entity_id)  # kind is essential even when native IDs collide.
        target = state.staged if state.replaying else state.entities
        old = target.get(key, {"position": len(target), "revision": 0})
        value = deepcopy(old)
        for name, data in changes.items():
            if name == "metadata":
                if data is None:
                    value.pop(name, None)
                else:
                    metadata = value.setdefault(name, {})
                    for meta_key, meta_value in data.items():
                        if meta_value is None:
                            metadata.pop(meta_key, None)
                        else:
                            metadata[meta_key] = deepcopy(meta_value)
            else:
                # Whole field replacement (including null); omitted fields stay.
                value[name] = deepcopy(data)
        value["revision"] += 1
        target[key] = value
        state.sequence += 1

    def append(self, session, kind, entity_id, field_name, text):
        state = self.sessions[session]
        target = state.staged if state.replaying else state.entities
        if (kind, entity_id) not in target:
            # ACP permits first chunk before full update. Adapter seeds a typed
            # empty entity before emitting neutral revision-checked append.
            self.put(session, kind, entity_id, {field_name: ""})
        current = target[(kind, entity_id)]
        self.put(session, kind, entity_id, {field_name: current.get(field_name, "") + text})

    def start_replay(self, session):
        state = self.sessions[session]
        if state.replaying:
            raise Rejected("overlapping authoritative reset")
        state.replaying = True
        state.replay_complete = False
        state.staged = {}

    def live_during_replay(self, session, kind, entity_id, changes):
        self.sessions[session].live_after_replay.append((kind, entity_id, changes))

    def replay_end(self, session):
        state = self.sessions[session]
        if not state.replaying:
            raise Rejected("end without begin")
        state.entities = state.staged
        state.staged = {}
        state.replaying = False
        state.replay_complete = True
        for kind, entity_id, changes in state.live_after_replay:
            self.put(session, kind, entity_id, changes)
        state.live_after_replay.clear()

    def replay_abort(self, session):
        state = self.sessions[session]
        state.replaying = False
        state.replay_complete = False
        state.staged = {}
        state.live_after_replay.clear()

    def notice(self, session, text):
        self.notices.append((session, text))

    def background(self, session, items):
        self.sessions[session].background = deepcopy(items)

    def quiet(self, session):
        state = self.sessions[session]
        return state.foreground == "idle" and state.background == []

    def reconnect(self):
        # Report state, never replay original inputs. An absent receipt remains unknown.
        return {name: {"foreground": state.foreground, "background": deepcopy(state.background)}
                for name, state in self.sessions.items()}

# ACP_CONFORMANCE_VOCABULARY: generated from P6 a726ede; above bytes are unmodified.
P6_MODEL_SHA256 = '661df1e8e754e2608719a4eda80ea2cb6761390abf8820beb38013968ad1d5a9'
SCHEMA_SHA256 = {'1': 'e747a7bec3c02708b70885bf3cccac5c72fa421390dd7e85758fd61e33b1ae21', '2': '67558dadf78ae97e961c6e9cdef73ee77d9ada03bc7e54688936a3e01d858f85'}
SCHEMA_VOCABULARY = {'v1': {'properties': {'_meta': {}, 'action': {}, 'additionalDirectories': {}, 'additionalProperties': {}, 'agentCapabilities': {}, 'agentInfo': {}, 'amount': {}, 'annotations': {}, 'anyOf': {}, 'args': {}, 'audience': {}, 'audio': {}, 'auth': {}, 'authMethods': {}, 'availableCommands': {}, 'availableModes': {}, 'blob': {}, 'boolean': {}, 'category': {}, 'clientCapabilities': {}, 'clientInfo': {}, 'close': {}, 'code': {}, 'command': {}, 'compaction': {}, 'compactionId': {}, 'configId': {}, 'configOptions': {}, 'const': {}, 'content': {}, 'cost': {}, 'currency': {}, 'currentModeId': {}, 'currentValue': {}, 'cursor': {}, 'cwd': {}, 'data': {}, 'default': {}, 'delete': {}, 'description': {}, 'elicitation': {}, 'elicitationId': {}, 'embeddedContext': {}, 'entries': {}, 'enum': {}, 'env': {}, 'error': {}, 'exitCode': {}, 'exitStatus': {}, 'form': {}, 'format': {}, 'fs': {}, 'group': {}, 'headers': {}, 'hint': {}, 'http': {}, 'id': {}, 'image': {}, 'input': {}, 'items': {}, 'jsonrpc': {}, 'kind': {}, 'lastModified': {}, 'limit': {}, 'line': {}, 'list': {}, 'loadSession': {}, 'locations': {}, 'logout': {}, 'maxItems': {}, 'maxLength': {}, 'maximum': {}, 'mcpCapabilities': {}, 'mcpServers': {}, 'message': {}, 'messageId': {}, 'method': {}, 'methodId': {}, 'mimeType': {}, 'minItems': {}, 'minLength': {}, 'minimum': {}, 'mode': {}, 'modeId': {}, 'modes': {}, 'name': {}, 'newText': {}, 'nextCursor': {}, 'notices': {}, 'oldText': {}, 'oneOf': {}, 'optionId': {}, 'options': {}, 'outcome': {}, 'output': {}, 'outputByteLimit': {}, 'override': {}, 'params': {}, 'path': {}, 'pattern': {}, 'priority': {}, 'prompt': {}, 'promptCapabilities': {}, 'properties': {}, 'protocolVersion': {}, 'rawInput': {}, 'rawOutput': {}, 'readTextFile': {}, 'requestId': {}, 'requestedSchema': {}, 'required': {}, 'resource': {}, 'result': {}, 'resume': {}, 'session': {}, 'sessionCapabilities': {}, 'sessionId': {}, 'sessionUpdate': {}, 'sessions': {}, 'severity': {}, 'signal': {}, 'size': {}, 'sse': {}, 'status': {}, 'stopReason': {}, 'summary': {}, 'systemPrompt': {}, 'terminal': {}, 'terminalId': {}, 'text': {}, 'title': {}, 'toolCall': {}, 'toolCallId': {}, 'truncated': {}, 'type': {}, 'update': {}, 'updatedAt': {}, 'uri': {}, 'url': {}, 'used': {}, 'value': {}, 'version': {}, 'writeTextFile': {}}, '$defs': {'SessionUpdate': {'anyOf': [{'properties': {'sessionUpdate': {'const': 'agent_message_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'agent_thought_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'available_commands_update'}}}, {'properties': {'sessionUpdate': {'const': 'compaction_summary_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'compaction_update'}}}, {'properties': {'sessionUpdate': {'const': 'config_option_update'}}}, {'properties': {'sessionUpdate': {'const': 'current_mode_update'}}}, {'properties': {'sessionUpdate': {'const': 'notice'}}}, {'properties': {'sessionUpdate': {'const': 'plan'}}}, {'properties': {'sessionUpdate': {'const': 'session_info_update'}}}, {'properties': {'sessionUpdate': {'const': 'tool_call'}}}, {'properties': {'sessionUpdate': {'const': 'tool_call_update'}}}, {'properties': {'sessionUpdate': {'const': 'usage_update'}}}, {'properties': {'sessionUpdate': {'const': 'user_message_chunk'}}}]}}}, 'v2': {'properties': {'_meta': {}, 'action': {}, 'additionalDirectories': {}, 'additionalProperties': {}, 'amount': {}, 'annotations': {}, 'anyOf': {}, 'args': {}, 'audience': {}, 'audio': {}, 'auth': {}, 'authMethods': {}, 'availableCommands': {}, 'background': {}, 'blob': {}, 'cancel': {}, 'capabilities': {}, 'category': {}, 'changes': {}, 'code': {}, 'cols': {}, 'command': {}, 'compactionId': {}, 'configId': {}, 'configOptions': {}, 'const': {}, 'content': {}, 'cost': {}, 'currency': {}, 'currentValue': {}, 'cursor': {}, 'cwd': {}, 'data': {}, 'default': {}, 'delete': {}, 'description': {}, 'elicitation': {}, 'elicitationId': {}, 'embeddedContext': {}, 'entries': {}, 'enum': {}, 'env': {}, 'error': {}, 'exitCode': {}, 'exitStatus': {}, 'fileType': {}, 'form': {}, 'format': {}, 'groupId': {}, 'headers': {}, 'hint': {}, 'http': {}, 'icons': {}, 'id': {}, 'idempotencyKey': {}, 'image': {}, 'info': {}, 'input': {}, 'items': {}, 'jsonrpc': {}, 'kind': {}, 'lastModified': {}, 'line': {}, 'locations': {}, 'maxItems': {}, 'maxLength': {}, 'maximum': {}, 'mcp': {}, 'mcpServers': {}, 'message': {}, 'messageId': {}, 'method': {}, 'methodId': {}, 'mimeType': {}, 'minItems': {}, 'minLength': {}, 'minimum': {}, 'mode': {}, 'name': {}, 'nextCursor': {}, 'oldPath': {}, 'oneOf': {}, 'operation': {}, 'optionId': {}, 'options': {}, 'outcome': {}, 'output': {}, 'override': {}, 'params': {}, 'patch': {}, 'path': {}, 'plan': {}, 'planId': {}, 'priority': {}, 'prompt': {}, 'properties': {}, 'protocolVersion': {}, 'rawInput': {}, 'rawOutput': {}, 'recipientMessageId': {}, 'recipientSessionId': {}, 'replayFrom': {}, 'requestId': {}, 'requested': {}, 'requestedSchema': {}, 'required': {}, 'resource': {}, 'result': {}, 'rows': {}, 'senderMessageId': {}, 'senderSessionId': {}, 'session': {}, 'sessionId': {}, 'sessionUpdate': {}, 'sessions': {}, 'severity': {}, 'signal': {}, 'size': {}, 'sizes': {}, 'src': {}, 'state': {}, 'status': {}, 'stdio': {}, 'stopReason': {}, 'subject': {}, 'summary': {}, 'supported': {}, 'systemPrompt': {}, 'terminal': {}, 'terminalId': {}, 'text': {}, 'theme': {}, 'title': {}, 'toolCall': {}, 'toolCallId': {}, 'type': {}, 'update': {}, 'updatedAt': {}, 'uri': {}, 'url': {}, 'used': {}, 'value': {}, 'version': {}}, '$defs': {'SessionUpdate': {'anyOf': [{'properties': {'sessionUpdate': {'const': 'agent_message'}}}, {'properties': {'sessionUpdate': {'const': 'agent_message_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'agent_thought'}}}, {'properties': {'sessionUpdate': {'const': 'agent_thought_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'available_commands_update'}}}, {'properties': {'sessionUpdate': {'const': 'compaction_summary_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'compaction_update'}}}, {'properties': {'sessionUpdate': {'const': 'config_option_update'}}}, {'properties': {'sessionUpdate': {'const': 'notice'}}}, {'properties': {'sessionUpdate': {'const': 'plan_update'}}}, {'properties': {'sessionUpdate': {'const': 'replay_complete'}}}, {'properties': {'sessionUpdate': {'const': 'session_info_update'}}}, {'properties': {'sessionUpdate': {'const': 'session_message'}}}, {'properties': {'sessionUpdate': {'const': 'session_message_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'state_update'}}}, {'properties': {'sessionUpdate': {'const': 'subagent_update'}}}, {'properties': {'sessionUpdate': {'const': 'terminal_output_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'terminal_update'}}}, {'properties': {'sessionUpdate': {'const': 'tool_call_content_chunk'}}}, {'properties': {'sessionUpdate': {'const': 'tool_call_update'}}}, {'properties': {'sessionUpdate': {'const': 'usage_update'}}}, {'properties': {'sessionUpdate': {'const': 'user_message'}}}, {'properties': {'sessionUpdate': {'const': 'user_message_chunk'}}}]}}}}
