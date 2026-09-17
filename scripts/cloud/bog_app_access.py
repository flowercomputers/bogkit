#!/usr/bin/env python3
"""Install Bog app access privately. Python 3.9+, standard library only."""
import argparse
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


class Failure(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise Failure('Unexpected redirect; verify the service origin and retry.')


def origin(value):
    parsed = urllib.parse.urlsplit(value)
    local = parsed.hostname in ('localhost', '127.0.0.1', '::1')
    if parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ('', '/') or not parsed.hostname or (parsed.scheme != 'https' and not (parsed.scheme == 'http' and local)):
        raise Failure('Use an HTTPS service origin (HTTP is allowed only on loopback).')
    return value.rstrip('/')


def request(base, path, body=None, token=None):
    headers = {'Accept': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers['Content-Type'] = 'application/json'
    req = urllib.request.Request(base + path, data=data, headers=headers)
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=35) as response:
            return response.status, json.loads(response.read(1024 * 1024))
    except urllib.error.HTTPError as error:
        try:
            result = json.loads(error.read(65536))
        except (ValueError, OSError):
            result = {}
        return error.code, result
    except (OSError, ValueError) as error:
        # Never print request headers, bodies, or server text containing a secret.
        raise Failure('Request failed. Check connectivity and retry; if redemption was attempted, prepare a new handoff.') from error


def read_auth(path, base):
    try:
        flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0)
        fd = os.open(path, flags)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise Failure('Cannot open the private helper authorization file safely.') from error
    with os.fdopen(fd) as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise Failure('Helper authorization must be an owned regular file with mode 600.')
        try:
            value = json.load(handle)
        except ValueError:
            return None
    if value.get('origin') == base and value.get('expires_at', 0) > time.time() + 30:
        return value.get('access_token')
    return None


def write_private(path, value, replace=False):
    path = Path(path)
    if not path.parent.is_dir():
        raise Failure('Output directory must already exist; choose a private directory.')
    if replace:
        if path.is_symlink():
            raise Failure('Refusing to replace a symbolic link.')
        fd, temporary = tempfile.mkstemp(prefix='.bog-', dir=path.parent)
        try:
            with os.fdopen(fd, 'w') as handle:
                os.fchmod(handle.fileno(), 0o600)
                json.dump(value, handle)
                handle.write('\n')
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    else:
        try:
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError as error:
            raise Failure('Output already exists. Choose another path or explicitly use --replace.') from error
        with os.fdopen(fd, 'w') as handle:
            json.dump(value, handle)
            handle.write('\n')
            handle.flush()
            os.fsync(handle.fileno())


def authorize(base, auth_file):
    status, device = request(base, '/auth/device', {'name': 'Bog private app installer'})
    if status != 200:
        raise Failure('Could not start device approval. Try again later.')
    # Public approval code is safe to show; private device_code remains only in memory.
    approval = device['verification_uri']
    if origin(urllib.parse.urlunsplit((*urllib.parse.urlsplit(approval)[:2], '', '', ''))) != base:
        raise Failure('Approval URL does not match the service origin.')
    print('This helper needs its own approval; it does not read Codex or Claude credentials.')
    print('Approve at: ' + approval + '?user_code=' + urllib.parse.quote(device['user_code']))
    interval = max(5, int(device.get('interval', 5)))
    deadline = time.monotonic() + min(600, int(device.get('expires_in', 600)))
    while time.monotonic() < deadline:
        time.sleep(interval)
        status, result = request(base, '/auth/device/token', {'device_code': device['device_code']})
        if status == 200:
            token = result['access_token']
            write_private(auth_file, {'origin': base, 'access_token': token, 'expires_at': int(time.time()) + result.get('expires_in', 2592000)}, replace=True)
            return token
        code = result.get('error', {}).get('code')
        if code == 'slow_down':
            interval += 5
        elif code != 'authorization_pending':
            raise Failure('Approval was denied or expired. Start again when ready.')
    raise Failure('Device approval expired. Run the helper again.')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--origin', default='https://flower-bog-cloud.fly.dev')
    parser.add_argument('--handoff', required=True)
    parser.add_argument('--output', required=True, help='Private JSON file for the application; never printed')
    parser.add_argument('--replace', action='store_true', help='Explicitly replace the selected output file')
    parser.add_argument('--auth-file', default=str(Path.home() / '.config/bog-cloud/helper-auth.json'), help='This helper’s own private authorization cache')
    args = parser.parse_args(argv)
    base = origin(args.origin)
    try:
        handoff = str(uuid.UUID(args.handoff))
    except ValueError as error:
        raise Failure('Handoff must be a UUID returned by prepare_app_access.') from error
    output, auth_file = Path(args.output).absolute(), Path(args.auth_file).absolute()
    if output == auth_file:
        raise Failure('App output and helper authorization must be different files.')
    if not output.parent.is_dir() or output.is_symlink() or (output.exists() and not args.replace):
        raise Failure('Choose an existing directory and a new output file, or explicitly use --replace.')
    auth_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    token = read_auth(auth_file, base) or authorize(base, auth_file)
    status, _ = request(base, '/v1/app-access/' + handoff, token=token)
    if status == 401:
        token = authorize(base, auth_file)
        status, _ = request(base, '/v1/app-access/' + handoff, token=token)
    if status != 200:
        raise Failure('Handoff unavailable. Use its initiating account and prepare a fresh handoff if expired or consumed.')
    status, result = request(base, '/v1/app-access/' + handoff + '/redeem', {}, token)
    if status != 200:
        raise Failure('Redemption failed. Check workspace access and prepare a new handoff. Do not reuse a consumed handoff.')
    try:
        write_private(output, {'BOG_CLOUD_URL': base, 'BOG_ID': result['bog_id'], 'BOG_CLOUD_TOKEN': result['token'], 'credential_id': result['id']}, args.replace)
    except (OSError, Failure, KeyError) as error:
        raise Failure('Delivery failed after redemption. Revoke the issued credential in the console, then prepare a new handoff.') from error
    print('App configuration saved privately with mode 600. No credential was printed.')


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Failure as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
    except (OSError, KeyError, TypeError, KeyboardInterrupt):
        # Known failures are deliberately generic: never serialize arbitrary server responses.
        print('Installation could not complete. Check the selected paths, approval and handoff expiry; after a redemption attempt, prepare a new handoff and revoke any unused issued credential.', file=sys.stderr)
        sys.exit(1)
