"""TCP JSON-line peer gossip."""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any, Awaitable, Callable

log = logging.getLogger("falcon-dag.network")
MessageHandler = Callable[[str, dict[str, Any]], Awaitable[None] | None]


class PeerServer:
    def __init__(self, host: str, port: int, on_message: MessageHandler) -> None:
        self.host = host
        self.port = port
        self.on_message = on_message
        self._server: asyncio.Server | None = None
        self._writers: set[asyncio.StreamWriter] = set()

    async def start(self) -> None:
        self._server = await asyncio.start_server(self._handle, self.host, self.port)
        log.info("peer listen %s:%s", self.host, self.port)

    async def stop(self) -> None:
        for w in list(self._writers):
            w.close()
            try:
                await w.wait_closed()
            except Exception:
                pass
        self._writers.clear()
        if self._server:
            self._server.close()
            await self._server.wait_closed()
            self._server = None

    async def broadcast(self, msg: dict[str, Any]) -> None:
        data = (json.dumps(msg, separators=(",", ":")) + "\n").encode()
        dead: list[asyncio.StreamWriter] = []
        for w in list(self._writers):
            try:
                w.write(data)
                await w.drain()
            except Exception:
                dead.append(w)
        for w in dead:
            self._writers.discard(w)
            try:
                w.close()
            except Exception:
                pass

    async def _handle(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        self._writers.add(writer)
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                try:
                    msg = json.loads(line.decode())
                except json.JSONDecodeError:
                    continue
                result = self.on_message(msg.get("type", ""), msg)
                if asyncio.iscoroutine(result):
                    await result
        except Exception as e:
            log.debug("peer error: %s", e)
        finally:
            self._writers.discard(writer)
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass


class PeerClient:
    def __init__(
        self, host: str, port: int, on_message: MessageHandler, name: str = ""
    ) -> None:
        self.host = host
        self.port = port
        self.on_message = on_message
        self.name = name or f"{host}:{port}"
        self._writer: asyncio.StreamWriter | None = None
        self._task: asyncio.Task | None = None
        self._stop = asyncio.Event()

    async def start(self) -> None:
        self._stop.clear()
        self._task = asyncio.create_task(self._run())

    async def stop(self) -> None:
        self._stop.set()
        if self._writer:
            self._writer.close()
            try:
                await self._writer.wait_closed()
            except Exception:
                pass
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass

    async def send(self, msg: dict[str, Any]) -> bool:
        if not self._writer:
            return False
        try:
            self._writer.write((json.dumps(msg, separators=(",", ":")) + "\n").encode())
            await self._writer.drain()
            return True
        except Exception:
            return False

    async def _run(self) -> None:
        while not self._stop.is_set():
            try:
                reader, self._writer = await asyncio.open_connection(self.host, self.port)
                log.info("connected to %s", self.name)
                while not self._stop.is_set():
                    line = await reader.readline()
                    if not line:
                        break
                    try:
                        msg = json.loads(line.decode())
                    except json.JSONDecodeError:
                        continue
                    result = self.on_message(msg.get("type", ""), msg)
                    if asyncio.iscoroutine(result):
                        await result
            except asyncio.CancelledError:
                raise
            except Exception as e:
                log.debug("%s down: %s", self.name, e)
            finally:
                if self._writer:
                    try:
                        self._writer.close()
                        await self._writer.wait_closed()
                    except Exception:
                        pass
                    self._writer = None
            if not self._stop.is_set():
                await asyncio.sleep(1.0)
