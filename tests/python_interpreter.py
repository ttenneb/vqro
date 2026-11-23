"""Import-safe test interpreter adapter. Selection happens only on an explicit call."""
from functools import lru_cache


@lru_cache(maxsize=1)
def python():
    import importlib.util
    from pathlib import Path
    path = Path(__file__).resolve().parents[1] / "scripts/test_python.py"
    spec = importlib.util.spec_from_file_location("vqro_test_python", path)
    resolver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(resolver)
    return resolver.resolve()
