"""Shared extension launch mechanism: one backend spawn, observed agent, recoverable UI.

Runtime selection, authentication and request-routing defaults remain launcher policy.
"""
import time

from vqro_ext import VqroError
from vqro_display import label


def valid_argv(argv):
    return isinstance(argv, list) and 1 <= len(argv) <= 64 and all(
        isinstance(arg, str) and arg and '\0' not in arg and len(arg.encode()) <= 32768 for arg in argv)


def runtimes(value):
    rows = value.get('runtimes') if isinstance(value, dict) else None
    if not isinstance(rows, list) or not 1 <= len(rows) <= 32:
        raise VqroError('Agent runtimes must be a nonempty list (at most 32)', 'invalid')
    seen = set()
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get('name'), str) or not row['name'].strip() \
                or len(row['name']) > 64 or row['name'] in seen:
            raise VqroError('runtime names must be unique, nonempty strings (at most 64 characters)', 'invalid')
        if not valid_argv(row.get('argv')):
            raise VqroError('runtime argv must be a nonempty list of literal argument strings', 'invalid')
        if 'bridge_argv' in row and not valid_argv(row['bridge_argv']):
            raise VqroError('bridge_argv must be literal argv for the terminal host', 'invalid')
        env = row.get('bridge_env', {})
        if not isinstance(env, dict) or any(not isinstance(k, str) or not k or '=' in k or '\0' in k
                or not isinstance(v, str) or '\0' in v for k, v in env.items()):
            raise VqroError('bridge_env must map host environment names to strings', 'invalid')
        seen.add(row['name'])
    return rows


def launch_cwd(ext, args):
    context = args.get('context') or {}
    source_id = context.get('resource')
    for prefix in ('agents-', 'agent-'):
        if not source_id and str(context.get('view', '')).startswith(prefix):
            source_id = context['view'][len(prefix):]
    cwd = args.get('cwd')
    if not cwd and source_id:
        scoped = ext.vqro.call('resource.list', {'id': source_id})
        source = next((r for r in scoped.get('resources', []) if r.get('id') == source_id), {})
        cwd = source.get('cwd')
    return cwd or ext.layout_folder(args)


def terminal(ext, host):
    try:
        return ext.invoke('tabs.show', {'resource': host})
    except VqroError:
        ext.notify('Terminal ' + host + ' still exists; tabs unavailable. Use the resource picker.', 'warn')


def launch(ext, params, *, brand, initialize_route, show_terminal=None, expected_agents=None):
    result = ext.vqro.call('resource.spawn', params)  # exactly once; never retried
    if not isinstance(result, dict) or not isinstance(result.get('resource'), dict) or not isinstance(result['resource'].get('id'), str):
        raise VqroError('Launch result is unconfirmed; do not blindly retry.', 'indeterminate')
    host = result['resource']['id']
    show_terminal = show_terminal or (lambda rid: terminal(ext, rid))
    end = time.monotonic() + 12
    lookup_problem = None
    while True:
        try:
            scoped = ext.vqro.call('resource.list', {'host': host})
            rows = scoped.get('resources', [])
            lookup_problem = None
        except VqroError as error:
            lookup_problem = label(f'{error} ({error.kind})', 256)
            rows = []
            if error.kind not in ('timeout', 'overloaded', 'unavailable', 'disconnected'):
                break
        found = [r for r in rows if r.get('kind') == 'agent' and r.get('host') == host and r.get('available')]
        if found:
            if expected_agents is not None and len(found) != expected_agents:
                lookup_problem = 'Unexpected number of hosted agents; select the terminal explicitly'
                break
            resource = found[0]
            route = initialize_route(resource['id'])
            try:
                shown = ext.invoke('agents.show', {'resource': resource['id']})
            except VqroError as error:
                if error.kind not in ('unavailable', 'not_found'):
                    ext.notify(brand + ' agent ' + resource['id'] + ' exists; view unavailable (' + error.kind + ').', 'warn')
                shown = show_terminal(host)
            ext.notify(brand + ' agent ' + resource['id'] + ' created (observed); no prompt sent.', 'ok')
            answer = {'outcome': result['outcome'], 'host': host, 'agent': resource['id'],
                      'client': dict((shown or {}).get('client') or {}, close=True)}
            if route is not None: answer['request_route'] = route
            return answer
        if time.monotonic() >= end:
            break
        time.sleep(.1)
    detail = ' Status unavailable: ' + lookup_problem + '.' if lookup_problem else ''
    ext.notify('Terminal ' + host + ' spawned; ' + brand + ' agent not ready.' + detail + ' Do not blindly retry.', 'warn')
    shown = show_terminal(host)
    return {'outcome': result['outcome'], 'host': host, 'ready': False, 'client': (shown or {}).get('client', {})}
