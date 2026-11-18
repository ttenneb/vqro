"""Credited streams for vqro_provider; byte bounds and ordering, no replay policy."""
import asyncio
import base64
import binascii
from collections import deque
from vqro_provider import MAX_DATA, ProviderError, integer, maybe_await, reason


class Stream:
    def __init__(self, provider, params):
        self.provider, self.params = provider, params
        self.id, self.ref = params["stream"], params["ref"]
        self.incarnation = params["incarnation"]
        self.offset = 0
        self.credit = params["window_bytes"]
        self.outstanding = 0
        self.on_input = self.on_resize = self.on_close = self.on_credit = self.on_end = None
        self.closed = False
        self._ready, self._wake = asyncio.Event(), asyncio.Event()
        self._wire_lock = asyncio.Lock()
        self._output = deque()
        self._output_bytes = 0
        self._input = asyncio.Queue()
        self._input_bytes = 0
        self._close_queued = False
        self._pending_credit = 0
        self._credit_task = None
        self._producer = provider._work(self._pump())
        self._consumer = provider._work(self._consume())

    def offer(self, data):
        """Enqueue <=64 KiB, not an acknowledgment. Raises before accepting if full."""
        if not isinstance(data, bytes) or len(data) > MAX_DATA:
            raise ProviderError("invalid", "output must be bytes, at most 64 KiB")
        if self.closed:
            raise ProviderError("unavailable", "stream closed")
        if not data:
            return None
        if self._output_bytes + len(data) > self.provider.limits.stream_queue_bytes or len(self._output) >= self.provider.limits.stream_queue_items:
            raise ProviderError("overloaded", "stream output queue full")
        receipt = asyncio.get_running_loop().create_future()
        self._output.append((data, receipt))
        self._output_bytes += len(data)
        self._wake.set()
        return receipt

    async def send(self, data):
        receipt = self.offer(data)
        return await receipt if receipt is not None else True

    async def error(self, error):
        if not self.closed:
            await self.provider.notify("stream.error", {"stream": self.id,
                "error": {"kind": error.kind, "message": str(error)}, "outcome": "rejected"})

    async def gap(self, gap):
        if gap not in ("backend_dropped", "paused", "viewer_slow"):
            raise ProviderError("invalid", "unknown gap kind")
        await self._end("stream.gap", {"reason": {"reason": "gap", "gap": gap}})

    async def close(self, why=None):
        await self._end("stream.closed", {"reason": reason(why or {"reason": "closed"})})

    async def _end(self, method, params):
        await self._ready.wait()
        async with self._wire_lock:
            if self.closed:
                return
            if method == "stream.gap":
                params["offset"] = self.offset
            self._finish()
            await self.provider.notify(method, {"stream": self.id, **params})

    def _finish(self):
        if self.closed:
            return
        self.closed = True
        if self.on_end is not None:
            try:
                self.on_end()  # synchronous local bookkeeping; no I/O here
            except Exception as error:
                self.provider.failure = f"stream cleanup callback: {error}"
        self.provider.streams.pop(self.id, None)
        self.provider._stream_ids.finish(self.id)
        self._ready.set()
        self._wake.set()
        while self._output:
            _, receipt = self._output.popleft()
            if not receipt.done():
                receipt.set_result(False)
        self._output_bytes = 0
        for task in (self._producer, self._consumer, self._credit_task):
            if task is not None and task is not asyncio.current_task():
                task.cancel()

    def _credit(self, count):
        count = min(count, self.outstanding)  # pinned v0: ignore excess
        self.credit += count
        self.outstanding -= count
        self._pending_credit += count
        self._wake.set()
        if count and self.on_credit is not None and self._credit_task is None:
            self._credit_task = self.provider._work(self._return_credit())

    async def _return_credit(self):
        try:
            while self._pending_credit and not self.closed:
                count, self._pending_credit = self._pending_credit, 0
                await maybe_await(self.on_credit(count))
        except Exception as error:
            await self.close({"reason": "closed", "message": f"backend credit failed: {error}"})
        finally:
            self._credit_task = None

    async def _pump(self):
        try:
            await self._ready.wait()
            while not self.closed:
                if not self._output or self.credit == 0:
                    self._wake.clear()
                    await self._wake.wait()
                    continue
                async with self._wire_lock:
                    data, receipt = self._output[0]
                    count = min(len(data), self.credit, MAX_DATA)
                    start = self.offset
                    self.offset += count
                    self.credit -= count
                    self.outstanding += count
                    try:
                        await self.provider.notify("stream.data", {"stream": self.id, "offset": start,
                            "data": base64.b64encode(data[:count]).decode("ascii")})
                    except ProviderError as error:
                        if error.kind in ("overloaded", "invalid"):
                            # These failures are before the socket write.
                            self.offset -= count
                            self.credit += count
                            self.outstanding -= count
                        raise
                    self._output_bytes -= count
                    if count == len(data):
                        self._output.popleft()
                        if not receipt.done():
                            receipt.set_result(True)
                    else:
                        self._output[0] = (data[count:], receipt)
        except ProviderError:
            if not self.provider._closed.is_set():
                await self.gap("viewer_slow")
        finally:
            if self.provider._closed.is_set():
                self._finish()

    def _enqueue(self, method, params):
        if self._close_queued:
            return
        value, size = None, 32
        if method == "stream.input":
            if "terminal.input" not in self.provider.capabilities:
                raise ProviderError("unsupported", "input unsupported")
            try:
                value = base64.b64decode(params["data"], validate=True)
            except (KeyError, TypeError, ValueError, binascii.Error):
                raise ProviderError("invalid", "bad input base64") from None
            size = max(1, len(value))
            if size > MAX_DATA:
                raise ProviderError("invalid", "input exceeds 64 KiB")
        elif method == "stream.resize":
            if "terminal.resize" not in self.provider.capabilities:
                raise ProviderError("unsupported", "resize unsupported")
            value = (integer(params.get("cols"), 1, 9999), integer(params.get("rows"), 1, 9999))
        if method == "stream.resize" and ("terminal.resize" not in self.provider.capabilities or self.on_resize is None):
            raise ProviderError("unsupported", "resize unsupported")
        if method == "stream.close":
            self._close_queued = True  # one reserved final marker, even when data queue is full
        elif self._input_bytes + size > self.provider.limits.stream_queue_bytes or self._input.qsize() >= self.provider.limits.stream_queue_items:
            raise ProviderError("overloaded", "stream input queue full; not consumed")
        self._input_bytes += size
        self._input.put_nowait((method, value, size))

    async def _consume(self):
        await self._ready.wait()
        while not self.closed:
            method, value, size = await self._input.get()
            try:
                if method == "stream.close":
                    if self.on_close is not None:
                        await maybe_await(self.on_close())
                    await self.close()
                elif method == "stream.input":
                    if self.on_input is None:
                        raise ProviderError("unsupported", "no input handler")
                    await maybe_await(self.on_input(value))
                elif self.on_resize is not None:
                    await maybe_await(self.on_resize(*value))
            except ProviderError as error:
                await self.error(error)
            except Exception as error:
                # A native call may have happened: do not fabricate rejection.
                await self.close({"reason": "closed", "message": f"backend outcome unknown: {error}"})
            finally:
                self._input_bytes -= size
