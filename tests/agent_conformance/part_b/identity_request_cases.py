# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""G9/G14 and Q1-Q4: actual managed actors and public request routing witnesses.

Only public RPC and private supervised fixtures are used; no coordination engine or
MemoryStore is imported. The pin and native ledger belong to PublicCoordination.
"""
import asyncio
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time

from ..results import CheckFailed, FixtureFailed, HarnessGap
from ..wire import Peer, RpcError
from .cases import case, member
from .context import PublicCoordination
from .launcher import tui
from .identity_backend import backend_child_environment, pinned_artifact
from .request_routing_binding import request_routing_case
from pi_provider_e2e import identity

PROBE_NAME = "partb-identity-probe"
PROBE_COMMAND = "partb.identity_probe"


def managed_principal_is_bound_not_inherited(observation):
    actor = observation.get("hello", {}).get("actor")
    return actor == "extension:" + PROBE_NAME and observation.get("launch_principal") in (None, actor)


def callback_attribution_is_actual(row, hello, principal, initiator):
    return (isinstance(principal, str) and bool(principal) and isinstance(initiator, str) and bool(initiator)
            and isinstance(hello.get("actor"), str) and bool(hello["actor"])
            and isinstance(hello.get("actual_author"), str) and bool(hello["actual_author"])
            and row.get("actor") == hello["actor"] and row.get("author") == hello["actual_author"]
            and row.get("principal") == principal and row.get("initiator") == initiator)


async def probe(ctx, action="observe", *, peer=None, **args):
    return await (peer or ctx.peer).call("command.invoke", {
        "name": PROBE_COMMAND, "args": {"action": action, **args}}, timeout=7)


async def managed_probe(ctx):
    """Install only an owned private manifest, then let the real core supervise it."""
    source = Path(__file__).with_name("identity_request_probe.py")
    destination = ctx.root / "ext" / source.name
    shutil.copyfile(source, destination)
    manifest = {"name": PROBE_NAME, "title": "Owned identity witness", "version": "0.1.0",
        "license": "Apache-2.0", "kind": "interface", "requires": {"protocol": 1},
        "provides": ["command/" + PROBE_COMMAND]}
    (ctx.root / "ext/manifests" / (PROBE_NAME + ".json")).write_text(json.dumps(manifest) + "\n")
    path = ctx.root / "ext/defaults.json"
    defaults = json.loads(path.read_text())
    defaults["extensions"].append({"name": PROBE_NAME, "since": 2, "default": True,
        "kind": "management", "argv": ["/usr/bin/python3", str(destination)]})
    path.write_text(json.dumps(defaults) + "\n")
    # Deliberately tag the launcher, not the child. The supervisor must filter it.
    ctx.backend.env["VQRO_PRINCIPAL"] = "worker:must-not-leak"
    await ctx.restart(crash=False)
    # Defaults are deliberately not reinstalled at an already-recorded version. Activate
    # the new fixture through an actual attributed workspace transaction instead.
    activation = await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/" + PROBE_NAME,
        "value": {"argv": ["/usr/bin/python3", str(destination)], "kind": "management", "enabled": True}, "expect": 0}],
        "note": "activate owned managed identity witness"})
    ctx.evidence.append({"kind": "managed_fixture_activation", "transaction": activation})
    async def ready():
        try: return await probe(ctx)
        except RpcError as error:
            if error.kind == "not_found": return None
            raise
    try: observed = await ctx.wait(ready, label="genuine supervised identity probe")
    except CheckFailed as error: raise FixtureFailed("owned managed identity fixture did not become ready") from error
    ctx.track_children()
    ctx.evidence.append({"kind": "managed_identity_fixture", "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                         "hello": observed.get("hello"), "native_observation": observed})
    return observed


async def seed_invalid_owner_from_historical_core(sub):
    """Existing history_cases.py pattern, but seed by real old RPC with no JSON edits.

    Modern Core correctly validates new owner writes. Invalid legacy configuration
    must originate in an actual hash-pinned pre-validation writer, not a fake error.
    """
    source = pinned_artifact({"path": sub.manifest.get("legacy_core"), "sha256": sub.manifest.get("legacy_core_sha256")})
    await asyncio.gather(*(peer.close() for peers in sub.peers.values() for peer in peers))
    sub.peers.clear()
    sub.track_children()
    await asyncio.to_thread(sub.backend.stop, False)
    state, endpoint = sub.backend.env["VQRO_STATE_DIR"], sub.root / "l.sock"
    env = dict(sub.backend.env, VQRO_SOCKET=str(endpoint))
    peer = None
    with (sub.root / "legacy-owner.log").open("ab") as log:
        process = subprocess.Popen([str(source), state, str(endpoint), "none"], env=env,
            cwd=sub.root / "work", stdout=log, stderr=subprocess.STDOUT)
        sub.backend.tracked.append(identity(process.pid))
        try:
            async def connected():
                if process.poll() is not None: raise FixtureFailed("historical owner-seed core exited")
                try: return await Peer.connect(str(endpoint))
                except OSError: return None
            try: peer = await sub.wait(connected, label="historical native owner writer")
            except CheckFailed as error: raise FixtureFailed("historical owner writer did not become ready") from error
            await peer.call("hello", {"client": "owned-legacy-owner-seed"})
            seeded = await peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/principals/owner", "value": {"invalid": True}}]})
            snapshot = await peer.call("workspace.snapshot")
            if snapshot["entries"]["doc/principals/owner"]["value"] != {"invalid": True}:
                raise FixtureFailed("historical writer did not actually retain malformed owner configuration")
            sub.evidence.append({"kind": "historical_owner_seed", "legacy_sha256": sub.manifest["legacy_core_sha256"],
                "transaction": seeded, "public_owner_entry": snapshot["entries"]["doc/principals/owner"], "handcrafted_state": False})
        finally:
            if peer is not None: await peer.close()
            if process.poll() is None: process.terminate()
            try: await asyncio.to_thread(process.wait, 4)
            except subprocess.TimeoutExpired:
                process.kill(); await asyncio.to_thread(process.wait, 3)
    await sub.start_core()
    await sub.wait_agents(sub.manifest.get("agents", 2))
    async def controls_registered():
        return any(row["name"] == "coord.control" for row in (await sub.peer.call("command.list"))["commands"])
    await sub.wait(controls_registered, label="current managed control after historical owner import")


async def unowned_fixture(ctx, label, *, invalid=False):
    # Use normal production core plus an authorized public configuration transaction,
    # not a driver flag whose bootstrap behaviour differs across frozen fixture cuts.
    # Keep the private socket below the portable 103-byte bound, even under a run root.
    root = ctx.root / ("i" if invalid else "m")
    if len(str(root / "p.sock").encode()) > 103:
        raise FixtureFailed("G14 subfixture socket exceeds the portable path bound")
    sub = PublicCoordination(root, ctx.manifest, ctx.vqro)
    try:
        await sub.start()
        owner = sub.peer
        entry = (await owner.call("workspace.snapshot"))["entries"]["doc/principals/owner"]
        if invalid:
            await seed_invalid_owner_from_historical_core(sub)
        else:
            await owner.call("workspace.apply", {"ops": [{"op": "delete", "key": "doc/principals/owner", "expect": entry["version"]}],
                "note": "owned missing owner configuration probe"})
        sub.peer = await sub.connect(None)
        return sub
    except BaseException:
        await sub.close()
        raise


@case("G14")
async def reserved_identities_and_unowned_recovery(ctx):
    for principal in ("extension:not-a-managed-extension", "host:not-a-host", "system:not-a-system"):
        peer = await Peer.connect(ctx.backend.env["VQRO_SOCKET"])
        try:
            try: await peer.call("hello", {"client": "reserved-negative", "identity": {"kind": "principal", "principal": principal}})
            except RpcError as error:
                ctx.check(error.kind == "invalid", "raw reserved assertion rejects; it never becomes fixture authority", component="core",
                          asserted=principal, refusal=error.kind)
            else: raise CheckFailed("core accepted an unverified reserved identity", component="core")
        finally: await peer.close()
    observation = await managed_probe(ctx)
    actor = observation.get("hello", {}).get("actor")
    ctx.check(actor == "extension:" + PROBE_NAME and observation.get("managed_token_present"),
              "managed actor comes from a genuine supervisor lifetime", component="core", observation=observation)
    ctx.check(managed_principal_is_bound_not_inherited(observation),
              "supervised child has no caller principal; a core-assigned own principal is not inheritance", component="core", observation=observation)
    await backend_child_environment(ctx)
    for label, invalid in (("missing-owner", False), ("invalid-owner", True)):
        sub = await unowned_fixture(ctx, label, invalid=invalid)
        try:
            snapshot = await sub.peer.call("workspace.snapshot")
            ctx.check(("doc/principals/owner" not in snapshot["entries"]) if not invalid else
                      snapshot["entries"]["doc/principals/owner"]["value"] == {"invalid": True},
                      "authorized missing/invalid owner configuration remains observable", component="core", fixture=label)
            raw = await sub.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/part-b/raw-recovery", "value": True}]})
            ctx.check(raw.get("txn"), "raw recovery remains usable without an owner", component="core", fixture=label)
            mark = sub.native.mark()
            await sub.rejected("control", {"op": await sub.operation(), "target": sub.agents[0]["id"],
                "action": "interrupt", "args": {"v": 0}}, {"no_grant", "invalid", "forbidden"})
            ctx.check(not sub.native.records("invoke", after=mark), "unowned raw client has no coordination grants or native dispatch", fixture=label)
            owner_peer = await Peer.connect(sub.backend.env["VQRO_SOCKET"])
            try:
                try: hello = await owner_peer.call("hello", {"client": "owner-unset", "identity": {"kind": "owner"}})
                except RpcError as error:
                    ctx.check(error.kind in ("invalid", "owner_unset", "owner-unset"), "owner-unset is an explicit refusal", component="core", fixture=label)
                else:
                    ctx.check(hello.get("actor") == "owner-unset" and hello.get("principal") == "owner-unset",
                              "missing/invalid configuration never guesses an owner", component="core", fixture=label, hello=hello)
                    mark = sub.native.mark()
                    await sub.rejected("control", {"op": await sub.operation(), "target": sub.agents[0]["id"],
                        "action": "interrupt", "args": {"v": 0}}, {"no_grant", "invalid", "forbidden"}, peer=owner_peer)
                    ctx.check(not sub.native.records("invoke", after=mark), "owner-unset handshake confers no coordination grants", fixture=label)
            finally: await owner_peer.close()
            async with tui(sub, "owner-unset-tui") as ui:
                # Verified 7d7c382 src/tui/mod.rs:3099; state id uses a hyphen,
                # rendered warning uses a space. Wait for actual frames, not a delay.
                try:
                    await ui.text(b"OWNER UNSET")
                    await ui.text(b"bootstrap")
                except FixtureFailed as error:
                    raise CheckFailed("actual TUI lacks visible owner-unset bootstrap recovery", component="core") from error
                ctx.check(True, "actual TUI presents owner-unset and bootstrap recovery", component="core", fixture=label,
                          frame=bytes(ui.output[-4096:]).decode(errors="replace"))
        finally:
            await sub.close()
            ctx.evidence.append({"kind": "owner_recovery_fixture", "fixture": label, "evidence": sub.evidence})


async def transaction(ctx, txn):
    rows = (await ctx.peer.call("workspace.history", {"limit": 10000}))["txns"]
    return next(row for row in rows if row["id"] == txn)


async def refuses_public(ctx, peer, method, params, message):
    try: await peer.call(method, params)
    except RpcError as error:
        ctx.check(error.kind in ("invalid", "forbidden", "no_grant"), message, component="core", refusal=error.kind)
        return error
    raise CheckFailed(message + ": accepted unexpectedly", component="core")


@case("G9")
async def delegated_managed_callbacks_preserve_actor_and_initiator(ctx):
    observed = await managed_probe(ctx)
    actor = observed.get("hello", {}).get("actor")
    ctx.check(actor == "extension:" + PROBE_NAME, "callback witness is an actual supervised extension", component="core", observation=observed)
    owner = observed["callback"]["principal"]
    key = "doc/part-b/g9-owner-callback"
    result = await probe(ctx, "write", key=key)
    row = await transaction(ctx, result["transaction"]["txn"])
    ctx.check(callback_attribution_is_actual(row, observed["hello"], owner, owner),
              "managed callback writes actual E author/actor with owner principal and initiator", component="core", transaction=row)
    await ctx.peer.call("command.invoke", {"name": "vqro.revert", "args": {}})
    ctx.check(key not in (await ctx.peer.call("workspace.snapshot"))["entries"], "owner undo selects its managed callback write", component="core")
    autonomous_key = "doc/part-b/g9-autonomous"
    autonomous = await probe(ctx, "autonomous", key=autonomous_key)
    ctx.check("value" in autonomous, "autonomous extension write actually completes", component="core", result=autonomous)
    autonomous_row = await transaction(ctx, autonomous["value"]["txn"])
    ctx.check(autonomous_row.get("principal") == actor and autonomous_row.get("actor") == actor,
              "later autonomous E write carries no expired owner standing", component="core", transaction=autonomous_row)
    own_undo = await probe(ctx, "autonomous_undo")
    ctx.check("value" in own_undo and autonomous_key not in (await ctx.peer.call("workspace.snapshot"))["entries"],
              "actual autonomous E undo selects E, not the earlier owner", component="core", result=own_undo)

    worker_id = "worker:g9"
    worker = await ctx.connect(worker_id, label=owner)
    target = "doc/part-b/g9-delegated"
    write = {"ops": [{"op": "set", "key": target, "value": True}], "on_behalf_of": owner, "label": owner}
    await refuses_public(ctx, worker, "workspace.apply", write, "label/on_behalf_of without delegation confers no authority")
    direct = "doc/principals/delegations/g9-direct"
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": direct, "value": {"from": owner, "to": worker_id}}]})
    delegated = await worker.call("workspace.apply", write)
    row = await transaction(ctx, delegated["txn"])
    ctx.check(row.get("actor") == worker_id and row.get("initiator") == worker_id and row.get("principal") == owner,
              "direct delegation keeps actual worker actor/initiator and effective owner", component="core", transaction=row)
    await ctx.peer.call("command.invoke", {"name": "vqro.revert", "args": {}})
    ctx.check(target not in (await ctx.peer.call("workspace.snapshot"))["entries"], "effective owner undo selects W's represented write", component="core")

    combined_key = "doc/part-b/g9-combined"
    combined = await worker.call("command.invoke", {"name": PROBE_COMMAND, "args": {"action": "write", "key": combined_key}, "on_behalf_of": owner})
    row = await transaction(ctx, combined["transaction"]["txn"])
    ctx.check(callback_attribution_is_actual(row, observed["hello"], owner, worker_id),
              "W representing P through E preserves E actor, P principal and W initiator separately", component="core", transaction=row)
    try: await worker.call("command.invoke", {"name": "vqro.revert", "args": {}})
    except RpcError as error: ctx.check(error.kind in ("not_found", "invalid", "no_grant"), "W has no represented default undo standing", component="core")
    ctx.check(combined_key in (await ctx.peer.call("workspace.snapshot"))["entries"], "W default undo cannot select effective P's write", component="core")
    await probe(ctx, "autonomous_undo")
    ctx.check(combined_key in (await ctx.peer.call("workspace.snapshot"))["entries"], "E default undo cannot select effective P's write", component="core")
    await ctx.peer.call("command.invoke", {"name": "vqro.revert", "args": {}})
    ctx.check(combined_key not in (await ctx.peer.call("workspace.snapshot"))["entries"], "P default undo selects combined delegated callback write", component="core")
    # A real managed callback holds an actual core-issued proof while another connection misuses it.
    held_key = "doc/part-b/g9-live-proof"
    pending = asyncio.create_task(ctx.peer.call("command.invoke", {"name": PROBE_COMMAND,
        "args": {"action": "hold", "key": held_key}}, timeout=7))
    try:
        async def proof_ready():
            entry = (await ctx.peer.call("workspace.snapshot"))["entries"].get(held_key)
            return entry["value"] if entry and entry["value"].get("invocation") is not None else None
        frame = await ctx.wait(proof_ready, label="actual managed callback proof", timeout=3)
        await refuses_public(ctx, worker, "workspace.apply", {"invocation": frame["invocation"],
            "ops": [{"op": "set", "key": "doc/part-b/g9-cross-connection", "value": True}]},
            "an active callback proof cannot cross connections")
    finally:
        await probe(ctx, "release")
        await pending
    await refuses_public(ctx, worker, "workspace.apply", {"on_behalf_of": frame["invocation"],
        "ops": [{"op": "set", "key": "doc/part-b/g9-legacy-proof", "value": True}]},
        "legacy on_behalf_of cannot serve as a callback proof")
    expired = await probe(ctx, "expired", key="doc/part-b/g9-expired-proof")
    ctx.check(expired.get("error_kind") in ("forbidden", "invalid", "no_grant") and
              "doc/part-b/g9-expired-proof" not in (await ctx.peer.call("workspace.snapshot"))["entries"],
              "same managed connection cannot reuse an expired callback proof", component="core", result=expired)

    entry = (await ctx.peer.call("workspace.snapshot"))["entries"][direct]
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": direct,
        "value": {"from": owner, "to": worker_id, "until_ms": int(time.time() * 1000) - 1}, "expect": entry["version"]}]})
    await refuses_public(ctx, worker, "workspace.apply", write, "expired direct delegation admits no represented write")
    entry = (await ctx.peer.call("workspace.snapshot"))["entries"][direct]
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "delete", "key": direct, "expect": entry["version"]}]})
    for suffix, value in (("wild-from", {"from": "*", "to": worker_id}), ("wild-to", {"from": owner, "to": "*"})):
        # A stored literal '*' is not itself proof of wildcard matching. Test authority,
        # rather than overconstraining whether a literal row may be stored at all.
        try:
            stored = await ctx.peer.call("workspace.apply", {"ops": [{"op": "set",
                "key": "doc/principals/delegations/g9-" + suffix, "value": value}]})
        except RpcError as error:
            ctx.check(error.kind in ("invalid", "forbidden"), "wildcard row may be rejected at admission", component="core", refusal=error.kind)
        else: ctx.evidence.append({"kind": "literal_wildcard_row_stored", "transaction": stored, "value": value})
        await refuses_public(ctx, worker, "workspace.apply", write, "wildcard row cannot match an unrelated principal")
    await ctx.peer.call("workspace.apply", {"ops": [
        {"op": "set", "key": "doc/principals/delegations/g9-p-q", "value": {"from": owner, "to": "worker:g9-Q"}},
        {"op": "set", "key": "doc/principals/delegations/g9-q-w", "value": {"from": "worker:g9-Q", "to": worker_id}}]})
    await refuses_public(ctx, worker, "workspace.apply", write, "a delegation chain is not a direct P-to-W grant")
    # Same local id through one actual extension stays namespaced by each effective caller.
    senders = [await ctx.connect("worker:g9-same-A"), await ctx.connect("worker:g9-same-B")]
    for index in range(2):
        await ctx.grant("g9-same-" + str(index), "worker:g9-same-" + ("A" if index == 0 else "B"), ["send:start"])
    shared_op = await ctx.operation("same-through-E")
    shared_submission = await ctx.operation("same-submission-through-E")
    payload = {"op": shared_op, "submission": shared_submission, "to": ctx.agents[0]["id"],
        "mode": "start", "content": [{"type": "text", "text": "same local op through managed E"}]}
    sent = [await probe(ctx, "coord_call", peer=sender, command="inbox.send", payload=payload) for sender in senders]
    deliveries = [row["result"]["delivery"] for row in sent]
    ctx.check(len(set(deliveries)) == 2, "two effective callers through E do not alias one local op", component="coordination", results=sent)
    records = [await ctx.get(delivery) for delivery in deliveries]
    ctx.check([row["origin"]["principal"] for row in records] == ["worker:g9-same-A", "worker:g9-same-B"],
              "asynchronous inbox records keep each admission principal separately", records=records)
    ctx.evidence.append({"kind": "managed_identity_and_delegation", "managed_actor": actor, "owner": owner,
                         "worker": worker_id, "public_witnesses_only": True})


async def publish_question(ctx, identifier, *, revision=1, session="s1", expires_at_ms=None):
    question = {"request": identifier, "revision": revision, "scope": {"session": session},
        "status": "pending", "method": "confirm", "title": "Owned native confirmation: " + identifier,
        "params": {}, "incomplete": False}
    if expires_at_ms is not None: question["expires_at_ms"] = expires_at_ms
    await ctx.native_set(0, requests=[question])
    return question


async def respond(ctx, question, *, peer=None, epoch="e1", revision=None):
    args = {"v": 0, "request": question["request"], "epoch": epoch,
        "revision": question["revision"] if revision is None else revision, "response": {"value": True}}
    return await ctx.command("control", {"op": await ctx.operation("answer"), "target": ctx.agents[0]["id"],
        "action": "respond", "args": args}, peer=peer)


async def rejected_answer(ctx, question, *, reason, peer=None, **guards):
    before = len(ctx.native.records("native_response"))
    try: result = await respond(ctx, question, peer=peer, **guards)
    except RpcError as error:
        ctx.check(error.kind in ("stale", "invalid", "no_grant", "not_found"),
                  reason + " produces a definite refusal", refusal=error.kind)
    else:
        ctx.check(result.get("state") == "rejected", reason + " is not accepted or unknown", result=result)
    ctx.check(len(ctx.native.records("native_response")) == before,
              reason + " produces zero native responses", native=ctx.native.records("native_response"))


@case("Q4")
async def native_request_fences_expiry_and_revoked_answer(ctx):
    resource = next(row for row in await ctx.resources() if row["id"] == ctx.agents[0]["id"])
    declaration = resource["data"]["agent"]["caps"].get("agent.respond")
    ctx.check(isinstance(declaration, dict) and "confirm" in declaration.get("methods", []) and
              "session" in declaration.get("scopes", []), "owned native witness declares confirm/session response", component="harness", declaration=declaration)
    ctx.evidence.append({"kind": "response_declaration_selection", "declaration": declaration,
                         "run_scope": "not-applicable: native fixture declares session only"})
    worker = await ctx.connect("worker:q4-answer")
    await ctx.grant("q4-answer", "worker:q4-answer", ["answer"], "agent:" + ctx.agents[0]["id"])
    positive = await publish_question(ctx, "q4-positive", expires_at_ms=int(time.time() * 1000) + 60000)
    admitted = await respond(ctx, positive, peer=worker)
    ctx.check(admitted.get("state") in ("accepted", "observed", "dispatched") and
              len(ctx.native.records("native_response")) == 1, "valid granted response has an actual native positive witness", reply=admitted)
    expiry = int(time.time() * 1000) + 60000
    pending = await publish_question(ctx, "q4-guarded", expires_at_ms=expiry)
    await rejected_answer(ctx, pending, epoch="stale-epoch", reason="stale epoch")
    await rejected_answer(ctx, pending, revision=2, reason="stale request revision")
    stale_scope = await publish_question(ctx, "q4-guarded", session="old-session", expires_at_ms=expiry)
    await rejected_answer(ctx, stale_scope, reason="stale native request scope")
    expired = await publish_question(ctx, "q4-expired", expires_at_ms=int(time.time() * 1000) - 1)
    await rejected_answer(ctx, expired, reason="native request expiry")
    pending = await publish_question(ctx, "q4-unanswered", expires_at_ms=expiry)
    entries = (await ctx.peer.call("workspace.snapshot"))["entries"]
    grant = entries["doc/agents/grants/q4-answer"]
    revoked = await ctx.command("grants", {"op": await ctx.operation("revoke-answer"), "id": "q4-answer",
        "value": None, "expect": grant["version"]})
    ctx.check(revoked.get("state") in ("accepted", "observed"), "answer grant revocation actually commits", reply=revoked)
    mark = ctx.native.mark()
    await rejected_answer(ctx, pending, peer=worker, reason="revoked current answer right")
    ctx.check(not ctx.native.records("invoke", after=mark), "revoked answer grant is rejected before native dispatch")
    await ctx.command("status")
    current = next(row for row in await ctx.resources() if row["id"] == ctx.agents[0]["id"])["data"]["agent"]["requests"]
    ctx.check(current == [pending] and current[0]["expires_at_ms"] == expiry and
              len(ctx.native.records("native_response")) == 1, "unanswered request keeps native expiry and is never auto-approved/renewed", requests=current)
    final = await respond(ctx, pending)
    ctx.check(final.get("state") in ("accepted", "observed", "dispatched") and
              len(ctx.native.records("native_response")) == 2, "owner can still answer the exact unexpired request once", reply=final)
    async def answered():
        rows = next(row for row in await ctx.resources() if row["id"] == ctx.agents[0]["id"])["data"]["agent"].get("requests", [])
        return next((row for row in rows if row["request"] == pending["request"] and row["status"] == "answered"), None)
    await ctx.wait(answered, label="actual native answered fact")


async def routing_binding(ctx, operation, **params):
    """One isolated config/launch/UI binding, NOT a proposed production wire schema.

    These two historical pinned targets were confirmed without AS10c. Preserve
    their mandatory absence verdict, never unsupported-declared. A newer target
    requires an explicit matching source/fixture pin and the real approved public
    request-routes, request_route, pi.new, inbox and live-view surfaces. Unknown
    unbound targets cannot inherit the historical verdict. The separate adapter
    only observes public transactions/launches/notices; it never implements routing.
    """
    revision = ctx.manifest.get("coord_revision")
    known_without_routing = {"c67f818f7cee0b6832948c1e403f361bf576315b", "fe8e3d02c31fb78abfdc8d1616f2067e963a58e4"}
    if revision not in known_without_routing:
        from .request_routing_binding import route_operation
        return await route_operation(ctx, operation, mail_matches_request, **params)
    commands = (await ctx.peer.call("command.list"))["commands"]
    ctx.evidence.append({"kind": "required_request_routing_missing", "coord_revision": revision,
        "spec": "coordination-v0 section 6.2", "binding_step": operation,
        "public_commands": sorted(row["name"] for row in commands),
        "public_endpoints": [row["id"] for row in await ctx.resources()],
        "semantic_probes_executed": False})
    raise CheckFailed("routing not implemented in target", component="coordination")


@case("Q1")
@request_routing_case
async def route_launch_defaults_and_attributed_config(ctx):
    # Return shapes below are normalized actual public observations, not wire keys.
    await routing_binding(ctx, "ensure")
    human = await routing_binding(ctx, "launch", launcher=ctx.peer, role="owner")
    ctx.check(human["route"] == "human" and human["launch_evidence"], "actual owner launch records human route")
    coordinator = await ctx.connect("worker:q1-coordinator")
    team = await routing_binding(ctx, "launch", launcher=coordinator, role="coordinator")
    ctx.check(team["route"] == "coordinator" and team["launch_evidence"], "actual coordinator launch records coordinator route")
    changed = await routing_binding(ctx, "configure", agent=human["agent"], route="both", peer=ctx.peer)
    ctx.check(changed["transaction"]["principal"] == "owner", "route change is an attributed ordinary transaction")
    worker = await ctx.connect("worker:q1-configure")
    try: await routing_binding(ctx, "configure", agent=human["agent"], route="human", peer=worker)
    except RpcError as error: ctx.check(error.kind == "no_grant", "ungranted route change is refused")
    else: raise CheckFailed("ungranted route configuration was accepted")
    await ctx.grant("q1-configure", "worker:q1-configure", ["configure"], "agent:" + human["agent"])
    configured = await routing_binding(ctx, "configure", agent=human["agent"], route="coordinator", peer=worker)
    ctx.check(configured["transaction"]["principal"] == "worker:q1-configure", "configure holder writes under its own principal")
    await ctx.peer.call("workspace.revert", {"txn": configured["transaction"]["id"]})
    restored = await routing_binding(ctx, "inspect", agent=human["agent"])
    ctx.check(restored["route"] == "both", "undo restores the actual previous route configuration")
    requester = ctx.agents[0]["id"]
    await routing_binding(ctx, "configure", agent=requester, route="both", peer=ctx.peer)
    for scenario, value in (("missing", None), ("malformed", {"route": "invalid-owned-probe"})):
        before = await routing_binding(ctx, "raw_configure", agent=requester, value=value)
        question = await publish_question(ctx, "q1-" + scenario, expires_at_ms=int(time.time() * 1000) + 60000)
        visible = await routing_binding(ctx, "observe", agent=requester, question=question)
        ctx.check(not visible["mail"] and len(visible["human"]) == 1,
            "missing/malformed public route shows this native question to the human", scenario=scenario, observation=visible)
        if scenario == "malformed":
            ctx.check(visible["human"][0].get("reason") == "invalid_request_route",
                "malformed route has actual visible fallback reason", observation=visible)
        after = await routing_binding(ctx, "inspect", agent=requester)
        ctx.check(after["entry"] == before["entry"], "recovery does not silently repair raw missing/malformed config", scenario=scenario)


def mail_matches_request(mail, agent, question, epoch):
    """Match the actual native question identity, including its entire scope.

    Session/run/subject and operation-scoped questions are distinct (§5). Missing
    scope on both sides is not evidence, nor is Python's True == 1 JSON equality.
    Epoch is mandatory and must come from observed requester facts, never a default.
    """
    scope = question.get("scope")
    if not isinstance(scope, dict) or not isinstance(mail.get("scope"), dict): return False
    if "operation" in scope:
        valid_scope = set(scope) == {"operation"} and isinstance(scope["operation"], str) and bool(scope["operation"])
    else:
        valid_scope = ("session" in scope and set(scope) <= {"session", "run", "subject"} and
            isinstance(scope["session"], str) and bool(scope["session"]) and
            ("run" not in scope or isinstance(scope["run"], str) and bool(scope["run"])))
    if not valid_scope: return False
    return (isinstance(epoch, str) and bool(epoch) and mail.get("agent") == agent and mail.get("epoch") == epoch and
            mail.get("request") == question["request"] and type(mail.get("revision")) is int and
            type(question.get("revision")) is int and mail["revision"] > 0 and mail["revision"] == question["revision"] and
            json.dumps(mail["scope"], sort_keys=True, separators=(",", ":")) ==
            json.dumps(scope, sort_keys=True, separators=(",", ":")))


@case("Q2")
@request_routing_case
async def exact_recorded_coordinator_and_visible_fallback(ctx):
    await routing_binding(ctx, "ensure")
    requester, coordinator = ctx.agents[0]["id"], ctx.agents[1]["id"]
    await routing_binding(ctx, "configure", agent=requester, route="coordinator", peer=ctx.peer)
    await ctx.native_set(1, principal="worker:q2-coordinator")
    await ctx.native.call("add", ref="a2")
    await ctx.wait_agents(3)
    await ctx.native_set(2, principal="worker:q2-other")
    await member(ctx, 1, "q2-project")
    await member(ctx, 0, "q2-project", coordinator)
    await ctx.grant("q2-coordinator-answer", "worker:q2-coordinator", ["answer"], "agent:" + requester)
    await ctx.grant("q2-other-answer", "worker:q2-other", ["answer"], "agent:" + requester)
    await ctx.scope("resume", "agent:" + coordinator)
    question = await publish_question(ctx, "q2-exact", expires_at_ms=int(time.time() * 1000) + 60000)
    observed = await routing_binding(ctx, "observe", agent=requester, question=question)
    epoch = next(row for row in await ctx.resources() if row["id"] == requester)["data"]["agent"]["epoch"]
    ctx.check(len(observed["mail"]) == 1 and observed["mail"][0]["to"] == coordinator and
              mail_matches_request(observed["mail"][0], requester, question, epoch),
              "request mail names exact agent/epoch/id/revision/scope and only the recorded answer-granted coordinator")
    ctx.check(not observed["human"], "valid coordinator-only route does not also show a human request notice")
    # Native end is irreversible for this incarnation. Revoke while live, then
    # restore valid answer standing under a fresh grant before testing end last.
    for scenario in ("missing_coordinator", "missing_answer_grant", "ended_coordinator"):
        await member(ctx, 0, "q2-project", None if scenario == "missing_coordinator" else coordinator)
        if scenario == "ended_coordinator":
            await ctx.grant("q2-ended-answer", "worker:q2-coordinator", ["answer"], "agent:" + requester)
            await ctx.native_set(1, lifecycle="ended")
        if scenario == "missing_answer_grant":
            entry = (await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/q2-coordinator-answer"]
            await ctx.command("grants", {"op": await ctx.operation(), "id": "q2-coordinator-answer", "value": None, "expect": entry["version"]})
        question = await publish_question(ctx, "q2-" + scenario, expires_at_ms=int(time.time() * 1000) + 60000)
        fallback = await routing_binding(ctx, "observe", agent=requester, question=question)
        ctx.check(not fallback["mail"] and fallback["human"] and all(row.get("reason") for row in fallback["human"]),
                  "unavailable coordinator route falls back visibly, never guesses another grant holder", scenario=scenario, observation=fallback)


@case("Q3")
@request_routing_case
async def both_routes_and_first_valid_answer_race(ctx):
    await routing_binding(ctx, "ensure")
    requester, coordinator = ctx.agents[0]["id"], ctx.agents[1]["id"]
    await routing_binding(ctx, "configure", agent=requester, route="both", peer=ctx.peer)
    await ctx.native_set(1, principal="worker:q3-coordinator")
    await member(ctx, 1, "q3-project")
    await member(ctx, 0, "q3-project", coordinator)
    controllers = [ctx.peer, await ctx.connect("worker:q3-coordinator"), await ctx.connect("worker:q3-other")]
    for suffix in ("coordinator", "other"):
        await ctx.grant("q3-" + suffix, "worker:q3-" + suffix, ["answer"], "agent:" + requester)
    await ctx.scope("resume", "agent:" + coordinator)
    question = await publish_question(ctx, "q3-race", expires_at_ms=int(time.time() * 1000) + 60000)
    routed = await routing_binding(ctx, "observe", agent=requester, question=question)
    epoch = next(row for row in await ctx.resources() if row["id"] == requester)["data"]["agent"]["epoch"]
    ctx.check(routed["human"] and len(routed["mail"]) == 1 and routed["mail"][0]["to"] == coordinator and
              mail_matches_request(routed["mail"][0], requester, question, epoch), "both route has actual human notice and exact coordinator mail")
    before = len(ctx.native.records("native_response"))
    results = await asyncio.gather(*(respond(ctx, question, peer=peer, epoch=epoch) for peer in controllers), return_exceptions=True)
    ctx.check(len(ctx.native.records("native_response")) == before + 1, "racing valid controllers produce exactly one actual native response", replies=[str(row) for row in results])
    for peer in controllers:
        async def sees_answered():
            rows = (await peer.call("resource.list"))["resources"]
            requests = next(row for row in rows if row["id"] == requester)["data"]["agent"].get("requests", [])
            return any(row["request"] == question["request"] and row["status"] == "answered" for row in requests)
        await ctx.wait(sees_answered, label="racing controller sees actual answered request")
