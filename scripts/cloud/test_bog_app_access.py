import contextlib
import importlib.util
import io
import json
import os
import selectors
import subprocess
import sys
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
        with self.assertRaisesRegex(helper.Failure, 'origin does not match'):
            helper.read_auth(self.auth,'https://other.example')
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
    def test_canonical_default_reuses_authorization(self):
        self.base = 'https://cloud.bog.new'
        self.cache()
        args = self.args()[2:]
        with patch.object(helper, 'request', return_value=(200, {'bog_id':'bog', 'id':'id', 'token':'app-secret'})) as request, patch.object(helper, 'authorize') as authorize, contextlib.redirect_stdout(io.StringIO()):
            helper.main(args)
        authorize.assert_not_called()
        self.assertTrue(all(call.args[0] == self.base for call in request.call_args_list))

    def test_default_cache_requires_new_explicit_approval(self):
        default = self.path / '.config/bog-cloud/helper-auth.json'
        default.parent.mkdir(parents=True)
        helper.write_private(default, {'origin': self.base, 'access_token': 'old-secret', 'expires_at': time.time()+600})
        args = self.args()[:-2]
        with patch.object(helper.Path, 'home', return_value=self.path), patch.object(helper, 'read_auth') as read, patch.object(helper, 'authorize', return_value='new-secret') as authorize, patch.object(helper, 'request', return_value=(200, {'bog_id':'bog','id':'id','token':'app'})), contextlib.redirect_stdout(io.StringIO()):
            helper.main(args)
        read.assert_not_called()
        authorize.assert_called_once_with(self.base, default)

    def test_invalid_cache_fails_without_network_or_secret_output(self):
        for value in [[], {'origin':self.base, 'access_token':'secret', 'expires_at':'later'}, {'origin':self.base, 'access_token':'secret', 'expires_at':float('nan')}]:
            helper.write_private(self.auth, value, replace=True)
            with patch.object(helper, 'request') as request:
                with self.assertRaises(helper.Failure) as error: helper.main(self.args())
                self.assertNotIn('secret', str(error.exception))
                request.assert_not_called()

    def test_expired_authorization_requires_fresh_approval(self):
        helper.write_private(self.auth, {'origin':self.base, 'access_token':'old-secret', 'expires_at':0})
        out = io.StringIO()
        with contextlib.redirect_stdout(out): self.assertIsNone(helper.read_auth(self.auth, self.base))
        self.assertIn('expired', out.getvalue())
        self.assertNotIn('old-secret', out.getvalue())

    def test_auth_symlinks_fifo_and_non600_rejected(self):
        self.cache()
        link = self.path / 'link'
        link.symlink_to(self.auth)
        with self.assertRaises(helper.Failure): helper.read_auth(link, self.base)
        fifo = self.path / 'fifo'
        os.mkfifo(fifo, 0o600)
        with self.assertRaises(helper.Failure): helper.read_auth(fifo, self.base)
        self.auth.chmod(0o400)
        with self.assertRaises(helper.Failure): helper.read_auth(self.auth, self.base)

    def test_approval_prompt_visible_through_pipe_before_poll(self):
        code = """import importlib.util, pathlib, time
spec = importlib.util.spec_from_file_location('helper', %r)
h = importlib.util.module_from_spec(spec); spec.loader.exec_module(h)
h.request = lambda *a, **k: (200, {'verification_uri':'https://cloud.bog.new/auth/device/approve', 'user_code':'PUBLICCODE', 'device_code':'PRIVATESECRET'})
sleep = time.sleep
h.time.sleep = lambda seconds: sleep(30)
h.authorize('https://cloud.bog.new', pathlib.Path(%r))
""" % (str(Path(helper.__file__).absolute()), str(self.auth))
        # No -u or PYTHONUNBUFFERED: a redirected agent log must work normally.
        env = os.environ.copy(); env.pop('PYTHONUNBUFFERED', None)
        process = subprocess.Popen([sys.executable, '-c', code], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        try:
            selector = selectors.DefaultSelector(); selector.register(process.stdout, selectors.EVENT_READ)
            data = ''
            deadline = time.monotonic() + 5
            while 'PUBLICCODE' not in data and time.monotonic() < deadline:
                self.assertTrue(selector.select(timeout=max(0, deadline - time.monotonic())), 'approval prompt remained buffered')
                chunk = os.read(process.stdout.fileno(), 8192).decode()
                if not chunk: break
                data += chunk
            self.assertIn('PUBLICCODE', data)
            self.assertNotIn('PRIVATESECRET', data)
            self.assertIsNone(process.poll())
            selector.close()
        finally:
            process.kill(); process.communicate()

    def test_uncertain_redemption_explains_recovery(self):
        self.cache()
        with patch.object(helper, 'request', side_effect=[(200, {}), helper.Failure('transport failed')]):
            with self.assertRaisesRegex(helper.Failure, 'outcome is uncertain.*revoke'):
                helper.main(self.args())

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


class ConnectionTests(unittest.TestCase):
    def test_approval_continues_without_chat_and_verifies_privately(self):
        with tempfile.TemporaryDirectory() as root:
            auth=Path(root)/'auth.json'; output=io.StringIO()
            responses=[(200,{'verification_uri':'https://bog.example/approve','user_code':'PUBLIC','device_code':'PRIVATE','interval':5,'expires_in':600}), (400,{'error':{'code':'authorization_pending'}}), (429,{'error':{'code':'slow_down'}}), (200,{'access_token':'SECRET','expires_in':600}), (200,{'kind':'agent'})]
            with patch.object(helper,'request',side_effect=responses) as req, patch.object(helper.time,'sleep') as sleep, contextlib.redirect_stdout(output):
                helper.main(['--origin','https://bog.example','--connect','--auth-file',str(auth)])
            self.assertIn('Connected',output.getvalue()); self.assertNotIn('SECRET',output.getvalue()); self.assertNotIn('PRIVATE',output.getvalue())
            self.assertEqual([c.args[0] for c in sleep.call_args_list],[5,5,10])
            self.assertEqual(req.call_args.args[1],'/v1/me')
            self.assertEqual(auth.stat().st_mode & 0o777,0o600)
    def test_denial_expiry_and_cancellation_do_not_connect(self):
        for code in ['access_denied','expired_token']:
            with tempfile.TemporaryDirectory() as root, patch.object(helper.time,'sleep'), patch.object(helper,'request',side_effect=[(200,{'verification_uri':'https://bog.example/approve','user_code':'PUBLIC','device_code':'PRIVATE'}),(400,{'error':{'code':code}})]), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(helper.Failure): helper.main(['--origin','https://bog.example','--connect','--auth-file',root+'/auth.json'])
                self.assertFalse(Path(root,'auth.json').exists())
        with tempfile.TemporaryDirectory() as root, patch.object(helper,'authorize',side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt): helper.main(['--connect','--auth-file',root+'/auth.json'])

class DeviceCompatibilityTests(unittest.TestCase):
    setUp = InstallerTests.setUp
    tearDown = InstallerTests.tearDown
    cache = InstallerTests.cache
    args = InstallerTests.args
    def device(self):
        return (200, {'verification_uri': self.base + '/approve', 'user_code': 'PUBLIC', 'device_code': 'PRIVATE', 'interval': 5, 'expires_in': 600})

    def test_flat_and_nested_pending_and_throttle(self):
        for nested in (False, True):
            def error(code):
                return (400, {'error': {'code': code} if nested else code})
            responses = [self.device(), error('authorization_pending'), error('slow_down'), (200, {'access_token': 'SECRET', 'expires_in': 600})]
            with patch.object(helper, 'request', side_effect=responses), patch.object(helper.time, 'sleep') as sleep, contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(helper.authorize(self.base, self.auth), 'SECRET')
            self.assertEqual([c.args[0] for c in sleep.call_args_list], [5, 5, 10])
            self.assertNotIn('SECRET', output.getvalue())
            self.assertNotIn('PRIVATE', output.getvalue())

    def test_flat_denial_expiry_and_malformed_error_stop(self):
        for error in ('access_denied', 'expired_token', None, [], 42):
            with patch.object(helper, 'request', side_effect=[self.device(), (400, {'error': error})]), patch.object(helper.time, 'sleep'), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(helper.Failure): helper.authorize(self.base, self.auth)
            self.assertFalse(self.auth.exists())

    def test_interrupted_poll_does_not_save_or_connect(self):
        for failure in (KeyboardInterrupt(), helper.Failure('connection interrupted')):
            output = io.StringIO()
            with patch.object(helper, 'request', side_effect=[self.device(), failure]), patch.object(helper.time, 'sleep'), contextlib.redirect_stdout(output):
                with self.assertRaises(type(failure)): helper.main(['--origin', self.base, '--connect', '--auth-file', str(self.auth)])
            self.assertFalse(self.auth.exists())
            self.assertNotIn('Connected', output.getvalue())

    def test_connect_reuses_cache_and_renews_rejected_cache(self):
        self.cache()
        args = ['--origin', self.base, '--connect', '--auth-file', str(self.auth)]
        with patch.object(helper, 'request', return_value=(200, {})), patch.object(helper, 'authorize') as authorize, contextlib.redirect_stdout(io.StringIO()):
            helper.main(args)
        authorize.assert_not_called()
        with patch.object(helper, 'request', side_effect=[(401, {}), (200, {})]), patch.object(helper, 'authorize', return_value='fresh') as authorize, contextlib.redirect_stdout(io.StringIO()):
            helper.main(args)
        authorize.assert_called_once_with(self.base, self.auth)

    def test_expired_cache_is_renewed_before_verification(self):
        helper.write_private(self.auth, {'origin': self.base, 'access_token': 'expired', 'expires_at': 0})
        with patch.object(helper, 'authorize', return_value='fresh') as authorize, patch.object(helper, 'request', return_value=(200, {})) as request, contextlib.redirect_stdout(io.StringIO()):
            helper.main(['--origin', self.base, '--connect', '--auth-file', str(self.auth)])
        authorize.assert_called_once_with(self.base, self.auth)
        self.assertEqual(request.call_args.kwargs['token'], 'fresh')

    def test_dotenv_private_escaping_and_no_overwrite(self):
        value = "quote' slash\\ $value `command`"
        helper.write_private(self.output, {'BOG_CLOUD_TOKEN': value}, output_format='dotenv')
        self.assertEqual(self.output.read_text(), "BOG_CLOUD_TOKEN='quote\\' slash\\\\ $value `command`'\n")
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(helper.Failure): helper.write_private(self.output, {}, output_format='dotenv')
        for bad in ('line\nvalue', 'line\rvalue', 'null\0value'):
            with self.assertRaises(helper.Failure): helper.write_private(self.output, {'TOKEN': bad}, True, 'dotenv')

    def test_dotenv_install_and_target_machine_message(self):
        self.cache()
        with patch.object(helper, 'request', return_value=(200, {'bog_id': 'bog', 'id': 'id', 'token': 'secret'})), contextlib.redirect_stdout(io.StringIO()) as out:
            helper.main(self.args() + ['--format', 'dotenv'])
        self.assertIn("BOG_CLOUD_TOKEN='secret'", self.output.read_text())
        self.assertIn('target machine', out.getvalue())
        self.assertNotIn('secret', out.getvalue())

if __name__ == '__main__': unittest.main()
