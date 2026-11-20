"""Trusted host launcher: python -I launch.py execs the Rust Pi bridge, never project imports."""
import os
import sys


def main():
    try:
        os.execvp("vqro-bridge-pi", ["vqro-bridge-pi", *sys.argv[1:]])
    except OSError as error:
        raise SystemExit("Cannot start vqro-bridge-pi on the terminal host: " + str(error)
                         + ". Install the matching bridge or configure bridge_argv explicitly."
                         + " The legacy Python entry is pi_bridge/python_launch.py.") from None


if __name__ == "__main__":
    main()
