"""Pi config options and commands, agent standard v0 section 6.3 (`agent.configure`, `agent.command`).

Facts (republished whenever Pi's answers change; `data.agent.config` / `data.agent.commands`):
- config option: {id, label, description?, category?, type, value, choices?: [{value, label, description?}], rev}
- command: {name, description?, input?: {hint}}
`agent.configure {v, id, value, rev?}` and `agent.command {v, name, args?}` (args is a string). Anything stale or
outside these declarations is rejected before any native write. Pi's own slash commands (templates, skills,
extension commands) run only through `agent.command` for advertised names; plain sends never expand them.
"""
from vqro_display import label

BUILTIN = [
    {"name": "new_session", "description": "Start a fresh Pi session; the current conversation stays in its session file."},
    {"name": "compact", "description": "Summarise the conversation to free context (one model call).",
     "input": {"hint": "Custom instructions (optional)"}},
    {"name": "cycle_model", "description": "Switch to Pi's next available model."},
]
MODES = [{"value": "one-at-a-time", "label": "One at a time"}, {"value": "all", "label": "All at once"}]
MAX_CHOICES = 64
MAX_COMMANDS = 64


def model_value(model):
    return f"{model['provider']}/{model['id']}" if isinstance(model, dict) and model.get("provider") and model.get("id") else None


async def _data(pi, command, field):
    reply = await pi.request(command)
    value = (reply.get("data") or {}).get(field) if reply.get("outcome") == "accepted" and isinstance(reply.get("data"), dict) else None
    return value if isinstance(value, list) else None


async def read(pi):
    """(options without rev, Pi commands) from get_state and Pi's lists; options None when Pi did not
    answer get_state (never guessed). Pi commands None when unknown."""
    state = await pi.request("get_state")
    options = None
    if state.get("outcome") == "accepted" and isinstance(state.get("data"), dict):
        data, options = state["data"], []
        current = model_value(data.get("model"))
        models = await _data(pi, "get_available_models", "models")
        if models is not None:
            choices = [{"value": model_value(m), "label": label(m.get("name") or m.get("id"), 80)} for m in models if model_value(m)][:MAX_CHOICES]
            if current and not any(c["value"] == current for c in choices):
                choices.insert(0, {"value": current, "label": label(data["model"].get("name") or current, 80)})
            if choices:
                options.append({"id": "model", "label": "Model", "category": "model", "type": "select", "value": current, "choices": choices})
        levels = await _data(pi, "get_available_thinking_levels", "levels")
        if levels is not None and isinstance(data.get("thinkingLevel"), str):
            options.append({"id": "thinking_level", "label": "Thinking level", "category": "thought_level", "type": "select",
                            "value": data["thinkingLevel"],
                            "choices": [{"value": v, "label": label(v, 40)} for v in levels if isinstance(v, str)][:MAX_CHOICES]})
        for ident, title, field, what in (("steering_mode", "Steering delivery", "steeringMode", "steering messages"),
                                          ("follow_up_mode", "Follow-up delivery", "followUpMode", "follow-up messages")):
            if data.get(field) in [m["value"] for m in MODES]:
                options.append({"id": ident, "label": title, "category": "_queue", "type": "select", "value": data[field],
                                "description": f"How Pi delivers queued {what}; applies to queued input when consumed.",
                                "choices": MODES})
        if isinstance(data.get("autoCompactionEnabled"), bool):
            options.append({"id": "auto_compaction", "label": "Auto-compaction", "category": "_compaction", "type": "boolean",
                            "value": data["autoCompactionEnabled"]})
    native = await _data(pi, "get_commands", "commands")
    commands = None
    if native is not None:
        builtin = {c["name"] for c in BUILTIN}
        commands = [{"name": c["name"], **({"description": label(c["description"], 300)} if c.get("description") else {}),
                     "input": {"hint": "Arguments (optional)"}}
                    for c in native if isinstance(c, dict) and isinstance(c.get("name"), str) and c["name"]
                    and not any(ch.isspace() for ch in c["name"]) and c["name"] not in builtin][:MAX_COMMANDS]
    return options, commands


class Declarations:
    """Current facts with per-option `rev`, bumped whenever an option's value or choices change."""

    def __init__(self):
        self.options = None   # None = unknown
        self.native = None    # Pi's own (slash) commands; None = unknown
        self._revs = {}

    def update(self, options, native):
        if options is not None:
            for option in options:
                old = next((o for o in self.options or [] if o["id"] == option["id"]), None)
                changed = old is None or {k: v for k, v in old.items() if k != "rev"} != option
                self._revs[option["id"]] = self._revs.get(option["id"], 0) + (1 if changed else 0)
                option["rev"] = self._revs[option["id"]]
            self.options = options
        if native is not None:
            self.native = native

    def commands(self):
        return BUILTIN + (self.native or [])

    def facts(self):
        return {"v": 0, "config": self.options if self.options is not None else None, "commands": self.commands()}

    def configure(self, args):
        """(native command, args) or (error kind, message)."""
        if args.get("v", 0) != 0:
            return "unsupported", "agent.configure v must be 0"
        option = next((o for o in self.options or [] if o["id"] == args.get("id")), None)
        if option is None:
            return "unsupported", f"option {args.get('id')!r} is not declared by this agent"
        if "rev" in args and args["rev"] != option["rev"]:
            return "stale", f"option {option['id']} is at rev {option['rev']}, not {args['rev']}; nothing sent"
        value = args.get("value")
        if option["type"] == "boolean" and not isinstance(value, bool):
            return "unsupported", "value must be true or false"
        if option["type"] == "select" and not any(c["value"] == value for c in option.get("choices", [])):
            return "unsupported", f"value is not one of the declared choices for {option['id']}"
        if option["id"] == "model":
            provider, _, model = value.partition("/")
            return "set_model", {"provider": provider, "modelId": model}
        if option["id"] == "thinking_level":
            return "set_thinking_level", {"level": value}
        if option["id"] in ("steering_mode", "follow_up_mode"):
            return ("set_steering_mode" if option["id"] == "steering_mode" else "set_follow_up_mode"), {"mode": value}
        return "set_auto_compaction", {"enabled": value}

    def command(self, args):
        """(native command, args) or (error kind, message). Pi's own commands become the prompt "/name args"."""
        if args.get("v", 0) != 0:
            return "unsupported", "agent.command v must be 0"
        name, text = args.get("name"), args.get("args")
        if text is not None and not isinstance(text, str):
            return "unsupported", "args must be a string"
        if name in ("new_session", "cycle_model"):
            return name, {}
        if name == "compact":
            return "compact", ({"customInstructions": text} if text and text.strip() else {})
        if any(c["name"] == name for c in self.native or []):
            return "prompt", {"message": "/" + name + (" " + text if text and text.strip() else "")}
        return "unsupported", f"command {name!r} is not advertised by this agent"

    def expands(self, message):
        """Whether Pi would run this plain text as a command (a send must stay literal)."""
        if not isinstance(message, str) or not message.startswith("/"):
            return False
        name = message[1:].split(None, 1)[0] if message[1:].strip() else ""
        if name.startswith("skill:") or self.native is None:
            return bool(name)  # unknown command list: Pi might expand it, so refuse rather than guess
        return any(c["name"] == name for c in self.native)

    def current(self, ident):
        return next((o["value"] for o in self.options or [] if o["id"] == ident), None)
