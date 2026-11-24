"""Trusted isolated host launcher; negotiate optional testing before exactly one bridge launch."""
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading

_HELP = {}
_HELP_REASONS = {}
_WARNED = set()


def _background_kwargs():
    # -I excludes project/PYTHONPATH imports. Load only this installation's shared helper.
    source = Path(__file__).resolve().parent.parent / "vqro_process.py"
    spec = importlib.util.spec_from_file_location("_vqro_bridge_process", source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.background_kwargs()


def supports_testing(binary):
    """Any optional-probe failure, including cleanup, is cached unsupported, never a launch veto."""
    key = (binary, None)
    supported, issues = False, []
    process = reader = None
    output = bytearray()
    try:
        path = shutil.which(binary)
        if path is not None:
            key = (path, os.stat(path).st_mtime_ns)
        if key in _HELP:
            return _HELP[key]
        if path is None:
            raise FileNotFoundError("bridge executable unavailable for help probe")
        try:
            process = subprocess.Popen([path, "--help"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, **_background_kwargs())
            def read():
                try:
                    output.extend(process.stdout.read(16385))
                    if len(output) > 16384:
                        process.kill()
                except Exception as error:
                    issues.append("help output unavailable (" + type(error).__name__ + ")")
                finally:
                    try:
                        process.stdout.close()
                    except Exception as error:
                        issues.append("help output cleanup failed (" + type(error).__name__ + ")")
            reader = threading.Thread(target=read, daemon=True)
            reader.start()
            process.wait(timeout=2)
            reader.join(timeout=.1)
            supported = not reader.is_alive() and process.returncode == 0 and len(output) <= 16384 and any(line.split()[:1] == [b"--testing-plugin"] for line in output.splitlines())
        except Exception as error:
            issues.append("help probe failed (" + type(error).__name__ + ")")
        finally:
            if process is not None:
                if process.returncode is None:
                    try:
                        process.kill()
                    except Exception as error:
                        issues.append("help probe kill failed; cleanup unconfirmed (" + type(error).__name__ + ")")
                    try:
                        process.wait(timeout=1)
                    except Exception as error:
                        issues.append("help probe reap unconfirmed (" + type(error).__name__ + ")")
                if reader is None or not reader.is_alive():
                    try:
                        process.stdout.close()
                    except Exception as error:
                        issues.append("help output cleanup failed (" + type(error).__name__ + ")")
    except Exception as error:
        # Also covers lookup, setup, and unexpected cleanup failures. No actual launch has occurred.
        issues.append("help probe unavailable (" + type(error).__name__ + ")")
    supported = supported and not issues
    _HELP[key] = supported
    _HELP_REASONS[binary] = "; ".join(issues) or "bridge does not advertise --testing-plugin"
    return supported


def negotiated(arguments, binary="vqro-bridge-pi"):
    # Only the wrapper's dedicated field before -- is optional. Native argv is never rewritten/replayed.
    before, separator, after = list(arguments), [], []
    if "--" in before:
        index = before.index("--")
        before, separator, after = before[:index], ["--"], before[index + 1:]
    index = next((i for i, arg in enumerate(before) if arg == "--testing-plugin" or arg.startswith("--testing-plugin=")), None)
    if index is not None and not supports_testing(binary):
        width = 1 if "=" in before[index] else 2
        del before[index:index + width]
        if binary not in _WARNED:
            _WARNED.add(binary)
            try:
                print("NOT VERIFIED: " + _HELP_REASONS.get(binary, "bridge testing support unverified") + "; launching without testing hooks", file=sys.stderr, flush=True)
            except Exception:
                pass  # Best-effort diagnostic only: never veto launch, claim delivery, or use protocol stdout.
    return [*before, *separator, *after]


def main():
    try:
        os.execvp("vqro-bridge-pi", ["vqro-bridge-pi", *negotiated(sys.argv[1:])])
    except OSError as error:
        raise SystemExit("Cannot start vqro-bridge-pi on the terminal host: " + str(error)
                         + ". Install the matching bridge or configure bridge_argv explicitly."
                         + " The legacy Python entry is pi_bridge/python_launch.py.") from None


if __name__ == "__main__":
    main()
