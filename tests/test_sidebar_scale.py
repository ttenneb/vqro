"""Deterministic phase-boundary regressions; no process, socket, sleep or real backend."""
from collections import Counter
import json
import queue
import threading
import unittest

from e2e_sidebar_scale import Client, sidebar_fence

WORKSPACES = [{'path': '/private/work/w0', 'name': 'Workspace 0'}]


def content(name='Workspace 0', progress=0, rows=1151):
    return {'page': {'number': 1, 'count': 18, 'rows': rows}, 'items': [
        {'id': 'ws:/private/work/w0', 'text': name, 'children': [{'id': 'term:r1', 'progress': progress}]}]}


def observer():
    client = Client.__new__(Client)
    client.cv = threading.Condition()
    client.counts, client.bytes, client.last = Counter(), Counter(), {}
    client.sidebar_event = client.previous_sidebar_event = None
    return client


def deliver(client, frame):
    with client.cv:
        seq = (client.sidebar_event or {}).get('seq', 0)+1
        client.view_event({'seq': seq, 'data': {'view': 'sidebar', 'content': frame}}, len(json.dumps(frame).encode())+100)
        client.cv.notify_all()


class Control:
    def __init__(self, client, progress=0, trailing=False):
        self.client, self.progress, self.trailing, self.first = client, progress, trailing, True

    def call(self, method, params):
        assert method == 'view.publish'
        if self.first and self.trailing:
            deliver(self.client, content(progress=self.progress))
        self.first = False
        deliver(self.client, content(params['content']['data'][0]['name'], self.progress))
        return {}


class SidebarPhaseFences(unittest.TestCase):
    def test_late_partial_startup_cannot_be_counted_as_an_off_page_change(self):
        client = observer()
        deliver(client, content(rows=975))
        old_baseline = client.stats()[0]['sidebar']
        calls, result = queue.Queue(), []
        class DelayedControl:
            def call(self, method, params):
                calls.put(params)
                return {}
        finished = threading.Event()
        def fence():
            result.append(sidebar_fence(DelayedControl(), client, WORKSPACES, 'startup'))
            finished.set()
        thread = threading.Thread(target=fence, daemon=True)
        thread.start()
        marker = calls.get(timeout=2)['content']['data'][0]['name']
        deliver(client, content(rows=1038))  # Late real startup, after the old stats-only baseline.
        self.assertEqual(client.stats()[0]['sidebar']-old_baseline, 1, 'the original window misattributes this frame')
        self.assertFalse(finished.is_set(), 'a nonempty old/partial sidebar is not a completed fence')
        deliver(client, content(marker))
        restored = calls.get(timeout=2)['content']['data'][0]['name']
        self.assertFalse(finished.is_set(), 'observe the restoration before opening the measurement window')
        deliver(client, content(restored))
        thread.join(2)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result[0]['content']['page']['rows'], 1151)
        self.assertEqual(result[0]['counts']['sidebar'], client.stats()[0]['sidebar'])
        self.assertEqual(result[0]['receipt']['natural_publications'], 2, 'warmup is recorded separately, never silently discarded')

    def test_only_two_known_label_changes_are_excluded(self):
        client = observer(); deliver(client, content())
        result = sidebar_fence(Control(client), client, WORKSPACES, 'off_page')
        self.assertEqual(result['receipt']['natural_publications'], 0)
        self.assertEqual(result['receipt']['natural_bytes'], 0)
        self.assertEqual(result['content'], content())

    def test_coalesced_real_detail_change_is_counted_and_remains_visible(self):
        client = observer(); deliver(client, content())
        result = sidebar_fence(Control(client, progress=7), client, WORKSPACES, 'off_page')
        self.assertEqual(result['receipt']['natural_publications'], 1)
        self.assertGreater(result['receipt']['natural_bytes'], 0)
        self.assertTrue(result['receipt']['marker_coalesced_detail'])
        self.assertNotEqual(result['content'], content(), 'the gate must still reject a real off-page invalidation')

    def test_trailing_visible_frame_belongs_to_previous_settling_not_next_phase(self):
        client = observer(); deliver(client, content())
        settled = sidebar_fence(Control(client, progress=3, trailing=True), client, WORKSPACES, 'previous')
        self.assertEqual(settled['receipt']['natural_publications'], 1)
        following = sidebar_fence(Control(client, progress=3), client, WORKSPACES, 'following')
        self.assertEqual(following['receipt']['natural_publications'], 0)
        self.assertGreater(following['receipt']['restored_seq'], settled['receipt']['restored_seq'])


if __name__ == '__main__':
    unittest.main()
