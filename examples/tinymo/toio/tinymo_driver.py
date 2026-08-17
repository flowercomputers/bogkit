#!/usr/bin/env python3
"""tinymo driver: toio_demo.py plus a JSON-lines link to the tinymo brain (Rust + bog).

    cargo run -p tinymo -- record                # in one terminal
    python3 examples/tinymo/toio/tinymo_driver.py   # in another (needs the cube on)

Every drive keypress goes to the cube immediately AND is reported to the brain
as {"type":"move",...}. The brain stores moves in bog and, on playback /
boomerang, sends {"type":"drive",...} lines back which we execute.

Extra keys vs toio_demo: R (record start/stop), P (play last recording).
Use --no-cube to run without hardware and just exercise the protocol.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import sys
import time
from collections import deque

import toio_protocol as tp
from toio_demo import (
    Cube, CubeState, LED_COLOURS, RawTerminal, decode_keys, find_cube, list_cubes,
    CLEAR, HOME, EOL,
)

DEFAULT_SPEED = 30  # same as pressing "1"

MOVE_KEYS = {"w", "up", "s", "down", "a", "left", "d", "right", "q", "e"}
BRAIN_KEYS = {"r", "p"}


class FakeClient:
    """Stand-in for BleakClient when running with --no-cube."""
    is_connected = True

    async def write_gatt_char(self, uuid, data, response=False):
        pass

    async def read_gatt_char(self, uuid):
        return b"\x00\x00\x00\x00\x00\x00"

    async def start_notify(self, uuid, cb):
        pass


class Link:
    """Newline-delimited JSON over TCP to the brain."""

    def __init__(self, host: str, port: int):
        self.host, self.port = host, port
        self.reader = self.writer = None

    async def connect(self) -> None:
        self.reader, self.writer = await asyncio.open_connection(self.host, self.port)
        await self.send({"type": "hello", "name": "tinymo_driver"})

    async def close(self) -> None:
        if self.writer is not None:
            try:
                self.writer.close()
            except Exception:
                pass
        self.reader = self.writer = None

    async def send(self, msg: dict) -> None:
        if self.writer is None:
            return
        try:
            self.writer.write((json.dumps(msg) + "\n").encode())
            await self.writer.drain()
        except (ConnectionError, OSError):
            await self.close()

    async def recv(self) -> dict | None:
        if self.reader is None:
            return None
        try:
            line = await self.reader.readline()
        except (ConnectionError, OSError):
            line = b""
        if not line:
            await self.close()
            return None
        return json.loads(line)


class BrainCube(Cube):
    """Cube that reports each drive to the brain and can be muted during playback."""

    def __init__(self, client, state: CubeState, link: Link):
        super().__init__(client, state)
        self.link = link
        self.input_enabled = True
        self.status_lines: deque[str] = deque(maxlen=6)

    async def drive(self, left: int, right: int, label: str) -> None:
        # logical (pre-flip) speeds go to the brain; Cube.drive applies MOUNTED_BACKWARDS
        await super().drive(left, right, label)
        await self.link.send({
            "type": "move", "left": left, "right": right,
            "duration_ms": __import__("toio_demo").DRIVE_MS, "label": label,
        })

    async def replay_drive(self, left: int, right: int, duration_ms: int, label: str) -> None:
        """Execute a drive the brain asked for; do NOT report it back."""
        self.state.last_motor = f"[brain] {label} (L={left:+d} R={right:+d})"
        import toio_demo
        if toio_demo.MOUNTED_BACKWARDS:
            left, right = -right, -left
        await self._write(tp.MOTOR_UUID, tp.motor_timed(left, right, duration_ms))


async def handle_key(key: str, cube: BrainCube, quit_event: asyncio.Event) -> None:
    st = cube.state
    v = st.speed
    if key in MOVE_KEYS and not cube.input_enabled:
        st.log("(brain is driving — keys ignored)")
        return
    if key in ("w", "up"):
        await cube.drive(v, v, "forward")
    elif key in ("s", "down"):
        await cube.drive(-v, -v, "backward")
    elif key in ("a", "left"):
        await cube.drive(-v, v, "spin left")
    elif key in ("d", "right"):
        await cube.drive(v, -v, "spin right")
    elif key == "q":
        await cube.drive(v // 3, v, "curve left")
    elif key == "e":
        await cube.drive(v, v // 3, "curve right")
    elif key == " ":
        await cube.stop()
    elif key.isdigit() and key != "0":
        st.speed = int(key) * 30
        st.log(f"speed -> {st.speed}")
    elif key in BRAIN_KEYS:
        await cube.link.send({"type": "key", "key": key})
    elif key == "l":
        st.led_index = (st.led_index + 1) % len(LED_COLOURS)
        name, rgb = LED_COLOURS[st.led_index]
        await cube.set_led(rgb)
        st.log(f"LED -> {name}")
    elif key == "b":
        await cube.beep()
        st.log("beep")
    elif key in ("x", "\x03"):
        quit_event.set()


async def brain_loop(cube: BrainCube, quit_event: asyncio.Event) -> None:
    """Apply commands coming from the brain; reconnect if it goes away."""
    st = cube.state
    while not quit_event.is_set():
        msg = await cube.link.recv()
        if msg is None:
            st.log("brain disconnected — reconnecting")
            cube.status_lines.append(f"{time.strftime('%H:%M:%S')}  brain disconnected — reconnecting ...")
            cube.input_enabled = True  # never leave the keys locked
            while not quit_event.is_set():
                try:
                    await cube.link.connect()
                    st.log("brain reconnected")
                    break
                except OSError:
                    await asyncio.sleep(1.0)
            continue
        try:
            t = msg.get("type")
            if t == "drive":
                await cube.replay_drive(msg["left"], msg["right"], msg["duration_ms"], msg.get("label", ""))
            elif t == "led":
                await cube.set_led((msg["r"], msg["g"], msg["b"]))
            elif t == "beep":
                await cube.beep(msg.get("effect", 4))
            elif t == "status":
                cube.status_lines.append(f"{time.strftime('%H:%M:%S')}  {msg['text']}")
            elif t == "input":
                cube.input_enabled = bool(msg["enabled"])
        except Exception as e:  # a BLE hiccup must not kill the brain link
            st.log(f"brain cmd {t} failed: {e}")


def render(cube: BrainCube) -> str:
    st = cube.state
    m = st.motion
    motion = (
        f"flat={'Y' if m.get('flat') else 'n'}  posture={m.get('posture_name', '?')}  shake={m.get('shake', 0)}"
        if m else "(none yet)"
    )
    lines = [
        f"tinymo driver — {st.name}     keys: {'ENABLED' if cube.input_enabled else 'LOCKED (brain driving)'}",
        "",
        f"  battery : {st.battery if st.battery >= 0 else '?'}%   button: {'PRESSED' if st.button else 'released'}",
        f"  motion  : {motion}",
        f"  motor   : {st.last_motor}   speed={st.speed}",
        "",
        "  brain (bog):",
        *([f"    {e}" for e in cube.status_lines] or ["    (waiting for brain)"]),
        "",
        "  events:",
        *([f"    {e}" for e in st.events] or ["    (none)"]),
        "",
        "  W/S A/D or arrows: drive   Q/E: curve   Space: stop   1-9: speed",
        "  R: record start/stop   P: play last recording   L: LED   B: beep   X: quit",
    ]
    return HOME + "\n".join(line + EOL for line in lines) + "\x1b[J"


async def run(address: str | None, timeout: float, host: str, port: int, no_cube: bool) -> int:
    link = Link(host, port)
    print(f"connecting to brain at {host}:{port} ...")
    try:
        await link.connect()
    except OSError as e:
        print(f"could not reach the brain ({e}). Start it first: cargo run -p tinymo -- record")
        return 1

    quit_event = asyncio.Event()

    if no_cube:
        state = CubeState(name="(no cube)", speed=DEFAULT_SPEED)
        client = FakeClient()
        return await _session(client, state, link, quit_event)

    from bleak import BleakClient
    device = await find_cube(address, timeout)
    if device is None:
        print("no toio cube found. Make sure it's powered on (LED blinking) and near the Mac.")
        return 1
    print(f"connecting to {device.name} ({device.address}) ...")
    state = CubeState(name=device.name or device.address, speed=DEFAULT_SPEED)

    def on_disconnect(_client) -> None:
        state.log("disconnected")
        quit_event.set()

    async with BleakClient(device, disconnected_callback=on_disconnect) as client:
        return await _session(client, state, link, quit_event)


async def _session(client, state: CubeState, link: Link, quit_event: asyncio.Event) -> int:
    cube = BrainCube(client, state, link)
    await cube.subscribe()
    await cube.set_led((0, 255, 0))
    await cube.beep(1)
    state.log("connected")

    loop = asyncio.get_running_loop()
    loop.add_signal_handler(signal.SIGINT, quit_event.set)
    key_queue: asyncio.Queue[str] = asyncio.Queue()

    def stdin_ready() -> None:
        for k in decode_keys(os.read(sys.stdin.fileno(), 64)):
            key_queue.put_nowait(k)

    async def key_loop() -> None:
        while not quit_event.is_set():
            key = await key_queue.get()
            try:
                await handle_key(key, cube, quit_event)
            except Exception as e:
                state.log(f"key {key!r} failed: {e}")

    async def render_loop() -> None:
        while not quit_event.is_set():
            sys.stdout.write(render(cube))
            sys.stdout.flush()
            await asyncio.sleep(0.1)

    sys.stdout.write(CLEAR)
    with RawTerminal():
        if sys.stdin.isatty():
            loop.add_reader(sys.stdin.fileno(), stdin_ready)
        tasks = [
            asyncio.create_task(key_loop()),
            asyncio.create_task(render_loop()),
            asyncio.create_task(brain_loop(cube, quit_event)),
        ]
        try:
            await quit_event.wait()
        finally:
            for t in tasks:
                t.cancel()
            if sys.stdin.isatty():
                loop.remove_reader(sys.stdin.fileno())
            if client.is_connected:
                try:
                    await cube.stop()
                    await cube.set_led(None)
                except Exception:
                    pass
    sys.stdout.write(render(cube) + "\n")
    print("bye")
    return 0


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--list", action="store_true", help="list cubes in range and exit")
    ap.add_argument("--address", help="connect to this cube (address/UUID from --list)")
    ap.add_argument("--timeout", type=float, default=10.0, help="scan timeout in seconds")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=int(os.environ.get("TINYMO_PORT", 7777)))
    ap.add_argument("--no-cube", action="store_true", help="skip BLE; drive a fake cube (protocol test)")
    args = ap.parse_args()
    try:
        if args.list:
            asyncio.run(list_cubes(args.timeout))
        else:
            sys.exit(asyncio.run(run(args.address, args.timeout, args.host, args.port, args.no_cube)))
    except KeyboardInterrupt:
        print("\nbye")


if __name__ == "__main__":
    main()
