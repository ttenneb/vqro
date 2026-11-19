"""One protected operational domain; no workspace fallback or private persistence.

MemoryStore is a deterministic CAS/fault fixture for the identical engine seam. The
production adapter uses only C1's public operational.* protocol. Logical keys are
prefixed here, so no caller can accidentally write ordinary configuration.
"""
import copy
import json
import threading

DOMAIN = "coord"
DOMAIN_BYTES = 8 * 1024 * 1024  # C1 must qualify this allocation; never shard owners.
DOMAIN_RECORDS = 1024
CONTROL_RESERVE = 0.10
PAGE_ITEMS = 64
PAGE_BYTES = 256 * 1024


def encoded(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


class StoreError(RuntimeError):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


class MemoryStore:
    """Private fake, not an alternative product persistence path."""
    def __init__(self, *, entries=None, byte_limit=DOMAIN_BYTES, record_limit=DOMAIN_RECORDS):
        self.lock = threading.RLock()
        self.entries = copy.deepcopy(entries or {})
        self.revision = max((row["version"] for row in self.entries.values()), default=0)
        self.byte_limit, self.record_limit = byte_limit, record_limit
        self.fault = None
        self.writes = []

    def snapshot(self):
        with self.lock:
            return copy.deepcopy(self.entries)

    def get(self, key):
        with self.lock:
            return copy.deepcopy(self.entries.get(key))

    def apply(self, ops, *, control=False, note="coordination"):
        with self.lock:
            if not ops or len(ops) > PAGE_ITEMS or encoded(ops) > PAGE_BYTES:
                raise StoreError("too_large", "operational batch exceeds its bound")
            keys = [op["key"] for op in ops]
            if len(set(keys)) != len(keys):
                raise StoreError("invalid", "duplicate key in atomic batch")
            if self.fault == "before":
                self.fault = None
                raise StoreError("io", "injected before durable replacement; no change")
            candidate = copy.deepcopy(self.entries)
            revision = self.revision + 1
            for op in ops:
                key = op["key"]
                if not isinstance(key, str) or not key or len(key.encode()) > 460:
                    raise StoreError("invalid", "invalid logical operational key")
                current = candidate.get(key, {}).get("version", 0)
                if op.get("expect") != current:
                    raise StoreError("conflict", f"{key}: expected {op.get('expect')}, found {current}")
                if op["op"] == "set":
                    candidate[key] = {"version": revision, "value": copy.deepcopy(op["value"])}
                elif op["op"] == "delete":
                    candidate.pop(key, None)
                else:
                    raise StoreError("invalid", "unknown operational operation")
            fraction = 1 if control else 1 - CONTROL_RESERVE
            if ((len(candidate) > int(self.record_limit * fraction) and len(candidate) > len(self.entries))
                    or (encoded(candidate) > int(self.byte_limit * fraction)
                        and encoded(candidate) > encoded(self.entries))):
                raise StoreError("quota", "protected domain full; recovery reserve retained")
            self.entries, self.revision = candidate, revision
            result = {"id": revision, "changes": [{"key": op["key"], "version": revision if op["op"] == "set" else None} for op in ops]}
            self.writes.append(copy.deepcopy(result))
            if self.fault == "after":
                self.fault = None
                raise StoreError("durability_unconfirmed", "injected adopted replacement without durable confirmation")
            return result


class OperationalStore:
    """C1 wire adapter. A missing operational API fails closed, never workspace.apply."""
    def __init__(self, call, *, domain=DOMAIN):
        self.call, self.domain = call, domain
        self.prefix = f"operational/{domain}/"
        self.byte_limit, self.record_limit = DOMAIN_BYTES, DOMAIN_RECORDS

    def _call(self, method, params):
        try:
            return self.call(method, params)
        except Exception as error:
            kind = getattr(error, "kind", None) or "indeterminate"
            raise StoreError(kind, str(error)) from error

    def register(self):
        result = self._call("operational.register", {"name": self.domain, "bytes": DOMAIN_BYTES})
        # Size negotiation is intentionally isolated here while C1 pins the API.
        self.snapshot()
        return result

    def snapshot(self):
        result = self._call("operational.snapshot", {"domain": self.domain})
        limits = result.get("limits", {})
        self.byte_limit = limits.get("bytes", self.byte_limit)
        self.record_limit = limits.get("records", self.record_limit)
        return {self._logical(key): copy.deepcopy(row) for key, row in result["entries"].items()}

    def _logical(self, key):
        if not key.startswith(self.prefix):
            raise StoreError("invalid", "operational reply crossed domain boundary")
        return key[len(self.prefix):]

    def get(self, key):
        return self._call("operational.get", {"domain": self.domain, "key": self.prefix + key})["entry"]

    def apply(self, ops, *, control=False, note="coordination"):
        if not ops or len(ops) > PAGE_ITEMS:
            raise StoreError("too_large", "operational batch exceeds its bound")
        wire = [dict(op, key=self.prefix + op["key"]) for op in ops]
        result = self._call("operational.apply", {"domain": self.domain, "ops": wire, "control": control, "note": note})
        result = copy.deepcopy(result)
        for change in result.get("changes", []):
            change["key"] = self._logical(change["key"])
        return result
