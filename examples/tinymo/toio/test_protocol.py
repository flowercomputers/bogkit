"""Unit tests for toio_protocol (pure encode/decode, no BLE needed).

Run: python3 -m pytest test_protocol.py   (or: python3 test_protocol.py)
"""
import unittest

import toio_protocol as tp


class TestUUIDs(unittest.TestCase):
    def test_service_uuid(self):
        self.assertEqual(tp.SERVICE_UUID, "10b20100-5b3b-4571-9508-cf3efcd7bbae")

    def test_characteristic_uuids(self):
        self.assertEqual(tp.ID_UUID, "10b20101-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.MOTOR_UUID, "10b20102-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.LIGHT_UUID, "10b20103-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.SOUND_UUID, "10b20104-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.MOTION_UUID, "10b20106-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.BUTTON_UUID, "10b20107-5b3b-4571-9508-cf3efcd7bbae")
        self.assertEqual(tp.BATTERY_UUID, "10b20108-5b3b-4571-9508-cf3efcd7bbae")


class TestMotorEncoding(unittest.TestCase):
    def test_forward_both_wheels(self):
        self.assertEqual(
            tp.motor(100, 100),
            bytes([0x01, 0x01, 0x01, 100, 0x02, 0x01, 100]),
        )

    def test_negative_speed_is_backward(self):
        self.assertEqual(
            tp.motor(-50, 80),
            bytes([0x01, 0x01, 0x02, 50, 0x02, 0x01, 80]),
        )

    def test_speed_is_clamped_to_255(self):
        pkt = tp.motor(999, -999)
        self.assertEqual(pkt[3], 255)
        self.assertEqual(pkt[6], 255)

    def test_timed_motor(self):
        # 200 ms -> duration byte 20 (units of 10 ms)
        self.assertEqual(
            tp.motor_timed(60, -60, 200),
            bytes([0x02, 0x01, 0x01, 60, 0x02, 0x02, 60, 20]),
        )

    def test_timed_motor_clamps_duration(self):
        self.assertEqual(tp.motor_timed(1, 1, 99999)[-1], 255)

    def test_stop(self):
        self.assertEqual(tp.motor_stop(), bytes([0x01, 0x01, 0x01, 0, 0x02, 0x01, 0]))


class TestLightAndSound(unittest.TestCase):
    def test_led_on_permanent(self):
        self.assertEqual(tp.led(255, 0, 128), bytes([0x03, 0, 0x01, 0x01, 255, 0, 128]))

    def test_led_on_with_duration(self):
        self.assertEqual(tp.led(1, 2, 3, duration_ms=500)[1], 50)

    def test_led_off(self):
        self.assertEqual(tp.led_off(), bytes([0x01]))

    def test_sound_effect(self):
        self.assertEqual(tp.sound_effect(3, 200), bytes([0x02, 3, 200]))

    def test_sound_effect_id_range(self):
        with self.assertRaises(ValueError):
            tp.sound_effect(11)

    def test_midi_note(self):
        # one note, 300 ms -> 30, note 60, volume 255
        self.assertEqual(
            tp.midi_note(60, 300), bytes([0x03, 1, 1, 30, 60, 255])
        )

    def test_sound_stop(self):
        self.assertEqual(tp.sound_stop(), bytes([0x01]))


class TestDecoding(unittest.TestCase):
    def test_position_id(self):
        # x=200 (0x00C8), y=150 (0x0096), angle=90, sensor x=203, y=151, angle=90
        data = bytes([0x01, 0xC8, 0x00, 0x96, 0x00, 0x5A, 0x00,
                      0xCB, 0x00, 0x97, 0x00, 0x5A, 0x00])
        r = tp.parse_id(data)
        self.assertEqual(r["type"], "position")
        self.assertEqual((r["x"], r["y"], r["angle"]), (200, 150, 90))
        self.assertEqual((r["sensor_x"], r["sensor_y"]), (203, 151))

    def test_standard_id(self):
        data = bytes([0x02, 0x10, 0x27, 0x00, 0x00, 0xB4, 0x00])  # value 10000, angle 180
        r = tp.parse_id(data)
        self.assertEqual(r["type"], "standard")
        self.assertEqual(r["value"], 10000)
        self.assertEqual(r["angle"], 180)

    def test_position_missed(self):
        self.assertEqual(tp.parse_id(bytes([0x03]))["type"], "position_missed")
        self.assertEqual(tp.parse_id(bytes([0x04]))["type"], "standard_missed")

    def test_motion(self):
        data = bytes([0x01, 0x01, 0x00, 0x01, 0x03, 0x05])
        r = tp.parse_motion(data)
        self.assertEqual(r, {
            "flat": True, "collision": False, "double_tap": True,
            "posture": 3, "posture_name": "left up", "shake": 5,
        })

    def test_motion_ignores_other_types(self):
        self.assertIsNone(tp.parse_motion(bytes([0x02, 0, 0, 0])))

    def test_button(self):
        self.assertTrue(tp.parse_button(bytes([0x01, 0x80])))
        self.assertFalse(tp.parse_button(bytes([0x01, 0x00])))

    def test_battery(self):
        self.assertEqual(tp.parse_battery(bytes([70])), 70)


class TestExtendedStreams(unittest.TestCase):
    def test_config_encoders(self):
        self.assertEqual(tp.REQUEST_PROTOCOL_VERSION, bytes([0x01, 0x00]))
        self.assertEqual(tp.config_magnet(2, 100, True), bytes([0x1B, 0, 2, 5, 1]))
        self.assertEqual(tp.config_motor_speed(True), bytes([0x1C, 0, 1]))
        self.assertEqual(tp.config_posture(1, 100, False), bytes([0x1D, 0, 1, 10, 0]))
        self.assertEqual(tp.config_id_notify(10, 0xFF), bytes([0x18, 0, 1, 0xFF]))
        self.assertEqual(tp.request_posture(2), bytes([0x83, 2]))

    def test_magnet(self):
        r = tp.parse_magnet(bytes([0x02, 0x01, 200, 0xF6, 0x00, 0x0A]))  # -10, 0, 10
        self.assertEqual((r["state"], r["force"], r["dir_x"], r["dir_y"], r["dir_z"]),
                         (1, 200, -10, 0, 10))

    def test_posture_euler(self):
        data = bytes([0x03, 0x01]) + (-30).to_bytes(2, "little", signed=True) \
            + (45).to_bytes(2, "little", signed=True) + (180).to_bytes(2, "little", signed=True)
        r = tp.parse_posture(data)
        self.assertEqual((r["kind"], r["roll"], r["pitch"], r["yaw"]), ("euler", -30, 45, 180))

    def test_posture_quaternion(self):
        import struct
        data = bytes([0x03, 0x02]) + struct.pack("<ffff", 1.0, 0.0, 0.0, 0.0)
        r = tp.parse_posture(data)
        self.assertEqual(r["kind"], "quaternion")
        self.assertAlmostEqual(r["w"], 1.0)

    def test_motion_any_dispatch(self):
        self.assertEqual(tp.parse_motion_any(bytes([0x01, 1, 0, 0, 1, 0]))["kind"], "detection")
        self.assertEqual(tp.parse_motion_any(bytes([0x02, 0]))["kind"], "magnet")
        self.assertEqual(tp.parse_motion_any(bytes([0x09]))["kind"], "unknown")

    def test_motor_speed(self):
        self.assertEqual(tp.parse_motor(bytes([0xE0, 30, 45])),
                         {"kind": "speed", "left": 30, "right": 45})

    def test_config_version(self):
        r = tp.parse_config(bytes([0x81, 0x00]) + b"2.4.0")
        self.assertEqual((r["kind"], r["version"]), ("protocol_version", "2.4.0"))

    def test_config_result(self):
        r = tp.parse_config(bytes([0x9D, 0x00, 0x00]))
        self.assertEqual((r["kind"], r["ok"]), ("posture_setting", True))


if __name__ == "__main__":
    unittest.main()
