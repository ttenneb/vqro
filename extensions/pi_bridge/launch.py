"""Trusted isolated host launcher; negotiate optional testing before exactly one bridge launch."""
import importlib.util
import os
from pathlib import Path
import stat
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


def exec_target(binary):
    """Follow execvp's PATH-only order, not shutil.which's Windows cwd/PATHEXT policy.

    Windows CRT exec checks the literal name, then .com/.exe/.bat/.cmd (not PATHEXT).
    Inaccessible/unsupported candidates are ambiguous: keep ordinary execvp, without testing.
    """
    suffixes = ("",)
    if os.name == "nt" and not os.path.splitext(binary)[1] and not binary.endswith("."):
        suffixes = ("", ".com", ".exe", ".bat", ".cmd")
    directories = [""] if os.path.dirname(binary) else os.get_exec_path()
    for directory in directories:
        for suffix in suffixes:
            candidate = os.path.abspath(os.path.join(directory, binary + suffix))
            try:
                metadata = os.stat(candidate)
            except (FileNotFoundError, NotADirectoryError):
                continue
            except OSError:
                return None
            if not stat.S_ISREG(metadata.st_mode) or (os.name != "nt" and not os.access(candidate, os.X_OK)):
                return None
            # CreateProcess/CRT spelling for an extensionless Windows file is not a stable probe identity.
            if os.name == "nt" and not os.path.splitext(candidate)[1]:
                return None
            return candidate
    return None


def supports_testing(binary):
    """Any optional-probe failure, including cleanup, is cached unsupported, never a launch veto."""
    key = (binary, None)
    supported, issues = False, []
    process = reader = None
    output = bytearray()
    try:
        path = os.path.abspath(binary) if binary is not None and os.path.dirname(binary) else None
        if path is not None:
            key = (path, os.stat(path).st_mtime_ns)
        if key in _HELP:
            return _HELP[key]
        if path is None:
            _HELP[key] = False
            _HELP_REASONS[binary] = "bridge executable identity unverified; help not probed"
            return False
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


def negotiated(arguments, binary=None):
    # Only the wrapper's dedicated field before -- is optional. Native argv is never rewritten/replayed.
    before, separator, after = list(arguments), [], []
    if "--" in before:
        index = before.index("--")
        before, separator, after = before[:index], ["--"], before[index + 1:]
    index = next((i for i, arg in enumerate(before) if arg == "--testing-plugin" or arg.startswith("--testing-plugin=")), None)
    if index is not None:
        width = 1 if "=" in before[index] else 2
        added = before[index:index + width]
        unrepresentable = os.name == "nt" and (len(added) != width or any("\0" in arg for arg in added))
        if unrepresentable:
            _HELP_REASONS[binary] = "optional testing argv cannot be represented by Windows CRT; testing field omitted"
        if not unrepresentable and supports_testing(binary):
            return [*before, *separator, *after]
        del before[index:index + width]
        if binary not in _WARNED:
            _WARNED.add(binary)
            try:
                print("NOT VERIFIED: " + _HELP_REASONS.get(binary, "bridge testing support unverified") + "; launching without testing hooks", file=sys.stderr, flush=True)
            except Exception:
                pass  # Best-effort diagnostic only: never veto launch, claim delivery, or use protocol stdout.
    return [*before, *separator, *after]


def main():
    binary = "vqro-bridge-pi"
    arguments = list(sys.argv[1:])
    prefix = arguments[:arguments.index('--')] if '--' in arguments else arguments
    if os.environ.get('VQRO_PI_ACCOUNT') and not any(value == '--account-extension' or value.startswith('--account-extension=') for value in prefix):
        # Required named-account binding resolves on the execution host; normal
        # login adds no mandatory flag to an older bridge's optional-testing path.
        plugin = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'pi_accounts', 'index.mjs')
        arguments = ['--account-extension', plugin, *arguments]
    before = arguments[:arguments.index("--")] if "--" in arguments else arguments
    requested = any(arg == "--testing-plugin" or arg.startswith("--testing-plugin=") for arg in before)
    target = None
    if requested:
        try:
            target = exec_target(binary)
        except Exception:
            pass  # Ambiguous selection cannot authorize a help probe or an optional field.
    try:
        argv = [binary, *negotiated(arguments, target)]
        if os.name == "nt":
            # execv/execvp pass raw strings to CRT, unlike subprocess.Popen's argv encoding.
            argv = [subprocess.list2cmdline([argument]) for argument in argv]
        if target is None:
            os.execvp(binary, argv)  # Original lookup, no optional field, no speculative probe.
        else:
            os.execv(target, argv)  # Probe and ordinary launch are bound to this SAME explicit path.
    except OSError as error:
        raise SystemExit("Cannot start vqro-bridge-pi on the terminal host: " + str(error)
                         + ". Install the matching bridge or configure bridge_argv explicitly."
                         + " The legacy Python entry is pi_bridge/python_launch.py.") from None


if __name__ == "__main__":
    main()
