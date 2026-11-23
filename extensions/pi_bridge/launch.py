"""Trusted isolated host launcher; negotiate optional testing before exactly one bridge launch."""
import os
import shutil
import subprocess
import sys
import threading

_HELP = {}
_WARNED = set()


def supports_testing(binary):
    """One bounded read-only help probe per resolved executable + mtime in this launcher."""
    path = shutil.which(binary)
    if path is None:
        return False
    try:
        stat = os.stat(path) if path else None
        key = (path, stat.st_mtime_ns if stat else None)
    except OSError:
        return False
    if key in _HELP:
        return _HELP[key]
    supported = False
    process = None
    reader = None
    output = bytearray()
    try:
        process = subprocess.Popen([path, "--help"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL)
        def read():
            try:
                output.extend(process.stdout.read(16385))
                if len(output) > 16384:
                    process.kill()
            except OSError:
                pass
            finally:
                process.stdout.close()
        reader = threading.Thread(target=read, daemon=True)
        reader.start()
        process.wait(timeout=2)
        reader.join(timeout=.1)
        supported = not reader.is_alive() and process.returncode == 0 and len(output) <= 16384 and any(line.split()[:1] == [b"--testing-plugin"] for line in output.splitlines())
    except (OSError, subprocess.TimeoutExpired):
        if process is not None:
            process.kill()
            process.wait(timeout=1)
    finally:
        if process is not None and (reader is None or not reader.is_alive()):
            process.stdout.close()
    _HELP[key] = supported
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
            print("NOT VERIFIED: bridge does not advertise --testing-plugin; launching without testing hooks", file=sys.stderr)
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
