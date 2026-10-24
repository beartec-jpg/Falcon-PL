#!/usr/bin/env python3
"""Public seed relays on the Helsinki host (5.78.142.246).

v12 :19312 is that host's own node, so it is not relayed.
v1 :19301 is the archive. Observers pull the join snapshot from it.
"""
import asyncio

ROUTES = (
    (19301, "100.64.0.1", 19301),
    (19303, "100.64.0.3", 19303),
    (19306, "100.64.0.6", 19306),
    (19307, "100.64.0.7", 19307),
    (19311, "100.64.0.3", 19311),
    (19313, "100.64.0.4", 19313),
)


async def pipe(reader, writer):
    try:
        while True:
            data = await reader.read(1 << 16)
            if not data:
                break
            writer.write(data)
            await writer.drain()
    except Exception:
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


async def handle(client_r, client_w, host, port):
    try:
        up_r, up_w = await asyncio.open_connection(host, port)
    except Exception:
        client_w.close()
        return
    await asyncio.gather(pipe(client_r, up_w), pipe(up_r, client_w))


async def main():
    servers = []
    for listen, host, port in ROUTES:
        server = await asyncio.start_server(
            lambda r, w, h=host, p=port: handle(r, w, h, p), "0.0.0.0", listen
        )
        print(f"relay 0.0.0.0:{listen} -> {host}:{port}", flush=True)
        servers.append(server)
    await asyncio.gather(*(s.serve_forever() for s in servers))


if __name__ == "__main__":
    asyncio.run(main())
