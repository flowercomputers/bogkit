import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('helper', Path(__file__).with_name('bog_app_access.py'))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)

class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.path = Path(self.root.name)
        self.auth = self.path / 'auth.json'
        self.output = self.path / 'app.json'
        self.base = 'https://bog.example'
        self.handoff = '00000000-0000-4000-8000-000000000001'
    def tearDown(self):
        self.root.cleanup()
    def cache(self):
        helper.write_private(self.auth, {'origin':self.base, 'access_token':'fixture-agent-secret', 'expires_at':time.time()+600})
    def args(self):
        return ['--origin',self.base,'--handoff',self.handoff,'--output',str(self.output),'--auth-file',str(self.auth)]
    def test_private_file_no_overwrite_and_explicit_replacement(self):
        helper.write_private(self.output, {'token':'one'})
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(helper.Failure): helper.write_private(self.output, {'token':'two'})
        helper.write_private(self.output, {'token':'two'}, True)
        self.assertEqual(json.loads(self.output.read_text())['token'], 'two')
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
    def test_symlink_not_overwritten(self):
        target=self.path/'target'; target.write_text('untouched'); self.output.symlink_to(target)
        with self.assertRaises(helper.Failure): helper.write_private(self.output, {}, True)
        self.assertEqual(target.read_text(),'untouched')
    def test_auth_origin_and_permissions(self):
        self.cache()
        self.assertEqual(helper.read_auth(self.auth,self.base),'fixture-agent-secret')
        self.assertIsNone(helper.read_auth(self.auth,'https://other.example'))
        self.auth.chmod(0o644)
        with self.assertRaises(helper.Failure): helper.read_auth(self.auth,self.base)
    def test_install_never_prints_secret(self):
        self.cache(); calls=[]
        def request(base,path,body=None,token=None):
            calls.append((path,body,token))
            return (200, {'bog_id':'fixture-bog','id':'fixture-id','token':'fixture-app-secret'})
        output=io.StringIO()
        with patch.object(helper,'request',request), contextlib.redirect_stdout(output): helper.main(self.args())
        self.assertEqual(len(calls),2)
        self.assertEqual(calls[-1][0],'/v1/app-access/'+self.handoff+'/redeem')
        self.assertEqual(json.loads(self.output.read_text())['BOG_CLOUD_TOKEN'],'fixture-app-secret')
        self.assertNotIn('fixture-app-secret',output.getvalue())
        self.assertNotIn('fixture-agent-secret',output.getvalue())
    def test_existing_output_stops_before_redemption(self):
        self.output.write_text('existing')
        with patch.object(helper,'request') as request:
            with self.assertRaises(helper.Failure): helper.main(self.args())
            request.assert_not_called()
    def test_device_code_is_private(self):
        replies=iter([(200,{'verification_uri':self.base+'/auth/device/approve','user_code':'ABCD1234','device_code':'private-device-secret','interval':5,'expires_in':600}), (400,{'error':{'code':'authorization_pending'}}), (200,{'access_token':'fixture-agent-secret','expires_in':600})])
        output=io.StringIO()
        with patch.object(helper,'request',lambda *args,**kw:next(replies)), patch.object(helper.time,'sleep') as sleep, contextlib.redirect_stdout(output):
            self.assertEqual(helper.authorize(self.base,self.auth),'fixture-agent-secret')
        self.assertEqual(sleep.call_count,2)
        self.assertIn('ABCD1234',output.getvalue())
        self.assertNotIn('private-device-secret',output.getvalue())
        self.assertNotIn('fixture-agent-secret',output.getvalue())
    def test_redirects_and_unsafe_origins_fail(self):
        with self.assertRaises(helper.Failure): helper.NoRedirect().redirect_request(None,None,None,None,None,None)
        for value in ['http://remote.example','https://user:pass@bog.example','https://bog.example/?secret=x']:
            with self.assertRaises(helper.Failure): helper.origin(value)
    def test_missing_handoff_never_redeems(self):
        self.cache()
        with patch.object(helper,'request',return_value=(404,{})) as request:
            with self.assertRaises(helper.Failure): helper.main(self.args())
            self.assertEqual(request.call_count,1)

if __name__=='__main__': unittest.main()
