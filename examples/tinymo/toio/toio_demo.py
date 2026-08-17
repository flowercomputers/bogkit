#!/usr/bin/env python3
"""toio Core Cube starter demo: connect, drive with the keyboard, watch live sensor data.

    python3 toio_demo.py                 # scan for the first cube, connect
    python3 toio_demo.py --list          # list cubes in range and exit
    python3 toio_demo.py --address XXXX  # connect to a specific cube (from --list)

Keys (while running):
    W/S or Up/Down      forward / backward
    A/D or Left/Right   spin left / right
    Q/E                 curve left / right
    Space               stop
    1-9                 set speed (x30)
    L                   cycle LED colour
    B                   beep
    Ctrl-C or X         quit
"""
from __future__ import annotations

import argparse
import asyncio
import os
import signal
import sys
import termios
import time
import tty
from collections import deque
from dataclasses import dataclass, field

from bleak import BleakClient, BleakScanner
from bleak.backends.characteristic import BleakGATTCharacteristic

import toio_protocol as tp

# ------------------------------------------------------------------ state ---

@dataclass
class CubeState:
    name: str = "?"
    battery: int = -1
    button: bool = False
    pos: dict = field(default_factory=lambda: {"type": "none"})
    motion: dict = field(default_factory=dict)
    last_id_time: float = 0.0
    speed: int = 90
    led_index: int = -1
    last_motor: str = "stop"
    events: deque = field(default_factory=lambda: deque(maxlen=8))
    id_count: int = 0

    def log(self, msg: str) -> None:
        self.events.append(f"{time.strftime('%H:%M:%S')}  {msg}")


LED_COLOURS = [
    ("red", (255, 0, 0)), ("green", (0, 255, 0)), ("blue", (0, 0, 255)),
    ("yellow", (255, 200, 0)), ("magenta", (255, 0, 255)), ("cyan", (0, 255, 255)),
    ("white", (255, 255, 255)), ("off", None),
]

# How long each key press drives the motors. The terminal can't see key-up
# events, so we rely on key auto-repeat re-sending while a key is held; each
# press keeps the wheels turning for this long and then they stop by themselves.
DRIVE_MS = 400

# Set True if the cube sits rotated 180° in its enclosure (its "front" points
# backwards). Forward/back flip and the wheels swap sides so all controls stay
# intuitive from the enclosure's point of view.
MOUNTED_BACKWARDS = True


# ------------------------------------------------------------------- cube ---

class Cube:
    """Thin wrapper over BleakClient for one toio cube."""

    def __init__(self, client: BleakClient, state: CubeState):
        self.client = client
        self.state = state

    # -- outgoing --
    async def _write(self, uuid: str, data: bytes) -> None:
        await self.client.write_gatt_char(uuid, data, response=False)

    async def drive(self, left: int, right: int, label: str) -> None:
        self.state.last_motor = f"{label} (L={left:+d} R={right:+d})"
        if MOUNTED_BACKWARDS:
            left, right = -right, -left
        await self._write(tp.MOTOR_UUID, tp.motor_timed(left, right, DRIVE_MS))

    async def stop(self) -> None:
        self.state.last_motor = "stop"
        await self._write(tp.MOTOR_UUID, tp.motor_stop())

    async def set_led(self, rgb) -> None:
        pkt = tp.led(*rgb) if rgb else tp.led_off()
        await self._write(tp.LIGHT_UUID, pkt)

    async def beep(self, effect_id: int = 4) -> None:
        await self._write(tp.SOUND_UUID, tp.sound_effect(effect_id))

    # -- incoming --
    def on_id(self, _c: BleakGATTCharacteristic, data: bytearray) -> None:
        self.state.pos = tp.parse_id(bytes(data))
        self.state.last_id_time = time.monotonic()
        self.state.id_count += 1

    def on_motion(self, _c: BleakGATTCharacteristic, data: bytearray) -> None:
        m = tp.parse_motion(bytes(data))
        if m is None:
            return
        prev = self.state.motion
        self.state.motion = m
        if m["collision"]:
            self.state.log("collision!")
        if m["double_tap"]:
            self.state.log("double tap")
        if m["shake"] and m["shake"] != prev.get("shake"):
            self.state.log(f"shake level {m['shake']}")
        if m["posture"] != prev.get("posture"):
            self.state.log(f"posture -> {m['posture_name']}")

    def on_button(self, _c: BleakGATTCharacteristic, data: bytearray) -> None:
        pressed = tp.parse_button(bytes(data))
        self.state.button = pressed
        self.state.log("button pressed" if pressed else "button released")

    def on_battery(self, _c: BleakGATTCharacteristic, data: bytearray) -> None:
        self.state.battery = tp.parse_battery(bytes(data))

    async def subscribe(self) -> None:
        # Seed state with a read, then subscribe for changes.
        self.on_battery(None, await self.client.read_gatt_char(tp.BATTERY_UUID))
        self.on_button(None, await self.client.read_gatt_char(tp.BUTTON_UUID))
        self.state.events.clear()  # drop the "button released" from the seed read
        self.on_motion(None, await self.client.read_gatt_char(tp.MOTION_UUID))
        await self.client.start_notify(tp.ID_UUID, self.on_id)
        await self.client.start_notify(tp.MOTION_UUID, self.on_motion)
        await self.client.start_notify(tp.BUTTON_UUID, self.on_button)
        await self.client.start_notify(tp.BATTERY_UUID, self.on_battery)


# ------------------------------------------------------------- keyboard ----

