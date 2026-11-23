#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Test-only real supervised extension; every identity/proof is assigned by its private core.

Like tests/fixtures/managed_identity.py it holds a genuine supervisor lifetime. Unlike a
raw reserved hello, its SDK uses that lifetime token and handles actual command callbacks.
No coordination target engine/store is imported.
"""
import os
import threading
from vqro_ext import Vqro, VqroError

NAME = "partb-identity-probe"
COMMAND = "partb.identity_probe"
released = threading.Event()
last_proof = None
hello = None
client = None


def write(key, value=True, **fields):
    return client.call("workspace.apply", {"ops": [{"op": "set", "key": key, "value": value}],
                                           "note": "owned managed identity probe", **fields})


def outside_callback(function):
    """A fresh thread has no SDK callback-local invocation proof."""
    result = {}
    def run():
        try: result["value"] = function()
        except VqroError as error: result["error_kind"] = error.kind
        except Exception as error: result["error_type"] = type(error).__name__
    thread = threading.Thread(target=run)
    thread.start(); thread.join(4)
    if thread.is_alive(): raise VqroError("owned autonomous probe exceeded deadline", "timeout")
    return result


def handle(method, params):
    global last_proof
    if method != "command.invoke": return None
    if params.get("name") != COMMAND: raise VqroError("unknown owned probe command", "not_found")
    args = params.get("args") or {}
    action = args.get("action", "observe")
    frame = {key: params.get(key) for key in ("caller", "author", "actor", "principal", "initiator", "invocation")}
    if action == "observe":
        return {"hello": hello, "callback": frame,
                "launch_principal": os.environ.get("VQRO_PRINCIPAL"),
                "inherited_identity": {key: key in os.environ for key in ("VQRO_PRINCIPAL", "VQRO_IDENTITY", "VQRO_HOST_TOKEN")},
                "managed_token_present": bool(os.environ.get("VQRO_EXTENSION_TOKEN"))}
    if action == "release":
        released.set(); return {"released": True}
    if action == "write":
        last_proof = params.get("invocation")
        return {"transaction": write(args["key"], args.get("value", True)), "callback": frame, "hello": hello}
    if action == "coord_call":
        return {"result": client.call("command.invoke", {"name": "coord." + args["command"], "args": args["payload"]}),
                "callback": frame, "hello": hello}
    if action == "autonomous":
        return outside_callback(lambda: write(args["key"]))
    if action == "autonomous_undo":
        return outside_callback(lambda: client.call("command.invoke", {"name": "vqro.revert", "args": {}}))
    if action == "expired":
        return outside_callback(lambda: write(args["key"], invocation=last_proof))
    if action == "hold":
        released.clear()
        last_proof = params.get("invocation")
        # This is the actual forwarded public envelope, never a fabricated proof or identity.
        write(args["key"], frame)
        if not released.wait(4): raise VqroError("owned callback barrier not released", "timeout")
        return {"released": True, "callback": frame}
    raise VqroError("unknown probe action", "invalid")


client = Vqro(os.environ["VQRO_SOCKET"], handle)
client.name = NAME
hello = client.call("hello", {"extension": os.environ["VQRO_EXTENSION"],
                              "extension_token": os.environ["VQRO_EXTENSION_TOKEN"]})
client.call("command.register", {"name": COMMAND, "title": "Owned identity witness"})
threading.Event().wait()
