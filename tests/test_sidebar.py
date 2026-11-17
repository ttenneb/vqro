"""Sidebar coalescing without sockets, processes or background threads."""
import copy
from pathlib import Path
import runpy
import sys
import threading
import types
import unittest
from unittest import mock


class CacheLock:
    held = False
    def __enter__(self): self.held = True
    def __exit__(self, *_): self.held = False


class Extension:
    def __init__(self, *_):
        self.entries, self.live = {}, {}
        self.resources = {"r1": {"argv": ["sh"], "state": "live", "status": {"state": "idle"}}}
        self.cache_lock, self.published, self.logs = CacheLock(), [], []
        self.hook, self.fail = None, False
    def value(self, *_): return None
    def on_event(self, fn): return fn
    def command(self, *_): return lambda fn: fn
    def contribute(self, *_): pass
    def run(self): pass
    def publish(self, name, value):
        assert not self.cache_lock.held, "never hold the cache lock across publication"
        if self.fail:
            self.fail = False
            raise RuntimeError("fixture failure")
        self.published.append((name, copy.deepcopy(value)))
        if self.hook: self.hook(name)
    def log(self, *args): self.logs.append(args)


def load():
    fake = types.SimpleNamespace(Extension=Extension, PrimError=RuntimeError)
    with mock.patch.dict(sys.modules, {"prim_ext": fake}), mock.patch.object(threading, "Thread"):
        return runpy.run_path(str(Path(__file__).resolve().parents[1] / "extensions/sidebar.py"))


class SidebarTests(unittest.TestCase):
    def test_status_burst_only_schedules_one_latest_publish(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        for _ in range(500):
            ext.resources["r1"]["status"] = {"state": "working"}
            module["on_event"]("resource", {"id": "r1"})
        self.assertTrue(module["refresh_pending"].is_set())
        self.assertEqual(ext.published, [])
        module["refresh_pending"].clear(); module["refresh"]()
        self.assertEqual([name for name, _ in ext.published], ["sidebar"])
        module["refresh"]()
        self.assertEqual(len(ext.published), 1, "unchanged content is not republished")

    def test_event_during_publish_is_not_lost(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        pending = module["refresh_pending"]
        ext.resources["r1"]["status"] = {"state": "working"}; pending.set()
        def during_publish(name):
            if name == "sidebar" and ext.resources["r1"]["status"]["state"] == "working":
                ext.resources["r1"]["status"] = {"state": "blocked"}; pending.set()
        ext.hook = during_publish
        calls = []
        class Done(Exception): pass
        def wait():
            if len(calls) == 2: raise Done()
            self.assertTrue(pending.is_set()); calls.append(True)
        with mock.patch.object(pending, "wait", side_effect=wait), mock.patch.object(module["time"], "sleep"):
            with self.assertRaises(Done): module["refresh_worker"]()
        self.assertEqual(sum(name == "sidebar" for name, _ in ext.published), 2)
        self.assertTrue(any(name == "sidebar.status" and value["label"] == "1 blocked" for name, value in ext.published))

    def test_failed_publication_does_not_mark_content_as_published(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        ext.resources["r1"]["status"] = {"state": "working"}; ext.fail = True
        with self.assertRaisesRegex(RuntimeError, "fixture failure"): module["refresh"]()
        module["refresh"]()
        self.assertEqual([name for name, _ in ext.published], ["sidebar"])


if __name__ == "__main__": unittest.main()
