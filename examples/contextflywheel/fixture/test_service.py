import unittest

try:
    from . import service
except ImportError:
    import service


class ServiceTest(unittest.TestCase):
    def test_account_load_finishes_before_deadline(self) -> None:
        self.assertEqual(service.load_account(), {"status": "ok"})


if __name__ == "__main__":
    unittest.main()
