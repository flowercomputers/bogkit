# toio Core Cube BLE Reference

## Overview
toio Core Cubes are small (~32mm) robot cubes by Sony. They work as standard BLE peripherals — no console or cartridges needed. They advertise as `"toio Core Cube-XXX"`.

**Auto power-off:** 15 minutes without BLE connection (warnings every 3 min).
**Avoid:** Holding button during blue flash enters "toio PlayGround mode" which blocks external BLE.

## Cube Capabilities
- **2 DC motors** (left/right wheels, speed 0-255, forward/backward, target-position navigation)
- **RGB LED indicator** (full color, duration, blink patterns)
- **Sound speaker** (11 preset effects, arbitrary MIDI notes, octaves 0-10)
- **ID sensor** (bottom) — reads position from toio play mats (X/Y + angle)
- **6-axis motion sensor** (accelerometer + gyroscope) — tilt, collision, double-tap, shake, posture
- **Magnetic sensor** — detects magnets
- **Button** (press/release)
- **Battery** monitoring (0-100%, notified ~every 5s)

## BLE Service & Characteristic UUIDs

Base pattern: `10B201xx-5B3B-4571-9508-CF3EFCD7BBAE`

| Characteristic | UUID (xx) | Properties |
|---|---|---|
| **Service** | `00` | Primary GATT service |
| **ID sensor** | `01` | Read, Notify |
| **Motor** | `02` | Write (no resp), Read, Notify |
| **Light** | `03` | Write |
| **Sound** | `04` | Write |
| **Motion sensor** | `06` | Write, Read, Notify |
| **Button** | `07` | Read, Notify |
| **Battery** | `08` | Read, Notify |
| **Configuration** | `FF` | Write, Read, Notify |

## Key Protocol Details

All multi-byte integers are **little-endian**.

### Motor Control
- Basic: `[0x01, 0x01, dir_L, speed_L, 0x02, dir_R, speed_R]`
  - Direction: `0x01`=forward, `0x02`=backward. Speed: 0-255.
- With duration: `[0x02, 0x01, dir_L, speed_L, 0x02, dir_R, speed_R, duration]` (duration × 10ms)
- Target position: type `0x03` — autonomous navigation on mat
- Acceleration: type `0x05` — acceleration and rotational velocity

### LED Control
- On: `[0x03, duration, 0x01, 0x01, R, G, B]` (duration × 10ms, 0=permanent)
- Off: `[0x01]`

### Sound
- Effect: `[0x02, effect_id, volume]` (effect_id 0-10, volume 0-255)
- MIDI: `[0x03, repeats, op_count, duration, note, volume, ...]`
- Stop: `[0x01]`

### Position ID (from mat)
13 bytes: `[0x01, X_lo, X_hi, Y_lo, Y_hi, angle_lo, angle_hi, ...]`

### Button
`0x01` byte at offset 1: `0x80`=pressed, `0x00`=released

### Battery
Single byte: 0-100 (increments of 10)

## Official Specification
https://toio.github.io/toio-spec/en/

## Libraries
| Library | Language | Install |
|---|---|---|
| toio.py | Python 3.8+ (bleak) | `pip install toio-py` |
| toio.js | Node.js (noble) | `npm install @toio/scanner` |
| p5.toio | Browser (Web Bluetooth) | Script tag |
