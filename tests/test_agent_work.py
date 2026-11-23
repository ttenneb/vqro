"""Native child/task/plan observation only; no runtime or core effects."""
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'extensions'))
import agent_work


def entities(values):
    return {key: (1, value) for key, value in values.items()}


def flatten(rows):
    for row in rows:
        yield row
        yield from flatten(row.get('children', []))


class WorkRows(unittest.TestCase):
    def fixture(self):
        return entities({
            ('child', 'review'): {'parent': None, 'name': 'Reviewer', 'role': 'review', 'status': 'running', 'items': 'scoped'},
            ('child', 'research'): {'parent': 'review', 'name': 'Researcher', 'status': 'completed', 'items': 'endpoint', 'endpoint': 'r3'},
            ('task', 'tests'): {'title': 'Run tests', 'owner': 'review', 'status': 'running', 'progress': {'summary': '12 tests checked'}},
            ('plan', 'current'): {'owner': None, 'source': 'native', 'update': 'whole', 'items': [
                {'id': 'a', 'text': 'Inspect', 'status': 'completed'}, {'id': 'b', 'text': 'Verify', 'status': 'in_progress', 'blocked_by': ['a']}]},
            ('message', 'root'): {'role': 'assistant', 'content': [{'type': 'text', 'text': 'ROOT'}]},
            ('message', 'kid'): {'child': 'review', 'role': 'assistant', 'content': [{'type': 'text', 'text': 'CHILD'}]},
            ('tool', 'kid-tool'): {'child': 'review', 'title': 'read'},
            ('receipt', 'd1'): {'stage': 'admitted'},
        })

    def test_children_are_nested_with_explicit_observation_navigation(self):
        rows = agent_work.work_rows(self.fixture(), 'r2')
        all_rows = list(flatten(rows))
        review = next(row for row in all_rows if row.get('id') == 'work:child:review')
        research = next(row for row in flatten(review['children']) if row.get('id') == 'work:child:research')
        self.assertEqual(research['command'], 'agents.child_view')
        self.assertEqual(research['args'], {'resource': 'r2', 'child': 'research'})
        self.assertIn('running', review['text'])
        self.assertFalse(any(row.get('command') in ('agents.send', 'agents.stop') for row in all_rows))

    def test_tasks_and_plan_keep_native_owner_progress_source_and_status(self):
        shown = json.dumps(agent_work.work_rows(self.fixture(), 'r2'))
        for value in ('Run tests', 'Reviewer', '12 tests checked', 'Inspect', 'Verify', 'in_progress', 'native', 'blocked'):
            self.assertIn(value, shown)
        self.assertNotIn('%', shown)
        child = json.dumps(agent_work.work_rows(self.fixture(), 'r2', scope='research'))
        self.assertNotIn('Run tests', child)
        self.assertNotIn('Inspect', child)

    def test_scoped_transcript_never_flattens_child_messages_into_parent(self):
        values = self.fixture()
        root = agent_work.transcript_entities(values)
        child = agent_work.transcript_entities(values, 'review')
        self.assertIn(('message', 'root'), root)
        self.assertNotIn(('message', 'kid'), root)
        self.assertNotIn(('plan', 'current'), root)
        self.assertIn(('message', 'kid'), child)
        self.assertIn(('tool', 'kid-tool'), child)
        self.assertNotIn(('message', 'root'), child)
        self.assertIn(('receipt', 'd1'), child)

    def test_empty_unknown_cycles_and_display_only_children_are_safe(self):
        self.assertEqual(agent_work.work_rows({}, 'r2'), [])
        values = entities({('child', 'a'): {'parent': 'b', 'name': 'A', 'status': 'future', 'items': 'none'},
                           ('child', 'b'): {'parent': 'a', 'name': 'B', 'status': 'unknown', 'items': 'none'},
                           ('task', 'bad'): {'progress': 'not a dict', 'owner': {}, 'parent_task': []},
                           ('plan', 'bad'): {'items': [None, {'text': 'Future', 'status': 'future'}]}})
        rows = list(flatten(agent_work.work_rows(values, 'r2')))
        self.assertLess(len(rows), 30)
        children = [row for row in rows if row.get('id', '').startswith('work:child:')]
        self.assertEqual(len(children), 2)
        self.assertFalse(any('command' in row for row in children))
        self.assertIn('unknown', json.dumps(rows))

    def test_missing_child_metadata_keeps_scoped_activity_reachable(self):
        values = entities({('message', 'orphan'): {'child': 'not-reported', 'role': 'assistant', 'content': []}})
        rows = list(flatten(agent_work.work_rows(values, 'r2')))
        self.assertTrue(any(row.get('args', {}).get('child') == 'not-reported' for row in rows))
        self.assertIn('not reported', json.dumps(rows))

    def test_long_lists_are_bounded_and_explicitly_clipped(self):
        values = entities({('plan', 'big'): {'items': [{'text': 'x' * 4000, 'status': 'pending'} for _ in range(500)]}})
        shown = json.dumps(agent_work.work_rows(values, 'r2'))
        self.assertLess(len(shown.encode()), 50000)
        self.assertIn('not shown', shown)


if __name__ == '__main__':
    unittest.main()
