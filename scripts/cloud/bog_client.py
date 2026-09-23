"""Small server-side Bog client. Python 3.9+, standard library only.

Credentials stay in owned mode-600 files. Exceptions never include server bodies.
"""
import copy
import json
import math
import os
from pathlib import Path
import re
import stat
import time
import urllib.error
import urllib.parse
import urllib.request
from bog_app_access import Failure, NoRedirect, origin, read_auth


class BogError(Failure):
    def __init__(self, status, code, retry_after=1):
        self.status, self.code, self.retry_after = status, code, retry_after
        self.retry_after_ms = None if retry_after is None else retry_after * 1000
        self.retry_exhausted = False
        super().__init__(f'Bog request failed ({status}, {code}).')


class Client:
    def __init__(self, base, token, bog_id=None, workspace_id=None):
        if not isinstance(token, str) or not token.strip() or any(ord(c) < 32 or ord(c) == 127 for c in token):
            raise Failure('Invalid private credential.')
        self.base, self._token = origin(base), token
        self.bog_id, self.workspace_id = bog_id, workspace_id

    @classmethod
    def from_auth(cls, path, base='https://cloud.bog.new', workspace_id=None):
        token = read_auth(Path(path), origin(base))
        if not token:
            raise Failure('Run bog-app-access.py --connect with this --auth-file first.')
        return cls(base, token, workspace_id=workspace_id)

    @classmethod
    def from_config(cls, path):
        fd = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
        with os.fdopen(fd) as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
                raise Failure('App configuration must be an owned regular mode-600 file.')
            raw = handle.read()
            try:
                value = json.loads(raw)
            except ValueError:
                value = {}
                for line in raw.splitlines():
                    if not line.strip() or line.lstrip().startswith('#'): continue
                    key, separator, text = line.partition('=')
                    if not separator: raise Failure('Invalid private configuration.')
                    try: value[key.strip()] = json.loads(text)
                    except ValueError:
                        text = text.strip()
                        if text.startswith("'"):
                            if len(text) < 2 or not text.endswith("'"): raise Failure('Invalid dotenv quotation.')
                            result = ''; i = 1
                            while i < len(text)-1:
                                c = text[i]
                                if c == "\\":
                                    i += 1
                                    if i >= len(text)-1 or text[i] not in ("\\", "'"): raise Failure('Invalid dotenv escape.')
                                    c = text[i]
                                elif c == "'": raise Failure('Invalid dotenv quotation.')
                                result += c; i += 1
                            value[key.strip()] = result
                        else: value[key.strip()] = text
        if not isinstance(value, dict) or any(not isinstance(value.get(k), str) or not value[k].strip() or any(ord(c) < 32 or ord(c) == 127 for c in value[k]) for k in ('BOG_CLOUD_URL', 'BOG_CLOUD_TOKEN', 'BOG_ID')):
            raise Failure('Private configuration requires valid BOG_CLOUD_URL, BOG_CLOUD_TOKEN and BOG_ID strings.')
        return cls(value['BOG_CLOUD_URL'], value['BOG_CLOUD_TOKEN'], value['BOG_ID'])

    def request(self, method, path, body=None, idempotency_key=None):
        deadline = time.monotonic() + 30
        for attempt in range(3):
            try: return self._request_once(method, path, body, idempotency_key, timeout=max(0.001, deadline-time.monotonic()))
            except BogError as error:
                if error.status != 429 or error.retry_after is None or attempt == 2 or time.monotonic() + error.retry_after > deadline:
                    error.retry_exhausted = True
                    raise
                time.sleep(error.retry_after)

    def _request_once(self, method, path, body=None, idempotency_key=None, timeout=35):
        if self.workspace_id:
            path += ('&' if '?' in path else '?') + urllib.parse.urlencode({'workspace_id': self.workspace_id})
        headers = {'Authorization': 'Bearer ' + self._token, 'Accept': 'application/json'}
        if idempotency_key:
            headers['Idempotency-Key'] = idempotency_key
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers['Content-Type'] = 'application/json'
        req = urllib.request.Request(self.base + path, data=data, headers=headers, method=method)
        try:
            response = urllib.request.build_opener(NoRedirect).open(req, timeout=timeout)
        except urllib.error.HTTPError as error:
            # Accept only known error codes; never echo arbitrary server text.
            code = 'request_rejected'
            payload = {}
            try:
                payload = json.loads(error.read(65536))
                candidate = payload.get('error', {}).get('code')
                if candidate in ('capacity','rate_limited','conflict','writes_paused','unauthorized','forbidden','not_found','invalid_request'):
                    code = candidate
            except (ValueError, AttributeError): pass
            delay = None
            milliseconds = payload.get('retry_after_ms') if isinstance(payload, dict) else None
            if not isinstance(milliseconds, bool) and isinstance(milliseconds, (int, float)) and math.isfinite(milliseconds) and milliseconds >= 0:
                delay = milliseconds / 1000
            else:
                try:
                    candidate_delay = float(error.headers.get('Retry-After', ''))
                    if math.isfinite(candidate_delay) and candidate_delay >= 0: delay = candidate_delay
                except ValueError: pass
            raise BogError(error.code, code, delay) from None
        except (OSError, ValueError):
            raise Failure('Connection failed; check readiness/connectivity. A write may have completed; do not blindly repeat it.') from None
        with response:
            raw = response.read(5 * 1024 * 1024 + 1)
            if len(raw) > 5 * 1024 * 1024:
                raise Failure('Response too large; request a smaller page.')
            return json.loads(raw) if raw else None

    def path(self, suffix=''):
        if not self.bog_id:
            raise Failure('Select a Bog first.')
        return '/v1/bogs/' + urllib.parse.quote(self.bog_id, safe='') + suffix

    def top(self, resource, limit=100, offset=0, include_fields=None): return self.query(resource, action='top', limit=limit, offset=offset, **({'include_fields': include_fields} if include_fields is not None else {}))
    def preview_cleanup(self, name_prefix): return self.request('POST', '/v1/bogs/cleanup/preview', {'name_prefix': name_prefix})
    def execute_cleanup(self, preview_id, confirm):
        if not isinstance(preview_id, str) or not preview_id or confirm != preview_id: raise Failure('Confirm the exact cleanup preview ID.')
        return self.request('POST', '/v1/bogs/cleanup/execute', {'preview_id': preview_id})
    def delete_sandbox(self, confirm):
        if not self.bog_id or confirm != self.bog_id: raise Failure('Confirm the exact sandbox Bog ID.')
        return self.request('DELETE', self.path(), {'confirm': confirm})
    def explain(self, request_id): return self.request_status(request_id)

    def resources(self): return self.request('GET', self.path('/resources'))
    def get(self, key): return self.request('GET', self.path('/docs/' + urllib.parse.quote(key, safe='')))
    def put(self, key, record): return self.request('PUT', self.path('/docs/' + urllib.parse.quote(key, safe='')), record)
    def remove(self, key): return self.request('DELETE', self.path('/docs/' + urllib.parse.quote(key, safe='')))
    def batch(self, ops): return self.request('POST', self.path('/batch'), {'ops': ops})
    def query(self, resource='docs', **query): return self.request('POST', self.path('/resources/' + urllib.parse.quote(resource, safe='') + '/query'), query)
    def search(self, resource, query, limit=3, include_fields=None): return self.request('POST', self.path('/resources/' + urllib.parse.quote(resource, safe='') + '/search'), {'query': query, 'limit': limit, **({'include_fields': include_fields} if include_fields is not None else {})})
    def list(self, resource='docs', limit=100, after=None, before=None):
        return self.query(resource, limit=limit, **({'after': after} if after is not None else {}), **({'before': before} if before is not None else {}))

    def batch_get(self, keys, resource='docs'):
        if not isinstance(keys, list) or not 1 <= len(keys) <= 100 or any(not isinstance(k, str) or not k for k in keys): raise Failure('batch_get requires 1 through 100 nonempty keys.')
        return self.query(resource, action='batch_get', keys=keys)

    def wait(self, cursor=None, timeout=25):
        if not isinstance(timeout, int) or not 0 <= timeout <= 25: raise Failure('Wait timeout must be 0 through 25 seconds.')
        return self.request('GET', self.path('/changes?') + urllib.parse.urlencode({'timeout': timeout, **({'cursor': cursor} if cursor is not None else {})}))

    def routes(self): return self.request('GET', self.path('/routes'))
    def request_status(self, request_id): return self.request('GET', '/v1/requests/' + urllib.parse.quote(request_id, safe=''))
    def claim_link(self):
        if not self.bog_id: raise Failure('Select a temporary Bog ID.')
        return self.request('POST', '/v1/claimable-bogs/' + urllib.parse.quote(self.bog_id, safe='') + '/claim')
    def diagnostics(self): return self.request('GET', self.path('/usage'))

    def create(self, name, idempotency_key, definition=None, timeout=60, *, wait=True, sandbox=False, app_access=None):
        if not isinstance(idempotency_key, str) or not idempotency_key.strip(): raise Failure('Creation requires a stable idempotency key.')
        if not isinstance(wait, bool) or not isinstance(sandbox, bool): raise Failure('wait and sandbox must be booleans.')
        body = {'name': name, 'wait': wait, 'sandbox': sandbox}
        if app_access is not None: body['app_access'] = app_access
        if definition is not None: body['definition'] = definition
        result = self.request('POST', '/v1/bogs', body, idempotency_key)
        self.bog_id = result['id']
        access = result.get('app_access')
        if not wait: return {**result, 'bog_id': self.bog_id}
        deadline = time.monotonic() + timeout
        while result['status'] != 'ready':
            if result['status'] == 'failed':
                raise Failure('Bog startup failed. Retain its ID and retry the same creation request/key after checking capacity.')
            if time.monotonic() >= deadline:
                raise Failure('Readiness timed out; creation may still finish. Retain the same request/key.')
            time.sleep(0.5)
            result = self.request('GET', self.path())
        return {'bog_id': self.bog_id, 'status': 'ready', 'resources': self.resources(), **({'app_access': access} if access is not None else {})}

    def add_search(self, name, fields, kind='semantic', stages=None, timeout=120):
        if kind not in ('semantic', 'bm25') or not re.fullmatch(r'[a-z][a-z0-9_]{0,47}', name):
            raise Failure('Use semantic/bm25 and a lowercase resource name.')
        if not fields or any(not isinstance(f, str) or not f.startswith('/') for f in fields):
            raise Failure('Search fields must be JSON Pointers.')
        current = self.request('GET', self.path('/definition'))
        definition = copy.deepcopy(current['definition'])
        if name in definition['resources'] or name in definition['expose']:
            raise Failure('That resource or operation already exists; choose a new name.')
        definition['resources'][name] = {'stages': stages or [], 'terminal': {'kind': kind, 'fields': fields}}
        definition['expose'][name] = {'target': name, 'action': 'search'}
        body = {'definition': definition, 'expected_revision': current['revision']}
        self.request('POST', self.path('/definition/plan'), body)
        deadline = time.monotonic() + timeout
        attempts = 0
        while True:
            try:
                job = self.request('POST', self.path('/definition/apply'), body)
                break
            except BogError as error:
                attempts += 1
                if error.status != 429 or error.retry_exhausted or error.retry_after is None or attempts > 2 or time.monotonic() + error.retry_after >= deadline:
                    raise
                time.sleep(error.retry_after)
        while job['status'] != 'succeeded':
            if job['status'] in ('failed', 'recovery_required'):
                raise Failure('Search activation ' + job['status'] + '; inspect definition job ' + job['job_id'])
            if time.monotonic() >= deadline:
                raise Failure('Search activation still pending; inspect definition job ' + job['job_id'] + ' before retrying.')
            time.sleep(0.5)
            job = self.request('GET', self.path('/definition/jobs/' + urllib.parse.quote(job['job_id'], safe='')))
        return job
