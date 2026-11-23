"""Process options for local, non-interactive stdio helpers (not terminal/UI launches).

A console-less Windows extension's child can allocate a new console even with all
standard streams redirected. Apply this at each background subprocess boundary;
CREATE_NO_WINDOW is not inherited by grandchildren. Unix options stay unchanged.
"""
import os
import subprocess


def background_kwargs():
    """Extra kwargs for subprocess/asyncio launches that must not open a console."""
    return {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {}
