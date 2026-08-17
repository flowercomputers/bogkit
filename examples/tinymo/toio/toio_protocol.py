"""toio Core Cube BLE protocol: UUIDs, packet encoders, notification decoders.

Pure functions only (no BLE I/O) so this can be unit-tested and reused.
Spec: https://toio.github.io/toio-spec/en/  (also docs/toio-ble-reference.md)
All multi-byte integers are little-endian.
"""
from __future__ import annotations

import struct
from typing import Optional


def _uuid(xx: str) -> str:
    return f"10b201{xx}-5b3b-4571-9508-cf3efcd7bbae"


SERVICE_UUID = _uuid("00")
ID_UUID = _uuid("01")        # read / notify   (position on mat)
MOTOR_UUID = _uuid("02")     # write w/o resp
LIGHT_UUID = _uuid("03")     # write
SOUND_UUID = _uuid("04")     # write
MOTION_UUID = _uuid("06")    # read / notify
BUTTON_UUID = _uuid("07")    # read / notify
BATTERY_UUID = _uuid("08")   # read / notify
CONFIG_UUID = _uuid("ff")    # write / read / notify

ADVERTISED_NAME_PREFIX = "toio Core Cube"


def _clamp(v: int, lo: int, hi: int) -> int:
    return max(lo, min(hi, int(v)))


# ---------------------------------------------------------------- motor ----

def _wheel(wheel_id: int, speed: int) -> bytes:
    direction = 0x01 if speed >= 0 else 0x02
    return bytes([wheel_id, direction, _clamp(abs(speed), 0, 255)])


def motor(left: int, right: int) -> bytes:
    """Run motors until told otherwise. Speed -255..255 (negative = backward)."""
    return bytes([0x01]) + _wheel(0x01, left) + _wheel(0x02, right)


