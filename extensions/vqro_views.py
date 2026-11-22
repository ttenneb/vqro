"""Resource-kind navigation policy shared by workspace extensions (not core)."""
import time

from vqro_ext import VqroError, views_in
from vqro_display import label


def ended(resource):
    return resource is None or resource.get("state") in ("finished", "gone", "ended", "closed")


def title(resource, resources):
    rid = resource.get("id")
    agent = next((r for r in resources.values() if r.get("kind") == "agent" and r.get("host") == rid), None)
    if agent:
        return label(agent.get("name") or "Agent") + " " + agent["id"] + " bridge"
    return label(resource.get("name") or (resource.get("argv") or ["Resource"])[0].split("/")[-1])


def terminal_target(ext, rid):
    """Return only an actual terminal; agents route to their UI when it is on.

    Do not turn a failed/uncertain agents.show into a different layout effect.
    The escape hatch is only for an unavailable command, never for arbitrary errors.
    """
    resource = ext.resources.get(rid)
    if resource is None:
        raise VqroError("resource is gone", "not_found")
    if resource.get("kind") == "agent":
        try:
            return None, ext.invoke("agents.show", {"resource": rid})
        except VqroError as error:
            if error.kind not in ("unavailable", "not_found"):
                raise
        resource = ext.resources.get(resource.get("host"))
        if not resource or resource.get("kind") != "terminal":
            raise VqroError("agent UI is off and no bridge terminal is available", "unavailable")
    if resource.get("kind") != "terminal":
        raise VqroError("no view for resource kind " + str(resource.get("kind")), "unsupported")
    return resource, None


def view_resource(ext, view):
    value = ext.value("view/" + view) or {}
    if value.get("kind") == "terminal":
        return value.get("resource")
    for prefix in ("agents-", "agent-"):
        if view.startswith(prefix) and view[len(prefix):].startswith("r") and view[len(prefix)+1:].isdigit():
            return view[len(prefix):]
    return None


def unknown_since(ext, resource):
    """Local "HH:MM" when the resource's provider disconnected (core registry `since`), else None.

    Only a disconnected provider gives a time: that is when vqro stopped knowing the resource's state.
    """
    provider = ext.providers.get(resource.get("provider")) or {}
    since = provider.get("since")
    if provider.get("connected") is False and isinstance(since, (int, float)) and since > 0:
        return time.strftime("%H:%M", time.localtime(since / 1000))
    return None


# ---- closing ends what it closes (human feedback batch 6) ---------------------------------------------------------

# Plain shells close without asking: vqro can't see what runs inside one (no backend reports a foreground process),
# and asking on every shell would add a click to almost every close (the human's choice (a)).
SHELLS = {"sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "pwsh", "elvish", "xonsh"}


def busy(ext, rid):
    """Why closing what shows `rid` should ask first ("cargo is still running.", "Pi r100 is working on a reply."),
    or None: ended, a plain shell, or an idle agent (the human's choice (a), batch 6: only busy things ask). An agent
    is busy while working, blocked or holding queued messages (agent-standard-coord's wording)."""
    r = ext.resources.get(rid)
    if ended(r):
        return None
    if r.get("kind") == "agent":
        facts = (r.get("data") or {}).get("agent") or {}
        state = (r.get("status") or {}).get("state") or facts.get("state")
        queued = len(facts.get("queue") or [])
        name = f"{label(r.get('name') or 'Agent')} {rid}"
        if state == "working":
            return f"{name} is working on a reply."
        if state == "blocked":
            return f"{name} is blocked."
        if queued:
            return f"{name} has {queued} queued message{'s' if queued != 1 else ''}."
        return None

    # A terminal hosting a busy agent (its bridge) is as busy as the agent.
    for agent in ext.resources.values():
        if agent.get("kind") == "agent" and agent.get("host") == rid and busy(ext, agent["id"]):
            return busy(ext, agent["id"])
    if (r.get("status") or {}).get("state") in ("working", "blocked"):
        return f"{title(r, ext.resources)} is {r['status']['state']}."
    program = ((r.get("argv") or [""])[0] or "").rsplit("/", 1)[-1]
    return None if program in SHELLS else f"{title(r, ext.resources)} is still running."


def graceful(ext, rid):
    """Whether an agent declares its own stop (agent v0 `agent.stop`); without it, stopping kills its terminal."""
    caps = (((ext.resources.get(rid) or {}).get("data") or {}).get("agent") or {}).get("caps") or {}
    return "agent.stop" in caps


def stop(ext, rid):
    """End a live resource: a terminal's program is killed; an agent is stopped by the agents extension, gracefully
    when it can (`agents.stop` with `how: "auto"`, confirmed by the caller's own popup)."""
    r = ext.resources.get(rid)
    if ended(r):
        return None
    if r.get("kind") == "agent":
        return ext.invoke("agents.stop", {"resource": rid, "confirm": True, "how": "auto"})
    return ext.vqro.call("resource.kill", {"resource": rid})


def shown_resources(ext, layouts):
    """Resources some pane of these layouts (`{key: root}`, overriding the cached ones) shows."""
    roots = {k: e["value"] for k, e in ext.entries.items() if k.startswith("layout/")}
    roots.update(layouts)
    return {view_resource(ext, v) for root in roots.values() if root for v in views_in(root)} - {None}


def confirm_popup(ext, view, title_, reasons, command, args, stop_label, keep_label, problem=None):
    """Publish the close confirmation: what is running, then mouse-only choices. Each runs `command` again with
    `stop`, `keep` or `cancel` (and the popup's own context, so it acts on the same pane or tab)."""
    items = [{"type": "text", "text": problem, "style": "error"}] if problem else []
    items += [{"type": "text", "text": reason, "style": "warn"} for reason in reasons]
    if not problem:
        items.append({"text": stop_label, "style": "error", "command": command, "args": dict(args, stop=True)})
    items += [{"text": keep_label, "command": command, "args": dict(args, keep=True)},
              {"text": "Cancel", "style": "dim", "command": command, "args": dict(args, cancel=True)}]
    ext.publish(view, {"title": title_, "items": items})
    return {"client": {"popup": {"view": view, "width": 64}}}
