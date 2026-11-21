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
                features = case.get("features") or model.BASE_FEATURES
                got = [{"id": r["id"], "depth": r["depth"], "text": r["item"].get("text"), "type": model.kind(r["item"]),
                        "has_children": r["has_children"], "expanded": r["expanded"], "pinned": r["pinned"],
                        "selectable": model.selectable(r["item"]), "bottom": r.get("bottom", False),
                        "chips": [c.get("text") for c in r["item"]["chips"]] if "chips" in r["item"] else None}
                       for r in model.rows(p, s.get("page", 0), s.get("filter", ""), s.get("open"), features, s.get("facets"), s.get("sort"))]
                enhancement = None
                if isinstance(p.get("list_detail"), dict):
                    enhancement = not model.missing_for(p["list_detail"], model.BASE_FEATURES)
                want = {"rows": got, "missing": model.missing_for(p, model.BASE_FEATURES), "list_detail": enhancement}
                looks = [{"id": r["id"], "card": c, "frame": f} for r in model.rows(p, s.get("page", 0), s.get("filter", ""), s.get("open"), features, s.get("facets"), s.get("sort"))
                         if model.kind(r["item"]) == "message" for c, f in [model.message_look(r["item"], features)]]
                if looks:
                    want["looks"] = looks
                if "search" in s:
                    want["search"] = [{"id": i, "open": o} for i, o in model.search(p, s["search"], s.get("page", 0), s.get("filter", ""), case.get("features") or model.BASE_FEATURES)]
                counts = model.facet_counts(p, s.get("page", 0), s.get("filter", ""), s.get("facets"), features)
                if counts:
                    want["facets"] = counts
                label = model.sort_label(p, s.get("page", 0), s.get("sort")) if "sort" in features else None
                if label:
                    want["sort"] = label
                spec = model.declared(p, s.get("page", 0), "columns")
                if spec and "columns" in features and "width" in case:
                    want["column_widths"] = model.column_widths(spec, model.rows(p, s.get("page", 0), s.get("filter", ""), s.get("open"), features, s.get("facets"), s.get("sort")), case["width"])
                self.assertEqual(want, case["expect"])

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
                got = {"menus": menus, "keys": keys, "gestures": gestures}
                if "scoped" in case:
                    got["scoped"] = [(lambda b: b.get("command") if b else None)(ws.scoped_binding(k, sc, ctx)) for k, sc, ctx in case["scoped"]]
                self.assertEqual(got, case["expect"])

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

    def test_markdown(self):
        for case in fixture("markdown.json")["cases"]:
            with self.subTest(case["name"]):
                got = [[list(span) for span in line] for line in model.markdown_lines(case["text"], case["width"])]
                self.assertEqual(got, case["expect"])

    def test_composer(self):
        for case in fixture("composer.json")["cases"]:
            with self.subTest(case["name"]):
                row, c, outcomes, args = case["row"], None, [], None
                c = model.Composer(row)
                for key in case["keys"]:
                    if key.startswith("paste:"):
                        outcome = c.editor.insert(key[6:])
                    elif key.startswith("set:"):
                        k, v = key[4:].split("=", 1)
                        c.values[k] = v
                        outcome = "edit"
                    else:
                        outcome = c.editor.key(key)
                    if outcome == "submit":
                        args = c.submit_args(row)
                    outcomes.append(outcome)
                self.assertEqual({"outcomes": outcomes, "text": c.editor.string(), "cursor": c.editor.cursor, "args": args}, case["expect"])

    def test_tab_mark(self):
        for case in fixture("tabmark.json")["cases"]:
            self.assertEqual(model.tab_mark(case.get("setting"), case["now"], case["seen"], case["reached"]), case["expect"], case["name"])

    def test_snapshot_adoption_keeps_a_covered_finish(self):
        """Review P2: working, a snapshot already showing done, then the covered (skipped) event: still marked."""
        snap = lambda seq, rs: {"epoch": "e", "seq": seq, "entries": {}, "resources": rs}
        r1 = lambda state: [{"id": "r1", "state": "live", "status": {"state": state}}]
        ws = model.Workspace(); ws.load(snap(0, r1("working")))
        ws.load(snap(2, r1("done")))
        ws.apply({"epoch": "e", "seq": 1, "type": "resource", "data": r1("done")[0]})
        self.assertEqual(ws.reached.get("r1"), "done")
        fresh = model.Workspace(); fresh.load(snap(5, r1("done")))
        self.assertEqual(fresh.reached, {}, "a first load latches nothing")
        ws.load(snap(3, []))
        self.assertEqual(ws.reached, {}, "a snapshot without the resource prunes its finish")

    def test_tab_latch(self):
        for case in fixture("tablatch.json")["cases"]:
            ws = model.Workspace()
            for step in case["steps"]:
                ws.load(step["load"]) if "load" in step else ws.apply(step["event"])
            self.assertEqual(ws.reached, case["expect"], case["name"])

    def test_tab_reached(self):
        for case in fixture("tabreached.json")["cases"]:
            self.assertEqual(model.reached(case["old"], case["new"]), case["expect"], case["name"])

    def test_tab_state(self):
        for case in fixture("tabstate.json")["cases"]:
            self.assertEqual(model.tab_state(case["tab"]), case["expect"], case["name"])

    def test_tab_title(self):
        for case in fixture("tabtitle.json")["cases"]:
            self.assertEqual(model.tab_title(case.get("setting"), case["title"], case["resource"]), case["expect"], case["name"])

    def test_tab_bar(self):
        for case in fixture("tabbar.json")["cases"]:
            self.assertEqual(model.tab_bar(case.get("setting"), case["titles"], case["active"]), case["expect"], case["name"])

    def test_key_names_follow_the_spec(self):
        for raw, want in (("C-space", "C-Space"), ("Ctrl-Shift-Left", "C-S-Left"), ("C-Q", "C-q"), ("S-a", None), ("Hyper-x", None),
                          ("f12", "F12"), ("F13", None), ("C--", "C--"), ("M-x", "M-x"), (" ", "Space"), ("BackTab", "BackTab")):
            self.assertEqual(model.normalize(raw), want, raw)


if __name__ == "__main__":
    unittest.main()
