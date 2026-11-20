"""The Python client model (extensions/clients/vqro_client_model.py) against the shared base-profile fixtures:
the second implementation of the client contract (D29). The Rust model passes the same files
(tests/client_conformance.rs)."""
import json
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "extensions", "clients"))
import vqro_client_model as model  # noqa: E402


def fixture(name):
    with open(os.path.join(ROOT, "tests", "fixtures", "client", name), encoding="utf-8") as f:
        return json.load(f)


def workspace(snapshot):
    ws = model.Workspace()
    ws.load(snapshot)
    return ws


class Conformance(unittest.TestCase):
    def test_panels(self):
        doc = fixture("panels.json")
        self.assertEqual(doc["base_features"], model.BASE_FEATURES)
        for case in doc["cases"]:
            with self.subTest(case["name"]):
                s, p = case["state"], case["panel"]
                got = [{"id": r["id"], "depth": r["depth"], "text": r["item"].get("text"), "type": model.kind(r["item"]),
                        "has_children": r["has_children"], "expanded": r["expanded"], "pinned": r["pinned"],
                        "selectable": model.selectable(r["item"]),
                        "chips": [c.get("text") for c in r["item"]["chips"]] if "chips" in r["item"] else None}
                       for r in model.rows(p, s.get("page", 0), s.get("filter", ""), s.get("open"))]
                enhancement = None
                if isinstance(p.get("list_detail"), dict):
                    enhancement = not model.missing_for(p["list_detail"], model.BASE_FEATURES)
                self.assertEqual({"rows": got, "missing": model.missing_for(p, model.BASE_FEATURES), "list_detail": enhancement}, case["expect"])

    def test_contributions(self):
        for case in fixture("contributions.json")["cases"]:
            with self.subTest(case["name"]):
                ws = workspace(case["snapshot"])
                menus = {place: [e.get("label") for e in ws.menu_entries(place, ctx)] for place, ctx in case["places"].items()}
                cands = ws.key_candidates(case["ctx"])
                keys = {k: (None if cands[i][1].get("command") == "client.none" else cands[i][0]) for k, i in model.resolve(cands).items()}
                gestures = {}
                for key, place in case["gesture_queries"]:
                    b = ws.gesture(key, place)
                    gestures[f"{key} {place}"] = b.get("command") if b else None
                self.assertEqual({"menus": menus, "keys": keys, "gestures": gestures}, case["expect"])

    def test_recovery(self):
        for case in fixture("recovery.json")["cases"]:
            with self.subTest(case["name"]):
                ws = workspace(case["snapshot"])
                prefix = [{"key": k, "label": label, "command": b.get("command"), "args": b.get("args")} for k, label, b in model.prefix_rows(ws, case["ctx"])]
                chooser = model.safe_chooser(ws, set(case.get("selected") or [])) if ws.safe_mode else None
                self.assertEqual({"prefix": prefix, "safe_mode_chooser": chooser}, case["expect"])

    def test_window(self):
        for case in fixture("window.json")["cases"]:
            with self.subTest(case["name"]):
                ws, win, got = workspace(case["snapshot"]), model.Window(), []
                for step in case["steps"]:
                    do = step["do"]
                    if do == "start":
                        out = {"settled": win.settle(ws)}
                    elif do == "event":
                        ws.apply(step["event"])
                        out = {"settled": win.settle(ws)}
                    elif do in ("show", "client.layout"):
                        ok, r = win.show(ws, step["layout"]) if do == "show" else win.client_layout(ws, step["args"])
                        out = {"ok": r} if ok else {"error": r}
                    elif do == "reply":
                        hinted = model.reply_layout(step["command"], step["reply"])
                        if hinted:
                            win.show(ws, hinted)
                        out = {"reply_layout": hinted}
                    elif do == "invoke":
                        out = {"ctx": win.invocation_context(ws, step["ctx"])}
                    elif do == "context":
                        out = {"context": win.context(ws)}
                    elif do == "mark":
                        out = {"panel": model.mark_current(step["panel"], win.shown(ws))}
                    out["shown"] = win.shown(ws)
                    got.append(out)
                self.assertEqual(got, case["expect"])

    def test_views(self):
        doc = fixture("views.json")
        for case in doc["cases"]:
            with self.subTest(case["name"]):
                shown, chain = model.shown_view(case["view"], lambda v: model.merged(v, doc["entries"], doc["live"]),
                                                lambda v: model.renders(v, doc["kinds"], model.BASE_FEATURES))
                self.assertEqual({"shown": shown, "chain": chain}, case["expect"])

    def test_key_names_follow_the_spec(self):
        for raw, want in (("C-space", "C-Space"), ("Ctrl-Shift-Left", "C-S-Left"), ("C-Q", "C-q"), ("S-a", None), ("Hyper-x", None),
                          ("f12", "F12"), ("F13", None), ("C--", "C--"), ("M-x", "M-x"), (" ", "Space"), ("BackTab", "BackTab")):
            self.assertEqual(model.normalize(raw), want, raw)


if __name__ == "__main__":
    unittest.main()
