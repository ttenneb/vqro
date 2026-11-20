"""Run K2's coordination extension (branch coord-ext) against a real vqro core that lacks C1/C2 yet.

Fixture only, two documented substitutions:
- the protected operational domain (C1) becomes K2's own in-memory MemoryStore;
- the callback principal (C2) is supplied by this fixture: `fixture.principal {author, principal}` binds a
  connection's author to a principal; unbound callers get $COORD_DEFAULT_PRINCIPAL (the plugin's principal).
Everything else is K2's code unchanged: commands `coord.*`, policy, grants, gates, pull, wake on `coord-status`.

  VQRO_SOCKET=... COORD_EXT=<coord-ext>/extensions python3 tests/fixtures/coord_k2.py
"""
import os
import sys

sys.path.insert(0, os.environ["COORD_EXT"])
import coordination  # noqa: E402
from coordination_store import MemoryStore  # noqa: E402
from vqro_ext import Extension  # noqa: E402


class FixtureStore(MemoryStore):
    """C1 stand-in: K2's own deterministic fake, registered implicitly."""
    def __init__(self, rpc):
        super().__init__()

    def register(self):
        return None


PRINCIPALS = {}
DEFAULT = os.environ.get("COORD_DEFAULT_PRINCIPAL", "p:plugin")


def callback_context(params):
    """C2 stand-in: principal by the invoking connection's author, never from args."""
    author = params.get("caller") or params.get("author")
    principal = PRINCIPALS.get(author, DEFAULT)
    return {"principal": principal, "actor": author or "unknown", "author": author or "unknown", "kind": "tool"}


coordination.OperationalStore = FixtureStore
coordination.callback_context = callback_context
ext = Extension("coord", event_types=["entries", "resource", "resource.removed", "provider", "missed", "operational"])


@ext.command("fixture.principal", "fixture: bind a connection author to a principal (C2 stand-in)")
def bind(args):
    PRINCIPALS[args["author"]] = args["principal"]
    return {"bound": args}


coordination.CoordinationService(ext).run()
