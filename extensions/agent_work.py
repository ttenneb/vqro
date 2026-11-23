"""Bounded native child/task/plan observations; no lifecycle or coordination policy."""
import math

import agent_transcript as transcript

MAX_ITEMS = 128
WORK_BYTES = 24 * 1024


def shown(value, limit=256):
    return transcript.clip(transcript.plain(value), limit) if isinstance(value, str) else ''


def values(entities, kind):
    return {eid: value for (k, eid), (_, value) in entities.items() if k == kind and isinstance(value, dict)}


def scoped_ids(entities):
    return {value['child'] for (kind, _), (_, value) in entities.items()
            if kind in ('message', 'tool', 'request') and isinstance(value, dict)
            and isinstance(value.get('child'), str) and value['child']}


def transcript_entities(entities, scope=None):
    """Receipts stay available for historical labels; child content never masquerades as the parent's output."""
    out = {}
    for key, record in entities.items():
        kind, _ = key
        value = record[1]
        if not isinstance(value, dict):
            continue
        if kind in ('child', 'task', 'goal') or kind == 'plan' and isinstance(value.get('items'), list):
            continue
        if kind == 'receipt':
            out[key] = record
        elif kind in ('message', 'tool', 'request'):
            if value.get('child') == scope:
                out[key] = record
        elif scope is None and not value.get('child'):
            out[key] = record
    return out


def status(value, allowed):
    native = value.get('status')
    return native if isinstance(native, str) and native in allowed else 'unknown'


def partial(value):
    return ' · incomplete' if value.get('incomplete') is True else ''


def style(state):
    return {'completed': 'ok', 'failed': 'error', 'waiting': 'warn', 'running': 'accent',
            'in_progress': 'accent'}.get(state, 'dim')


