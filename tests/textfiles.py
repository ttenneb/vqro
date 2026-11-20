"""Small file helpers for the tier-1 test scripts: every file is closed before the call returns, so the scripts
run clean under `python3 -X dev -W error::ResourceWarning`."""
import json


def read_text(path):
    with open(path, encoding="utf-8") as file:
        return file.read()


def read_lines(path):
    with open(path, encoding="utf-8") as file:
        return file.read().splitlines(keepends=True)


def read_json(path):
    with open(path, encoding="utf-8") as file:
        return json.load(file)


def write_text(path, text):
    with open(path, "w", encoding="utf-8") as file:
        file.write(text)


def write_json(path, value, **options):
    with open(path, "w", encoding="utf-8") as file:
        json.dump(value, file, **options)
