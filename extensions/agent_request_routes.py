"""Shared default agent request-route contract; no coordination implementation dependency."""
import hashlib
import json

REQUEST_ROUTES = 'doc/agents/request-routes/'
ROUTES = frozenset(('human', 'coordinator', 'both'))


def native_facts(resource):
    data = resource.get('data')
    native = data.get('agent') if isinstance(data, dict) else None
    return native if isinstance(native, dict) else {}


def request_identity_key(resource, request):
    native = native_facts(resource)
    value = [resource['id'], resource.get('incarnation'), native.get('epoch'), request.get('request')]
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()


def request_key(resource, request):
    native = native_facts(resource)
    epoch, identity, revision, scope = native.get('epoch'), request.get('request'), request.get('revision'), request.get('scope')
    if (not isinstance(epoch, str) or not epoch or not isinstance(identity, str) or not identity
            or type(revision) is not int or revision < 0 or not isinstance(scope, dict) or not scope):
        return None
    value = [resource['id'], resource.get('incarnation'), epoch, identity, revision, scope]
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(',', ':')).encode()).hexdigest()