def work_rows(entities, resource, scope=None):
    children = values(entities, 'child')
    tasks = values(entities, 'task')
    plans = values(entities, 'plan')
    goals = values(entities, 'goal')
    sections = []
    by_parent = {}
    for eid, value in children.items():
        parent = value.get('parent')
        if 'parent' in value and (parent is None or isinstance(parent, str)):
            by_parent.setdefault(parent, []).append(eid)

    def owner(value, key='owner'):
        if key not in value:
            return 'owner not reported'
        eid = value[key]
        if eid is None:
            return 'main conversation'
        return (shown(children.get(eid, {}).get('name')) if isinstance(eid, str) else '') or shown(eid) or 'owner unknown'

    def usage(value):
        rows = []
        if isinstance(value.get('usage'), dict) and value['usage']:
            row = transcript.facts_footer({'usage': value['usage']}, 'Reported usage')
            row.pop('pin', None)
            rows.append(row)
        return rows

    def navigation(row, eid):
        row.update(command='agents.child_view', args={'resource': resource, 'child': eid},
                   actions=[{'text': 'Open view', 'command': 'agents.child_view',
                             'args': {'resource': resource, 'child': eid}}])

    child_rows, seen = [], set()
    allowed_child = ('pending', 'running', 'waiting', 'completed', 'failed', 'cancelled', 'unknown')

    def child_row(eid, depth=0):
        if eid in seen or len(seen) >= MAX_ITEMS:
            return None
        seen.add(eid)
        value = children[eid]
        state = status(value, allowed_child)
        text = ' · '.join(filter(None, (shown(value.get('name')) or shown(eid), shown(value.get('role')), state)))
        row = {'id': 'work:child:' + eid, 'text': text + partial(value), 'style': style(state), 'expanded': False}
        mode = value.get('items')
        if mode == 'scoped' or mode == 'endpoint' and isinstance(value.get('endpoint'), str):
            navigation(row, eid)
        else:
            row['description'] = 'View not reported'
        details = usage(value)
        model = value.get('model')
        model = (model.get('label') or model.get('id')) if isinstance(model, dict) else model
        if shown(model):
            details.append({'type': 'text', 'style': 'dim', 'text': 'Model: ' + shown(model)})
        native = value.get('native')
        if isinstance(native, dict) and shown(native.get('status')):
            details.append({'type': 'text', 'style': 'dim', 'text': 'Native status: ' + shown(native['status'])})
        descendants = by_parent.get(eid, [])
        if depth < 12:
            for key in descendants:
                nested = child_row(key, depth + 1)
                if nested is not None:
                    details.append(nested)
        elif descendants:
            details.append({'type': 'text', 'style': 'dim', 'text': 'More child depth not shown here (pane limit)'})
        if details:
            row['children'] = transcript.bounded_children(details, WORK_BYTES, 'child details')
        return row

    for eid in by_parent.get(scope, []):
        row = child_row(eid)
        if row:
            child_rows.append(row)
    if scope is None:
        # Missing parents, cycles and depth-limited descendants remain visible, never silently discarded.
        for eid in children:
            if eid not in seen:
                row = child_row(eid)
                if row:
                    row['description'] = 'Parent relation unavailable, cyclic or outside the displayed tree'
                    child_rows.append(row)
        orphan_scopes = sorted(scoped_ids(entities) - set(children))
        for eid in orphan_scopes[:MAX_ITEMS]:
            row = {'id': 'work:scope:' + eid, 'text': 'Scoped child activity · ' + shown(eid),
                   'description': 'Child metadata not reported', 'style': 'dim'}
            navigation(row, eid)
            child_rows.append(row)
        if len(orphan_scopes) > MAX_ITEMS:
            child_rows.append({'type': 'text', 'style': 'dim', 'text': 'More scoped activity not shown here (pane limit)'})
    if len(seen) < len(children) and scope is None:
        child_rows.append({'type': 'text', 'style': 'dim', 'text': 'More child agents not shown here (pane limit)'})
    if child_rows:
        sections.append({'id': 'work:children', 'text': 'Child agents', 'style': 'heading', 'expanded': True,
                         'children': transcript.bounded_children(child_rows, WORK_BYTES, 'child agents')})

    selected_tasks = {eid: value for eid, value in tasks.items() if scope is None or value.get('owner') == scope}
    task_seen = set()
    by_task_parent = {}
    for eid, value in selected_tasks.items():
        if isinstance(value.get('parent_task'), str):
            by_task_parent.setdefault(value['parent_task'], []).append(eid)

    def task_row(eid, depth=0):
        if eid in task_seen or len(task_seen) >= MAX_ITEMS:
            return None
        task_seen.add(eid)
        value = selected_tasks[eid]
        state = status(value, ('running', 'completed', 'failed', 'stopped', 'unknown'))
        row = {'id': 'work:task:' + eid, 'text': (shown(value.get('title')) or shown(eid)) + ' · ' + state + partial(value),
               'style': style(state), 'description': 'Owner: ' + owner(value), 'expanded': False}
        details = usage(value)
        if isinstance(value.get('child'), str):
            details.append({'type': 'text', 'style': 'dim', 'text': 'Child: ' +
                            (shown(children.get(value['child'], {}).get('name')) or shown(value['child']))})
        if isinstance(value.get('parent_task'), str):
            row['description'] += ' · Parent task: ' + shown(value['parent_task'])
        if shown(value.get('kind')):
            details.append({'type': 'text', 'style': 'dim', 'text': 'Kind: ' + shown(value['kind'])})
        progress = value.get('progress')
        if isinstance(progress, dict) and shown(progress.get('summary'), 512):
            details.append({'type': 'text', 'text': shown(progress['summary'], 512), 'style': 'dim'})
        if depth < 12:
            for key in by_task_parent.get(eid, []):
                nested = task_row(key, depth + 1)
                if nested is not None:
                    details.append(nested)
        if details:
            row['children'] = transcript.bounded_children(details, WORK_BYTES, 'task details')
        return row

    task_rows = []
    for eid, value in selected_tasks.items():
        parent = value.get('parent_task')
        if not isinstance(parent, str) or parent not in selected_tasks:
            row = task_row(eid)
            if row:
                task_rows.append(row)
    for eid in selected_tasks:
        if eid not in task_seen:
            row = task_row(eid)
            if row:
                task_rows.append(row)
    if len(task_seen) < len(selected_tasks):
        task_rows.append({'type': 'text', 'style': 'dim', 'text': 'More tasks not shown here (pane limit)'})
    if task_rows:
        sections.append({'id': 'work:tasks', 'text': 'Background tasks', 'style': 'heading', 'expanded': True,
                         'children': transcript.bounded_children(task_rows, WORK_BYTES, 'tasks')})

    for eid, value in list(goals.items())[:MAX_ITEMS]:
        if scope is not None and value.get('owner') != scope:
            continue
        state = status(value, ('active', 'paused', 'blocked', 'completed', 'unknown'))
        body = [{'type': 'text', 'text': shown(value.get('objective'), 512) or 'Objective not reported'}]
        budget = value.get('budget') if isinstance(value.get('budget'), dict) else {}
        used = value.get('used') if isinstance(value.get('used'), dict) else {}
        reported = []
        if type(budget.get('tokens')) is int and budget['tokens'] >= 0:
            reported.append('Budget tokens: ' + str(budget['tokens']))
        if type(used.get('tokens')) is int and used['tokens'] >= 0:
            reported.append('Used tokens: ' + str(used['tokens']))
        seconds = used.get('seconds')
        if (type(seconds) is int and seconds >= 0) or (type(seconds) is float and seconds >= 0 and math.isfinite(seconds)):
            reported.append('Used seconds: ' + str(seconds))
        if reported:
            body.append({'type': 'text', 'style': 'dim', 'text': shown(' · '.join(reported), 1024)})
        sections.append({'id': 'work:goal:' + eid, 'text': 'Goal · ' + state + partial(value),
                         'style': 'heading', 'expanded': True, 'description': 'Owner: ' + owner(value),
                         'children': body})
    if len(goals) > MAX_ITEMS:
        sections.append({'type': 'text', 'style': 'dim', 'text': 'More goals not shown here (pane limit)'})

    for eid, value in list(plans.items())[:MAX_ITEMS]:
        items = value.get('items')
        if not isinstance(items, list) or scope is not None and value.get('owner') != scope:
            continue
        body = []
        if partial(value):
            body.append({'type': 'text', 'style': 'warn', 'text': 'Native plan incomplete; omitted details are unknown.'})
        if shown(value.get('explanation'), 512):
            body.append({'type': 'text', 'text': shown(value['explanation'], 512), 'style': 'dim'})
        for index, item in enumerate(items[:MAX_ITEMS]):
            if not isinstance(item, dict) or not isinstance(item.get('text'), str):
                body.append({'type': 'text', 'text': 'List item details not reported', 'style': 'dim'})
                continue
            state = status(item, ('pending', 'in_progress', 'completed', 'cancelled', 'unknown'))
            row = {'id': 'work:plan:' + eid + ':' + (shown(item.get('id')) or str(index)),
                   'text': shown(item['text'], 512) + ' · ' + state, 'style': style(state)}
            details = []
            if shown(item.get('description'), 512):
                details.append(shown(item['description'], 512))
            if state == 'in_progress' and shown(item.get('active_form'), 512):
                details.append('Active: ' + shown(item['active_form'], 512))
            if isinstance(item.get('blocked_by'), list):
                blockers = [shown(key) for key in item['blocked_by'][:MAX_ITEMS] if shown(key)]
                if blockers:
                    details.append('blocked by: ' + ', '.join(blockers))
            if 'owner' in item:
                details.append('Owner: ' + owner(item))
            if details:
                row['description'] = shown(' · '.join(details), 1024)
            body.append(row)
        if len(items) > MAX_ITEMS:
            body.append({'type': 'text', 'text': 'More list items not shown here (pane limit)', 'style': 'dim'})
        if not items:
            body.append({'type': 'text', 'text': 'No list items in this partial report' if partial(value) else 'Empty list (reported)', 'style': 'dim'})
        source = value.get('source')
        title = 'Plan / todo list' + (' · ' + source if source in ('native', 'tool') else '') + partial(value)
        sections.append({'id': 'work:plan:' + eid, 'text': title, 'style': 'heading', 'expanded': True,
                         'description': 'Owner: ' + owner(value),
                         'children': transcript.bounded_children(body, WORK_BYTES // 2, 'list items')})
    if len(plans) > MAX_ITEMS:
        sections.append({'type': 'text', 'style': 'dim', 'text': 'More plan lists not shown here (pane limit)'})
    if not sections:
        return []
    return [{'id': 'work', 'text': 'Work', 'style': 'heading', 'expanded': True,
             'children': transcript.bounded_children(sections, WORK_BYTES, 'work sections')}]