def motor_timed(left: int, right: int, duration_ms: int) -> bytes:
    """Run motors for duration_ms (10 ms resolution, max 2550 ms), then stop."""
    return bytes([0x02]) + _wheel(0x01, left) + _wheel(0x02, right) + bytes([
        _clamp(duration_ms // 10, 0, 255)
    ])


def motor_stop() -> bytes:
    return motor(0, 0)


# ---------------------------------------------------------------- light ----

def led(r: int, g: int, b: int, duration_ms: int = 0) -> bytes:
    """Turn the LED on. duration_ms=0 keeps it on until changed."""
    return bytes([
        0x03, _clamp(duration_ms // 10, 0, 255), 0x01, 0x01,
        _clamp(r, 0, 255), _clamp(g, 0, 255), _clamp(b, 0, 255),
    ])


def led_off() -> bytes:
    return bytes([0x01])


# ---------------------------------------------------------------- sound ----

def sound_effect(effect_id: int, volume: int = 255) -> bytes:
    """Play a preset sound effect (0..10)."""
    if not 0 <= effect_id <= 10:
        raise ValueError("effect_id must be 0..10")
    return bytes([0x02, effect_id, _clamp(volume, 0, 255)])


def midi_note(note: int, duration_ms: int, volume: int = 255) -> bytes:
    """Play a single MIDI note (0..127; 128 = rest)."""
    return bytes([
        0x03, 1, 1, _clamp(duration_ms // 10, 1, 255), _clamp(note, 0, 128),
        _clamp(volume, 0, 255),
    ])


def sound_stop() -> bytes:
    return bytes([0x01])


# --------------------------------------------- motion-sensor requests ----
# Written to MOTION_UUID; the cube replies with a notification of that type.

REQUEST_DETECTION = bytes([0x81])
REQUEST_MAGNET = bytes([0x82])


def request_posture(kind: int = 0x01) -> bytes:
    """Ask for one posture-angle notification. kind: 1=euler, 2=quaternion, 3=hp euler."""
    return bytes([0x83, kind])


# ------------------------------------------------------- configuration ----
# Written to CONFIG_UUID; the cube replies on CONFIG_UUID (see parse_config).

REQUEST_PROTOCOL_VERSION = bytes([0x01, 0x00])


def config_id_notify(min_interval_ms: int = 10, condition: int = 0xFF) -> bytes:
    """ID sensor notify rate. condition: 0x00 always, 0x01 on change, 0xFF 300ms-or-change."""
    return bytes([0x18, 0x00, _clamp(min_interval_ms // 10, 0, 255), condition])


def config_magnet(function: int = 0x02, interval_ms: int = 100, on_change: bool = True) -> bytes:
    """function: 0 off, 1 magnet state, 2 magnetic force (fw >= 2.3). interval in 20 ms steps."""
    return bytes([0x1B, 0x00, function, _clamp(interval_ms // 20, 0, 255), 1 if on_change else 0])


def config_motor_speed(enabled: bool = True) -> bytes:
    """Stream actual wheel speeds on MOTOR_UUID as 0xE0 notifications (fw >= 2.2)."""
    return bytes([0x1C, 0x00, 1 if enabled else 0])


def config_posture(kind: int = 0x01, interval_ms: int = 100, on_change: bool = True) -> bytes:
    """kind: 1 euler, 2 quaternion, 3 high-precision euler (fw >= 2.4). interval in 10 ms steps."""
    return bytes([0x1D, 0x00, kind, _clamp(interval_ms // 10, 0, 255), 1 if on_change else 0])


# ------------------------------------------------------------- decoders ----

def parse_id(data: bytes) -> dict:
    """ID sensor notification (position on a toio mat / standard ID card)."""
    if not data:
        return {"type": "empty"}
    t = data[0]
    if t == 0x01 and len(data) >= 13:
        x, y, a, sx, sy, sa = struct.unpack_from("<HHHHHH", data, 1)
        return {"type": "position", "x": x, "y": y, "angle": a,
                "sensor_x": sx, "sensor_y": sy, "sensor_angle": sa}
    if t == 0x02 and len(data) >= 7:
        value, angle = struct.unpack_from("<IH", data, 1)
        return {"type": "standard", "value": value, "angle": angle}
    if t == 0x03:
        return {"type": "position_missed"}
    if t == 0x04:
        return {"type": "standard_missed"}
    return {"type": "unknown", "raw": data.hex()}


POSTURES = {
    1: "top up", 2: "bottom up", 3: "left up",
    4: "right up", 5: "front up", 6: "back up",
}


def parse_motion(data: bytes) -> Optional[dict]:
    """Motion sensor notification, type 0x01 (detection info). Returns None otherwise."""
    if len(data) < 6 or data[0] != 0x01:
        return None
    return {
        "flat": bool(data[1]),
        "collision": bool(data[2]),
        "double_tap": bool(data[3]),
        "posture": data[4],
        "posture_name": POSTURES.get(data[4], f"unknown({data[4]})"),
        "shake": data[5],
    }


MAGNET_STATES = {
    0: "none", 1: "N-up (pattern 1)", 2: "pattern 2", 3: "pattern 3",
    4: "pattern 4", 5: "pattern 5", 6: "pattern 6",
}


def parse_magnet(data: bytes) -> Optional[dict]:
    """Motion sensor notification, type 0x02 (magnetic sensor). Needs config_magnet() first."""
    if len(data) < 2 or data[0] != 0x02:
        return None
    r = {"state": data[1], "state_name": MAGNET_STATES.get(data[1], f"unknown({data[1]})")}
    if len(data) >= 6:  # firmware >= 2.3 adds force + direction
        r["force"] = data[2]
        r["dir_x"], r["dir_y"], r["dir_z"] = struct.unpack_from("<bbb", data, 3)
    return r


def parse_posture(data: bytes) -> Optional[dict]:
    """Motion sensor notification, type 0x03 (posture angle). Needs config_posture() first."""
    if len(data) < 2 or data[0] != 0x03:
        return None
    kind = data[1]
    if kind == 0x01 and len(data) >= 8:
        roll, pitch, yaw = struct.unpack_from("<hhh", data, 2)
        return {"kind": "euler", "roll": roll, "pitch": pitch, "yaw": yaw}
    if kind == 0x02 and len(data) >= 18:
        w, x, y, z = struct.unpack_from("<ffff", data, 2)
        return {"kind": "quaternion", "w": w, "x": x, "y": y, "z": z}
    if kind == 0x03 and len(data) >= 14:
        roll, pitch, yaw = struct.unpack_from("<fff", data, 2)
        return {"kind": "euler_hp", "roll": roll, "pitch": pitch, "yaw": yaw}
    return {"kind": f"unknown({kind})", "raw": data.hex()}


def parse_motion_any(data: bytes) -> dict:
    """Decode any motion-characteristic notification into {"kind": ..., ...}."""
    if data and data[0] == 0x01 and (m := parse_motion(data)):
        return {"kind": "detection", **m}
    if data and data[0] == 0x02 and (m := parse_magnet(data)):
        return {"kind": "magnet", **m}
    if data and data[0] == 0x03 and (m := parse_posture(data)):
        return {"kind": "posture", **m}
    return {"kind": "unknown", "raw": data.hex()}


def parse_motor(data: bytes) -> dict:
    """Motor characteristic notification: speed feedback (0xE0) or target-position results."""
    if not data:
        return {"kind": "empty"}
    t = data[0]
    if t == 0xE0 and len(data) >= 3:
        return {"kind": "speed", "left": data[1], "right": data[2]}
    if t in (0x83, 0x84) and len(data) >= 3:
        return {"kind": "target_result" if t == 0x83 else "multi_target_result",
                "control_id": data[1], "result": data[2]}
    return {"kind": "unknown", "raw": data.hex()}


CONFIG_RESPONSES = {
    0x81: "protocol_version", 0x98: "id_notify_setting", 0x99: "id_missed_setting",
    0x9B: "magnet_setting", 0x9C: "motor_speed_setting", 0x9D: "posture_setting",
    0x9E: "serialized_setting", 0xB0: "conn_interval_request", 0xB1: "conn_interval_request_value",
    0xB2: "conn_interval_actual",
}


def parse_config(data: bytes) -> dict:
    """Configuration characteristic notification/read."""
    if not data:
        return {"kind": "empty"}
    t = data[0]
    name = CONFIG_RESPONSES.get(t, f"unknown(0x{t:02x})")
    if t == 0x81 and len(data) >= 7:
        return {"kind": name, "version": data[2:7].decode(errors="replace")}
    if len(data) >= 3:
        return {"kind": name, "result": data[2], "ok": data[2] == 0x00}
    return {"kind": name, "raw": data.hex()}


def parse_button(data: bytes) -> bool:
    """True while pressed."""
    return len(data) >= 2 and data[1] == 0x80


def parse_battery(data: bytes) -> int:
    return data[0] if data else -1