class RawTerminal:
    """Context manager: put stdin in cbreak mode so single keys arrive immediately."""

    def __init__(self):
        self.fd = sys.stdin.fileno()
        self.saved = None

    def __enter__(self):
        if sys.stdin.isatty():
            self.saved = termios.tcgetattr(self.fd)
            tty.setcbreak(self.fd)
        return self

    def __exit__(self, *exc):
        if self.saved is not None:
            termios.tcsetattr(self.fd, termios.TCSADRAIN, self.saved)


ARROWS = {"\x1b[A": "up", "\x1b[B": "down", "\x1b[C": "right", "\x1b[D": "left"}


def decode_keys(buf: bytes) -> list[str]:
    """Turn raw stdin bytes into a list of key names ('w', 'up', ' ', ...)."""
    s = buf.decode(errors="ignore")
    keys, i = [], 0
    while i < len(s):
        if s[i] == "\x1b" and s[i:i + 3] in ARROWS:
            keys.append(ARROWS[s[i:i + 3]])
            i += 3
        else:
            keys.append(s[i].lower())
            i += 1
    return keys


async def handle_key(key: str, cube: Cube, quit_event: asyncio.Event) -> None:
    st = cube.state
    v = st.speed
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


# ---------------------------------------------------------------- render ----

CLEAR, HOME, EOL = "\x1b[2J", "\x1b[H", "\x1b[K"


def render(st: CubeState) -> str:
    p = st.pos
    if p.get("type") == "position":
        pos = f"x={p['x']:4d}  y={p['y']:4d}  angle={p['angle']:3d}°"
    elif p.get("type") == "standard":
        pos = f"standard ID {p['value']}  angle={p['angle']}°"
    elif p.get("type") in ("position_missed", "standard_missed"):
        pos = "off mat"
    else:
        pos = "(no data — put the cube on a toio mat)"
    if st.last_id_time and time.monotonic() - st.last_id_time > 1.0 and "off" not in pos:
        pos += "  (stale)"

    m = st.motion
    motion = (
        f"flat={'Y' if m.get('flat') else 'n'}  posture={m.get('posture_name', '?')}"
        f"  shake={m.get('shake', 0)}"
        if m else "(none yet — tap or tilt the cube)"
    )
    lines = [
        f"toio demo — {st.name}",
        "",
        f"  battery : {st.battery if st.battery >= 0 else '?'}%",
        f"  button  : {'PRESSED' if st.button else 'released'}",
        f"  mat pos : {pos}   [{st.id_count} updates]",
        f"  motion  : {motion}",
        f"  motor   : {st.last_motor}   speed={st.speed}",
        "",
        "  events:",
        *([f"    {e}" for e in st.events] or ["    (none)"]),
        "",
        "  W/S A/D or arrows: drive   Q/E: curve   Space: stop   1-9: speed",
        "  L: LED   B: beep   X or Ctrl-C: quit",
    ]
    return HOME + "\n".join(line + EOL for line in lines) + "\x1b[J"


# ------------------------------------------------------------------ main ----

def _is_cube(device, adv) -> bool:
    name = adv.local_name or device.name or ""
    return name.startswith(tp.ADVERTISED_NAME_PREFIX)


async def list_cubes(timeout: float) -> None:
    print(f"scanning for {timeout:.0f}s ...")
    found = await BleakScanner.discover(timeout=timeout, return_adv=True)
    cubes = [(d, a) for d, a in found.values() if _is_cube(d, a)]
    if not cubes:
        print("no toio cubes found (is the cube on and blinking?)")
        return
    for d, a in cubes:
        print(f"  {(a.local_name or d.name):24s}  address={d.address}  rssi={a.rssi}")


async def find_cube(address: str | None, timeout: float):
    if address:
        print(f"looking for {address} ...")
        return await BleakScanner.find_device_by_address(address, timeout=timeout)
    print("scanning for a toio cube ...")
    return await BleakScanner.find_device_by_filter(_is_cube, timeout=timeout)


async def run(address: str | None, timeout: float) -> int:
    device = await find_cube(address, timeout)
    if device is None:
        print("no toio cube found. Make sure it's powered on (LED blinking) and near the Mac.")
        return 1
    print(f"connecting to {device.name} ({device.address}) ...")

    quit_event = asyncio.Event()
    state = CubeState(name=device.name or device.address)

    def on_disconnect(_client: BleakClient) -> None:
        state.log("disconnected")
        quit_event.set()

    async with BleakClient(device, disconnected_callback=on_disconnect) as client:
        cube = Cube(client, state)
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
                await handle_key(key, cube, quit_event)

        async def render_loop() -> None:
            while not quit_event.is_set():
                sys.stdout.write(render(state))
                sys.stdout.flush()
                await asyncio.sleep(0.1)

        sys.stdout.write(CLEAR)
        with RawTerminal():
            if sys.stdin.isatty():
                loop.add_reader(sys.stdin.fileno(), stdin_ready)
            tasks = [asyncio.create_task(key_loop()), asyncio.create_task(render_loop())]
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
        sys.stdout.write(render(state) + "\n")
    print("bye")
    return 0


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--list", action="store_true", help="list cubes in range and exit")
    ap.add_argument("--address", help="connect to this cube (address/UUID from --list)")
    ap.add_argument("--timeout", type=float, default=10.0, help="scan timeout in seconds")
    args = ap.parse_args()
    try:
        if args.list:
            asyncio.run(list_cubes(args.timeout))
        else:
            sys.exit(asyncio.run(run(args.address, args.timeout)))
    except KeyboardInterrupt:
        print("\nbye")  # only reachable during the scan phase


if __name__ == "__main__":
    main()
