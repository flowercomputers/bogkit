"""Offline safety/shape checks; this test never contacts production."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('probe', Path(__file__).parents[1] / 'composable_live_acceptance.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ProbeTests(unittest.TestCase):
    def test_state_has_no_auth(self):
        with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {'BOG_CLOUD_TOKEN': 'secret-fixture'}):
            p = module.Probe(Path(folder) / 'state.json')
            p.state = {'run_id': 'test'}
            p.save()
            self.assertNotIn('secret-fixture', p.path.read_text())
            self.assertEqual(p.path.stat().st_mode & 0o777, 0o600)

    def test_mcp_unwrap(self):
        p = object.__new__(module.Probe)
        p.requests = []
        p.rpc = lambda *args: {'structuredContent': {'status': 200, 'request_id': 'test', 'data': {'data': 2}}}
        self.assertEqual(p.tool('query_resource'), {'data': 2})

    def test_cleanup_rejects_other_bog(self):
        p = object.__new__(module.Probe)
        p.state = {'run_id': '00000000-0000-0000-0000-000000000001', 'bog_id': '00000000-0000-0000-0000-000000000002'}
        calls = []
        p.http = lambda method, *args, **kwargs: calls.append(method) or {'name': 'existing-chat'}
        with self.assertRaises(RuntimeError):
            p.cleanup()
        self.assertEqual(calls, ['GET'])

    def test_redirect_rejected(self):
        with self.assertRaises(RuntimeError):
            module.NoRedirect().redirect_request(None)


if __name__ == '__main__':
    unittest.main()
