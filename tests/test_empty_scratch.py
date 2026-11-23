"""Empty Scratch policy and its first-shell race, through public extension primitives."""
import copy
import threading
import unittest
from unittest import mock

from test_workspace_tabs import Core, load, walk
from vqro_ext import Extension, VqroError, views_in


class EmptyScratch(unittest.TestCase):
    def shells(self, core):
        return [p for method, p in core.calls if method == "resource.spawn"]

    def test_activation_creates_one_shell_and_settings(self):
        core = Core()
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), 1)
        self.assertEqual(core.value("workspace/active"), {"layout": "main"})
        self.assertEqual(core.value("doc/layout/main")["title"], "Scratch")
        row = core.live["settings.page.workspaces"]["items"][0]
        self.assertEqual(row["text"], "Open a shell in an empty Scratch")
        self.assertTrue(row["checked"])
        self.assertIsNone(core.value("doc/workspaces/first-shell"))

    def test_setting_off_and_nonempty_restart_do_not_spawn(self):
        core = Core()
        core.apply([{"op": "set", "key": "doc/settings/workspaces", "value": {"open_shell_when_empty": False}}], "off")
        load(core, "workspaces")
        self.assertEqual(self.shells(core), [])
        core = Core()
        load(core, "workspaces")
        before = len(self.shells(core))
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), before)

    def test_invalid_settings_fall_back_to_the_default(self):
        core = Core()
        core.apply([{"op": "set", "key": "doc/settings/workspaces", "value": "broken"}], "bad preference")
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), 1)
        core.handlers["workspaces.startup"]({"value": False})
        self.assertFalse(core.value("doc/settings/workspaces")["open_shell_when_empty"])

    def test_selected_workspace_is_preserved(self):
        core = Core()
        core.apply([{"op": "set", "key": "view/user", "value": {"kind": "panel"}},
                    {"op": "set", "key": "layout/ws-user", "value": {"view": "user"}},
                    {"op": "set", "key": "workspace/active", "value": {"layout": "ws-user"}}], "user")
        load(core, "workspaces")
        self.assertEqual(self.shells(core), [])
        self.assertEqual(core.value("workspace/active"), {"layout": "ws-user"})

    def test_recovery_placeholder_replaced_but_user_panel_is_kept(self):
        for user_panel in (False, True):
            with self.subTest(user_panel=user_panel):
                core = Core()
                placeholder = {"kind": "panel", "title": "vqro", "items": [{"text": "No terminals yet."}, {"text": "Open the command palette", "command": "client.palette"}]}
                core.apply([{"op": "set", "key": "view/vqro-empty", "value": placeholder},
                            {"op": "set", "key": "view/user", "value": {"kind": "panel", "items": [{"text": "keep"}]}},
                            {"op": "set", "key": "layout/main", "value": {"split": "h", "children": [{"view": "user"}, {"tabs": [{"title": "vqro", "node": {"view": "vqro-empty"}}], "active": 0}]} if user_panel else {"tabs": [{"title": "vqro", "node": {"view": "vqro-empty"}}], "active": 0}},
                            {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}], "placeholder")
                load(core, "workspaces")
                self.assertEqual(len(self.shells(core)), 1)
                self.assertNotIn("vqro-empty", views_in(core.value("layout/main")))
                if user_panel:
                    self.assertIn("user", views_in(core.value("layout/main")))

    def test_user_panel_tab_is_not_empty_or_discarded(self):
        core = Core()
        core.apply([{"op": "set", "key": "view/user", "value": {"kind": "panel"}},
                    {"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "user", "node": {"view": "user"}}], "active": 0}}], "user")
        saved = copy.deepcopy(core.value("layout/main"))
        load(core, "workspaces")
        self.assertEqual(self.shells(core), [])
        self.assertEqual(core.value("layout/main"), saved)

    def test_bare_terminal_layout_is_not_empty(self):
        core = Core()
        core.apply([{"op": "set", "key": "view/term", "value": {"kind": "terminal", "resource": "old"}},
                    {"op": "set", "key": "layout/main", "value": {"view": "term"}}], "old terminal")
        load(core, "workspaces")
        self.assertEqual(self.shells(core), [])

    def test_changed_recovery_panel_is_not_discarded(self):
        core = Core()
        core.apply([{"op": "set", "key": "view/vqro-empty", "value": {"kind": "panel", "items": [{"text": "user content"}]}},
                    {"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "user", "node": {"view": "vqro-empty"}}], "active": 0}}], "user panel")
        load(core, "workspaces")
        self.assertEqual(self.shells(core), [])
        self.assertEqual(core.value("view/vqro-empty")["items"], [{"text": "user content"}])

    def test_prefix_n_completes_before_bootstrap(self):
        core = Core()
        mods = load(core, "tabs")
        mods["tabs"]["open_tab"]({})
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), 1)

    def test_prefix_n_works_with_a_retained_automatic_claim(self):
        core = Core()
        core.apply([{"op": "set", "key": "doc/workspaces/first-shell", "value": {"attempt": "uncertain"}}], "old automatic attempt")
        core.notify = mock.Mock()
        modules = load(core, "tabs", "workspaces")
        self.assertEqual(self.shells(core), [])
        self.assertIn("An earlier automatic Scratch shell was not confirmed; press Ctrl-g n for a new one.", str(core.notify.call_args))
        modules["tabs"]["open_tab"]({})
        self.assertEqual(len(self.shells(core)), 1)
        # A layout event queues cleanup of the now-moot automatic claim.
        with mock.patch.object(threading, "Thread") as queued:
            modules["workspaces"]["on_event"]("entries", {"changes": [{"key": "layout/main"}]})
        queued.call_args.kwargs["target"]()
        self.assertIsNone(core.value("doc/workspaces/first-shell"))
        self.assertEqual(modules["workspaces"]["startup_notice"][0], "")

    def test_stale_claim_notice_names_the_configured_prefix(self):
        core = Core()
        core.apply([{"op": "set", "key": "doc/workspaces/first-shell", "value": {"attempt": "uncertain"}},
                    {"op": "set", "key": "doc/settings/tui", "value": {"prefix": "C-a", "key_labels": "short"}}], "old automatic attempt")
        module = load(core, "workspaces")["workspaces"]
        self.assertEqual(module["startup_notice"][0], "An earlier automatic Scratch shell was not confirmed; press C-a n for a new one.")
        self.assertEqual(self.shells(core), [])

    def test_explicit_main_is_not_blocked_by_a_retained_automatic_claim(self):
        core = Core()
        core.apply([{"op": "set", "key": "doc/workspaces/first-shell", "value": {"attempt": "uncertain"}}], "old automatic attempt")
        load(core, "workspaces")
        core.handlers["workspaces.main"]({})
        self.assertEqual(len(self.shells(core)), 1)

    def test_nonempty_scratch_clears_stale_claim_on_activation(self):
        core = Core()
        load(core, "tabs")["tabs"]["open_tab"]({})
        core.apply([{"op": "set", "key": "doc/workspaces/first-shell", "value": {"attempt": "uncertain"}}], "old automatic attempt")
        original = core.apply
        core.apply = mock.Mock(wraps=original)
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), 1)
        self.assertIsNone(core.value("doc/workspaces/first-shell"))
        deletes = [op for call in core.apply.call_args_list for op in call.args[0] if op["op"] == "delete" and op["key"] == "doc/workspaces/first-shell"]
        self.assertEqual(deletes, [{"op": "delete", "key": "doc/workspaces/first-shell", "expect": 1}])

    def test_raw_layout_write_wins_inflight_bootstrap(self):
        self.raw_layout_race(uncertain_kill=False)

    def test_uncertain_stop_retains_claim_and_says_so(self):
        self.raw_layout_race(uncertain_kill=True)

    def raw_layout_race(self, uncertain_kill):
        core = Core()
        listeners = []
        core.on_event = lambda fn: (listeners.append(fn), fn)[1]
        core.apply([{"op": "set", "key": "view/sidebar", "value": {"kind": "panel"}}], "sidebar already available")
        core.retrying = lambda fn, attempts=3: Extension.retrying(core, fn, attempts)
        entered, release = threading.Event(), threading.Event()
        original = core.call
        def paused(method, params=None):
            if method == "resource.spawn":
                entered.set()
                self.assertTrue(release.wait(3))
            if method == "resource.kill" and uncertain_kill:
                core.calls.append((method, copy.deepcopy(params)))
                raise VqroError("kill reply lost", "timeout", data={"outcome": "indeterminate"})
            return original(method, params)
        core.call = paused
        errors, modules = [], {}
        def boot():
            try: modules.update(load(core, "workspaces"))
            except Exception as error: errors.append(error)
        worker = threading.Thread(target=boot)
        worker.start()
        self.assertTrue(entered.wait(3))
        user_layout = {"tabs": [{"title": "mine", "node": {"view": "user"}}], "active": 0}
        try:
            core.apply([{"op": "set", "key": "view/user", "value": {"kind": "panel"}},
                        {"op": "set", "key": "layout/main", "value": user_layout},
                        {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}], "user layout during spawn")
            listeners[0]("entries", {"changes": [{"key": "layout/main"}]})
        finally:
            release.set()
            worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(core.value("layout/main"), user_layout)
        self.assertEqual([p for m, p in core.calls if m == "resource.kill"], [{"resource": "r1"}])
        self.assertEqual(len(self.shells(core)), 1)
        if uncertain_kill:
            self.assertIsNotNone(core.value("doc/workspaces/first-shell"))
            self.assertIn("stopping it was not confirmed", modules["workspaces"]["startup_notice"][0])
            self.assertIn("press Ctrl-g n", modules["workspaces"]["startup_notice"][0])
            modules["workspaces"]["bootstrap"]()
            self.assertEqual(core.value("doc/workspaces/first-shell")["stopping"], "r1")
            self.assertIn("stopping it was not confirmed", modules["workspaces"]["startup_notice"][0])
            self.assertEqual(len([m for m, _ in core.calls if m == "resource.kill"]), 1)
        else:
            self.assertIsNone(core.value("doc/workspaces/first-shell"))
            self.assertEqual(core.resources["r1"]["state"], "finished")

    def test_backend_availability_retries_only_definite_rejection(self):
        core = Core()
        call = core.call
        core.call = mock.Mock(side_effect=lambda m, p=None: (_ for _ in ()).throw(VqroError("no backend", "unavailable", data={"outcome": "rejected"})) if m == "resource.spawn" else call(m, p))
        core.notify = mock.Mock()
        modules = load(core, "workspaces")
        self.assertIsNone(core.value("doc/workspaces/first-shell"))
        self.assertTrue(core.notify.called)
        core.call = call
        with mock.patch.object(threading, "Thread") as queued:
            modules["workspaces"]["on_event"]("provider", {})
        queued.call_args.kwargs["target"]()
        self.assertEqual(len(self.shells(core)), 1)

    def test_availability_during_another_bootstrap_is_not_dropped(self):
        core = Core()
        call = core.call
        core.call = mock.Mock(side_effect=lambda m, p=None: (_ for _ in ()).throw(VqroError("no backend", "unavailable", data={"outcome": "rejected"})) if m == "resource.spawn" else call(m, p))
        module = load(core, "workspaces")["workspaces"]
        core.call = call
        ready = threading.Event()
        publish = core.publish
        def published(name, content):
            publish(name, content)
            if name == "settings.page.workspaces":
                ready.set()
        core.publish = published
        lock = module["bootstrap_lock"]
        lock.acquire()
        try:
            module["on_event"]("provider", {})
            self.assertEqual(self.shells(core), [])
        finally:
            lock.release()
        # The queued availability reaction must run once the previous attempt leaves the lock.
        self.assertTrue(ready.wait(2))
        self.assertEqual(len(self.shells(core)), 1)

    def test_indeterminate_spawn_is_not_replayed_on_restart(self):
        core = Core()
        call = core.call
        def uncertain(m, p=None):
            if m == "resource.spawn":
                call(m, p)
                raise VqroError("lost reply", "timeout", data={"outcome": "indeterminate"})
            return call(m, p)
        core.call = uncertain
        load(core, "workspaces")
        core.call = call
        load(core, "workspaces")
        self.assertEqual(len(self.shells(core)), 1)
        self.assertIsNotNone(core.value("doc/workspaces/first-shell"))

    def test_explicit_prefix_n_is_not_blocked_by_inflight_bootstrap(self):
        core = Core()
        core.retrying = lambda fn, attempts=3: Extension.retrying(core, fn, attempts)
        mods = load(core, "tabs")
        entered, release = threading.Event(), threading.Event()
        original = core.call
        def paused(m, p=None):
            if m == "resource.spawn" and threading.current_thread() is worker:
                entered.set()
                self.assertTrue(release.wait(3))
            return original(m, p)
        core.call = paused
        errors = []
        def boot():
            try: load(core, "workspaces")
            except Exception as error: errors.append(error)
        worker = threading.Thread(target=boot)
        worker.start()
        self.assertTrue(entered.wait(3))
        try:
            mods["tabs"]["open_tab"]({})
            self.assertEqual(len(self.shells(core)), 1)  # n already worked, before releasing bootstrap
        finally:
            release.set()
            worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(len(self.shells(core)), 2)  # both native spawns happened, but only the user's is shown
        self.assertEqual(len(next(n for n in walk(core.value("layout/main")) if "tabs" in n)["tabs"]), 1)
        self.assertEqual([p for m, p in core.calls if m == "resource.kill"], [{"resource": "r2"}])
        self.assertEqual(core.resources["r2"]["state"], "finished")
        self.assertIsNone(core.value("doc/workspaces/first-shell"))
