"""Default request-route policy over ordinary config and current public native facts.

Not an answer journal: notice delivery never establishes a native answer.
"""
from agent_request_routes import REQUEST_ROUTES, ROUTES, native_facts, request_key


def pending_requests(resource):
    """None is unknown, not proof that an old notice can be discarded."""
    native = native_facts(resource)
    if native.get('lifecycle') == 'ended':
        return []
    incomplete = native.get('incomplete', [])
    rows = native.get('requests')
    if (resource.get('available') is False or not isinstance(rows, list)
            or not isinstance(incomplete, list) or 'requests' in incomplete):
        return None
    pending = []
    for row in rows:
        if not isinstance(row, dict):
            return None
        if row.get('status', 'pending') != 'pending':
            continue
        if request_key(resource, row) is None:
            return None
        pending.append(row)
    return pending


def principal_for(resource):
    native = native_facts(resource)
    principals = {p for p in (resource.get('principal'), native.get('principal')) if isinstance(p, str) and p}
    return next(iter(principals)) if len(principals) == 1 else None


def inspect_route(policy, agent):
    key = REQUEST_ROUTES + agent
    entry = policy.entries.get(key) or {}
    value = entry.get('value')
    configured = value.get('route') if isinstance(value, dict) else None
    result = {'agent': agent, 'key': key, 'version': entry.get('version', 0), 'configured': configured,
              'route': 'human', 'human': True, 'coordinator': None, 'fallback_reason': None}
    if not entry:
        return result
    if not isinstance(configured, str) or configured not in ROUTES:
        return dict(result, fallback_reason='invalid_request_route')
    if configured == 'human':
        return result
    if policy.membership_error:
        return dict(result, fallback_reason='membership_invalid')
    parent = policy.members.get(agent, {}).get('reports_to')
    if not parent:
        return dict(result, fallback_reason='coordinator_missing')
    resource = policy.resources.get(parent) or {}
    native = native_facts(resource)
    if not resource or resource.get('available') is False or native.get('lifecycle') == 'ended':
        return dict(result, fallback_reason='coordinator_unavailable')
    principal = principal_for(resource)
    if principal is None:
        return dict(result, fallback_reason='coordinator_identity_unavailable')
    if not policy.allowed(principal, 'answer', agent):
        return dict(result, fallback_reason='coordinator_no_answer_right')
    return dict(result, route=configured, human=configured == 'both', coordinator=parent)


def notice_states(resources):
    result = {}
    for agent, resource in resources.items():
        rows = pending_requests(resource)
        result[agent] = None if rows is None else {request_key(resource, row) for row in rows}
    return result


def notice_open(record, states):
    notice = record.get('request_notice')
    if notice is None:
        return False
    if not isinstance(notice, dict) or not isinstance(notice.get('key'), str):
        return True
    keys = states.get(notice.get('agent'))
    return keys is None or notice['key'] in keys
