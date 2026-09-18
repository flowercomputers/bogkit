#!/usr/bin/env python3
"""Install Bog app access privately. Python 3.9+, standard library only."""
import argparse
import json
import math
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
        if path.is_symlink():
            raise Failure('Helper authorization must not be a symbolic link.')
        flags = os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0)
        fd = os.open(path, flags)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise Failure('Cannot open the private helper authorization file safely.') from error
    with os.fdopen(fd) as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
            raise Failure('Helper authorization must be an owned regular file with mode 600.')
        try:
            value = json.load(handle)
        except ValueError as error:
            raise Failure('Invalid helper authorization JSON; select a valid private authorization file.') from error
    if not isinstance(value, dict):
        raise Failure('Helper authorization must be a JSON object.')
    if value.get('origin') != base:
        raise Failure('Helper authorization origin does not match --origin. Select the matching authorization file or a new cache path.')
    expiry, token = value.get('expires_at'), value.get('access_token')
    if isinstance(expiry, bool) or not isinstance(expiry, (int, float)) or not math.isfinite(expiry) or not isinstance(token, str) or not token.strip() or any(ord(c) < 32 or ord(c) == 127 for c in token):
        raise Failure('Helper authorization needs a valid access_token and numeric expires_at timestamp.')
    if expiry > time.time() + 30:
        return token
    print('Saved helper authorization has expired or is about to expire; a fresh approval is required.', flush=True)
    return None


def write_private(path, value, replace=False, output_format='json'):
    if output_format == 'dotenv':
        # Quote dotenv values and escape backslashes and quotes; never execute this file as a script.
        if any(not isinstance(v, str) or any(c in v for c in '\r\n\0') for v in value.values()):
            raise Failure('Dotenv values must be single-line strings.')
        content = ''.join(k + "='" + v.replace("\\", "\\\\").replace("'", "\\'") + "'\n" for k, v in value.items())
    else:
        content = json.dumps(value) + '\n'

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
                handle.write(content)
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
            os.fchmod(handle.fileno(), 0o600)
            handle.write(content)
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
    print('Approve this helper, or reuse your own device authorization with --auth-file. Codex and Claude private storage is never read.', flush=True)
    print('Approve at: ' + approval + '?user_code=' + urllib.parse.quote(device['user_code']), flush=True)
    interval = max(5, int(device.get('interval', 5)))
    deadline = time.monotonic() + min(600, int(device.get('expires_in', 600)))
    while time.monotonic() < deadline:
        time.sleep(interval)
        status, result = request(base, '/auth/device/token', {'device_code': device['device_code']})
        if status == 200:
            token = result['access_token']
            write_private(auth_file, {'origin': base, 'access_token': token, 'expires_at': int(time.time()) + result.get('expires_in', 2592000)}, replace=True)
            return token
        error = result.get('error')
        code = error if isinstance(error, str) else error.get('code') if isinstance(error, dict) else None
        if code == 'slow_down':
            interval += 5
        elif code != 'authorization_pending':
            raise Failure('Approval was denied or expired. Start again when ready.')
    raise Failure('Device approval expired. Run the helper again.')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--origin', default='https://cloud.bog.new')
    parser.add_argument('--connect', action='store_true', help='Wait for agent approval, save authorization privately, and verify access')
    parser.add_argument('--handoff')
    parser.add_argument('--output', help='Private configuration file on this machine; never printed')
    parser.add_argument('--format', choices=('json', 'dotenv'), default='json', help='App configuration format (default: json)')
    parser.add_argument('--replace', action='store_true', help='Explicitly replace the selected output file')
    parser.add_argument('--auth-file', default=None, help='Owned regular mode-600 JSON file containing origin, access_token and expires_at (Unix seconds). Explicitly reuses your own device authorization for the selected origin. Without this flag a new approval is required and saved in ~/.config/bog-cloud/helper-auth.json. Never select Codex or Claude private storage.')
    args = parser.parse_args(argv)
    base = origin(args.origin)
    if args.connect:
        if args.handoff or args.output or args.replace or not args.auth_file:
            raise Failure('Use --connect with an explicit --auth-file, without app installation arguments.')
        auth_file = Path(args.auth_file).absolute()
        auth_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        token = read_auth(auth_file, base) or authorize(base, auth_file)
        status, _ = request(base, '/v1/me', token=token)
        if status == 401:
            token = authorize(base, auth_file)
            status, _ = request(base, '/v1/me', token=token)
        if status != 200:
            raise Failure('Authorization was collected, but connection verification failed. Retry --connect.')
        print('Connected. Authorization saved privately; continue the original task.', flush=True)
        return
    if not args.handoff or not args.output:
        raise Failure('Installation requires --handoff and --output; connection uses --connect --auth-file.')
    try:
        handoff = str(uuid.UUID(args.handoff))
    except ValueError as error:
        raise Failure('Handoff must be a UUID returned by prepare_app_access.') from error
    output, auth_file = Path(args.output).absolute(), Path(args.auth_file or Path.home() / '.config/bog-cloud/helper-auth.json').absolute()
    if output == auth_file:
        raise Failure('App output and helper authorization must be different files.')
    if not output.parent.is_dir() or output.is_symlink() or (output.exists() and not args.replace):
        raise Failure('Choose an existing directory and a new output file, or explicitly use --replace.')
    auth_file.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    token = (read_auth(auth_file, base) if args.auth_file is not None else None) or authorize(base, auth_file)
    status, _ = request(base, '/v1/app-access/' + handoff, token=token)
    if status == 401:
        token = authorize(base, auth_file)
        status, _ = request(base, '/v1/app-access/' + handoff, token=token)
    if status != 200:
        raise Failure('Handoff unavailable. Use its initiating account and prepare a fresh handoff if expired or consumed.')
    try:
        status, result = request(base, '/v1/app-access/' + handoff + '/redeem', {}, token)
    except Failure as error:
        raise Failure('Redemption outcome is uncertain. Prepare a new handoff; ask a workspace owner to revoke any unused credential that may have been issued.') from error
    if status != 200:
        raise Failure('Redemption failed. Check workspace access and prepare a new handoff. Do not reuse a consumed handoff.')
    try:
        write_private(output, {'BOG_CLOUD_URL': base, 'BOG_ID': result['bog_id'], 'BOG_CLOUD_TOKEN': result['token'], 'credential_id': result['id']}, args.replace, args.format)
    except (OSError, Failure, KeyError) as error:
        raise Failure('Delivery failed after redemption. Ask a workspace owner to revoke the issued credential, then prepare a new handoff.') from error
    print('App configuration saved privately on this machine with mode 600. Run this helper on the target machine to install there. No credential was printed.', flush=True)


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Failure as error:
        print(str(error), file=sys.stderr, flush=True)
        sys.exit(1)
    except (OSError, KeyError, TypeError, KeyboardInterrupt):
        # Known failures are deliberately generic: never serialize arbitrary server responses.
        print('Installation could not complete. Check the selected paths, approval and handoff expiry; after a redemption attempt, prepare a new handoff; revoke any unused issued credential as a workspace owner, or ask an owner to revoke it.', file=sys.stderr, flush=True)
        sys.exit(1)
