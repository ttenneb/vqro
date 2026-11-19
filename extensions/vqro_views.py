"""Resource-kind navigation policy shared by workspace extensions (not core)."""
import time

from vqro_ext import VqroError
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
