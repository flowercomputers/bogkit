"""Isolated service for a context-free local trial; stop to remove all test data.

Prints only a loopback URL and private auth-file path. Never contacts production.
"""
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def run():
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(SystemExit()))
    with tempfile.TemporaryDirectory(prefix='bog-blind-', dir='/tmp') as directory:
        private = Path(directory)
        os.chmod(private, 0o700)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        origin = f'http://127.0.0.1:{port}'
        token = secrets.token_urlsafe(40)
        auth = private / 'authorization.json'
        fd = os.open(auth, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as f:
            json.dump({'origin': origin, 'access_token': token, 'expires_at': int(time.time()) + 3600}, f)
        environment = {**os.environ, 'BOG_CLOUD_ROOT': str(private / 'data'),
                       'BOG_WORKER_BINARY': str(ROOT / 'target/debug/bog-records-worker'),
                       'BOG_CLOUD_OWNER_TOKEN': token, 'BOG_CLOUD_PUBLIC_ORIGIN': origin, 'BOG_CLOUD_BIND': f'127.0.0.1:{port}',
                       'BOG_ALLOW_LEGACY_PUBLIC_OPERATOR': 'true', 'BOG_CLOUD_COMPOSABLE': 'true'}
        with open(private / 'service.log', 'w') as log:
            server = subprocess.Popen([str(ROOT / 'target/debug/bog-cloud-server')], env=environment, stdout=log, stderr=log)
            try:
                for _ in range(200):
                    try:
                        with urllib.request.urlopen(origin + '/v1', timeout=1) as response:
                            assert response.status == 200
                        break
                    except OSError:
                        if server.poll() is not None:
                            raise RuntimeError('Local trial service exited')
                        time.sleep(.1)
                else:
                    raise RuntimeError('Local trial startup timeout')
                print(json.dumps({'origin': origin, 'private_authorization_file': str(auth),
                                  'scope': 'disposable localhost; already authenticated; stop for complete cleanup'}), flush=True)
                signal.pause()
            finally:
                server.terminate()
                server.wait()


if __name__ == '__main__':
    try:
        run()
    except KeyboardInterrupt:
        pass
