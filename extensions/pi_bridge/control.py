"""Pi commands and configuration as the interim agent contract (`agent.command`, `agent.configure`).

The resource declares what it supports in its facts, kept current from Pi's own answers:
- `data.commands`: [{name, label, description?, input?: {hint}}] (ACP's AvailableCommand shape)
- `data.config`: [{id, label, type: "select"|"boolean", value, choices?: [{value, label}]}]
  (the agent standard's `session.config`, section 6.3)
Requests outside these declarations are rejected before any native write.
"""
from vqro_display import label

COMMANDS = [
    {"name": "new_session", "label": "New session",
     "description": "Start a fresh Pi session; the current conversation stays in its session file."},
    {"name": "compact", "label": "Compact context",
     "description": "Summarise the conversation to free context (one model call).",
     "input": {"hint": "Custom instructions (optional)"}},
    {"name": "cycle_model", "label": "Next model", "description": "Switch to Pi's next available model."},
]
MODES = [{"value": "one-at-a-time", "label": "One at a time"}, {"value": "all", "label": "All at once"}]
MAX_CHOICES = 64


def model_value(model):
    return f"{model['provider']}/{model['id']}" if isinstance(model, dict) and model.get("provider") and model.get("id") else None


async def read(pi):
    """Current configuration from get_state plus the model and thinking-level lists, or None
    when Pi did not answer (never guessed)."""
    state = await pi.request("get_state")
    if state.get("outcome") != "accepted" or not isinstance(state.get("data"), dict):
        return None
    data = state["data"]
    config = []
    models = await pi.request("get_available_models")
    listed = (models.get("data") or {}).get("models") if models.get("outcome") == "accepted" else None
    current = model_value(data.get("model"))
    if isinstance(listed, list):
        choices = [{"value": model_value(m), "label": label(m.get("name") or m.get("id"), 80)}
                   for m in listed if model_value(m)][:MAX_CHOICES]
        if current and not any(c["value"] == current for c in choices):
            choices.insert(0, {"value": current, "label": label(data["model"].get("name") or current, 80)})
        if choices:
            config.append({"id": "model", "label": "Model", "type": "select", "value": current, "choices": choices})
    levels = await pi.request("get_available_thinking_levels")
    listed = (levels.get("data") or {}).get("levels") if levels.get("outcome") == "accepted" else None
    if isinstance(listed, list) and isinstance(data.get("thinkingLevel"), str):
        config.append({"id": "thinking_level", "label": "Thinking level", "type": "select", "value": data["thinkingLevel"],
                       "choices": [{"value": v, "label": label(v, 40)} for v in listed if isinstance(v, str)][:MAX_CHOICES]})
    for ident, title, field in (("steering_mode", "Steering delivery", "steeringMode"), ("follow_up_mode", "Follow-up delivery", "followUpMode")):
        if data.get(field) in [m["value"] for m in MODES]:
            config.append({"id": ident, "label": title, "type": "select", "value": data[field], "choices": MODES})
    if isinstance(data.get("autoCompactionEnabled"), bool):
        config.append({"id": "auto_compaction", "label": "Auto-compaction", "type": "boolean", "value": data["autoCompactionEnabled"]})
    return config


def configure(config, ident, value):
    """(native command, args) for a declared setting and an allowed value, or an error string."""
    entry = next((c for c in config or [] if c["id"] == ident), None)
    if entry is None:
        return f"setting {ident!r} is not declared by this agent"
    if entry["type"] == "boolean":
        if not isinstance(value, bool):
            return "value must be true or false"
    elif not any(c["value"] == value for c in entry.get("choices", [])):
        return f"value is not one of the declared choices for {ident}"
    if ident == "model":
        provider, _, model = value.partition("/")
        return "set_model", {"provider": provider, "modelId": model}
    if ident == "thinking_level":
        return "set_thinking_level", {"level": value}
    if ident in ("steering_mode", "follow_up_mode"):
        return ("set_steering_mode" if ident == "steering_mode" else "set_follow_up_mode"), {"mode": value}
    if ident == "auto_compaction":
        return "set_auto_compaction", {"enabled": value}
    return f"setting {ident!r} has no native mapping"


def command(name, args):
    """(native command, args) for a declared command, or an error string."""
    if not any(c["name"] == name for c in COMMANDS):
        return f"command {name!r} is not declared by this agent"
    args = args if isinstance(args, dict) else {}
    if name == "compact":
        text = args.get("text")
        if text is not None and not isinstance(text, str):
            return "text must be a string"
        return "compact", ({"customInstructions": text} if text and text.strip() else {})
    return name, {}


def current(config, ident):
    return next((c["value"] for c in config or [] if c["id"] == ident), None)
