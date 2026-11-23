"""Default-backend choices are one undoable public transaction, not provider enable/disable effects."""
import copy
import unittest
from unittest import mock

from test_settings import load


class DefaultBackend(unittest.TestCase):
    def setup_backend(self):
        module = load()
        ext = module["ext"]
        for number, (name, enabled, default) in enumerate((("alpha", True, True), ("beta", False, False), ("zeta", True, True)), 11):
            ext.entries["extension/" + name] = {"version": number, "value": {
                "kind": "provider", "argv": ["never-run", "--literal"], "enabled": enabled,
                "default": default, "description": "preserve", "custom": {"untouched": True}}}
            ext.states[name] = {"enabled": enabled, "display_status": "running" if enabled else "off",
                                "manifest": {"title": name.title(), "kind": "provider"}}
        ext.entries["extension/third-party"]["value"].update(kind="interface", default=True)
        ext.version = lambda key: ext.entries.get(key, {}).get("version", 0)
        ext.fresh = mock.Mock()
        ext.retrying = lambda fn, attempts=3: fn()
        history = []
        def apply(ops, note):
            before = copy.deepcopy(ext.entries)
            candidate = copy.deepcopy(ext.entries)
            for op in ops:
                self.assertEqual(op["expect"], candidate[op["key"]]["version"])
                candidate[op["key"]] = {"version": candidate[op["key"]]["version"] + 1, "value": copy.deepcopy(op["value"])}
            ext.entries = candidate
            history.append(before)
            return {"txn": len(history), "undoable": True}
        ext.apply = mock.Mock(side_effect=apply)
        return module, ext, history

    def test_pick_is_one_versioned_transaction_and_can_be_undone(self):
        module, ext, history = self.setup_backend()
        before = copy.deepcopy(ext.entries)
        reply = module["set_default_backend"]({"value": "beta"})
        self.assertEqual(reply, {"txn": 1, "undoable": True})
        ext.apply.assert_called_once()
        ops = ext.apply.call_args.args[0]
        self.assertEqual({op["key"]: op["expect"] for op in ops}, {"extension/alpha": 11, "extension/beta": 12, "extension/zeta": 13})
        for name in ("alpha", "beta", "zeta"):
            value = ext.value("extension/" + name)
            self.assertEqual(value.get("default", False), name == "beta")
            expected = copy.deepcopy(before["extension/" + name]["value"])
            expected.pop("default", None)
            actual = dict(value); actual.pop("default", None)
            self.assertEqual(actual, expected)
        self.assertFalse(ext.value("extension/beta")["enabled"], "selection must not enable a disabled backend")
        self.assertEqual(ext.entries["extension/third-party"], before["extension/third-party"])
        self.assertFalse(hasattr(ext, "invoked"), "only workspace.apply, no native/provider effects")
        # The transaction's before-images restore all default flags together, not one provider at a time.
        ext.entries = history[reply["txn"] - 1]
        self.assertEqual(ext.entries, before)

    def test_none_clears_every_provider_default_in_one_transaction(self):
        module, ext, _ = self.setup_backend()
        module["set_default_backend"]({"value": ""})
        ext.apply.assert_called_once()
        for name in ("alpha", "beta", "zeta"):
            self.assertNotIn("default", ext.value("extension/" + name))
        self.assertTrue(ext.value("extension/third-party")["default"])

    def test_pick_list_marks_effective_default_and_disabled_but_selectable(self):
        module, ext, _ = self.setup_backend()
        module["refresh"]()
        page = next(p for p in ext.published["settings"]["pages"] if p["title"] == "Backends")
        row = page["items"][0]
        self.assertEqual(row["type"], "select")
        self.assertEqual(row["command"], "settings.default_backend")
        self.assertEqual(row["value"], "alpha", "matches core's sorted first explicit default if legacy state has several")
        self.assertEqual([o["value"] for o in row["options"]], ["", "alpha", "beta", "zeta"])
        disabled = next(o for o in row["options"] if o["value"] == "beta")
        self.assertIn("disabled", disabled["label"])
        self.assertFalse(disabled.get("disabled", False))
        self.assertIn("fallback", row["description"], "none clears preference, not the core's fallback routing")
        self.assertIn("restart", row["description"], "changing extension declarations uses their normal restart mechanism")

    def test_refresh_after_pick_and_none_marks_new_selection(self):
        module, ext, _ = self.setup_backend()
        module["set_default_backend"]({"value": "beta"})
        module["on_event"]("entries", {"changes": [{"key": "extension/beta"}]})
        row = next(p for p in ext.published["settings"]["pages"] if p["title"] == "Backends")["items"][0]
        self.assertEqual(row["value"], "beta")
        module["set_default_backend"]({"value": ""})
        module["on_event"]("entries", {"changes": [{"key": "extension/alpha"}]})
        row = next(p for p in ext.published["settings"]["pages"] if p["title"] == "Backends")["items"][0]
        self.assertEqual(row["value"], "")

    def test_picking_the_same_canonical_default_is_a_noop(self):
        module, ext, _ = self.setup_backend()
        module["set_default_backend"]({"value": "beta"})
        ext.apply.reset_mock()
        self.assertEqual(module["set_default_backend"]({"value": "beta"}), {"unchanged": True})
        ext.apply.assert_not_called()

    def test_unknown_nonprovider_or_nonstring_pick_is_rejected_without_transaction(self):
        for value in ("missing", "settings", None, 42):
            with self.subTest(value=value):
                module, ext, _ = self.setup_backend()
                with self.assertRaises(RuntimeError):
                    module["set_default_backend"]({"value": value})
                ext.apply.assert_not_called()

    def test_no_provider_extensions_still_has_none_choice(self):
        module = load()
        page = module["backends_page"]({})
        self.assertEqual(page["items"][0]["options"], [{"label": "None (normal fallback)", "value": ""}])
        self.assertEqual(page["items"][0]["value"], "")
