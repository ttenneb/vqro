#!/usr/bin/env python3
"""prim extension: gestures. Right-click opens the pane or tab menu; middle-click closes a tab.

Only bindings: clients own mouse mechanics (and whether the mouse is captured at all, a client
setting), this decides what gestures mean. Remove it and these gestures do nothing; left-click
focus, tabs, buttons, dragging and the wheel stay, since those are client mechanics.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension  # noqa: E402

ext = Extension("gestures", event_types=["entries"])  # no resource events: not used here
ext.contribute({
    "binding/gestures.pane-menu": {"key": "mouse.right", "where": "pane", "command": "client.menu", "args": {"where": "pane"}, "label": "pane menu"},
    "binding/gestures.tab-menu": {"key": "mouse.right", "where": "tab", "command": "client.menu", "args": {"where": "tab"}, "label": "tab menu"},
    "binding/gestures.tab-close": {"key": "mouse.middle", "where": "tab", "command": "tabs.close", "label": "close tab"},
})
ext.run()
