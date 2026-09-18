"""Deterministic local journey measurement, not a fresh agent or OAuth-host trial.

Build bog-cloud-server and bog-records-worker first. Uses no production endpoint.
Reports only response sizes/timing/status, never credentials or response bodies.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]


def run():
    measurements = []
    startup_health_checks = 0
    with tempfile.TemporaryDirectory(prefix='bog-journey-', dir='/tmp') as directory:
        root = Path(directory)
        os.chmod(root, 0o700)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        token = secrets.token_urlsafe(40)
        environment = {**os.environ, 'BOG_CLOUD_ROOT': str(root / 'data'),
                       'BOG_WORKER_BINARY': str(ROOT / 'target/debug/bog-records-worker'),
                       'BOG_CLOUD_OWNER_TOKEN': token, 'BOG_CLOUD_PUBLIC_ORIGIN': base, 'BOG_CLOUD_BIND': f'127.0.0.1:{port}',
                       'BOG_ALLOW_LEGACY_PUBLIC_OPERATOR': 'true', 'BOG_CLOUD_COMPOSABLE': 'true'}

        def request(stage, method, path, body=None, expected=None, headers=None):
            head = {'Authorization': 'Bearer ' + token, **(headers or {})}
            data = None if body is None else json.dumps(body).encode()
            if data is not None:
                head['Content-Type'] = 'application/json'
            if method == 'POST' and path == '/v1/bogs':
                head['Idempotency-Key'] = body['name']
            started = time.monotonic()
            try:
                response = urllib.request.urlopen(urllib.request.Request(base + path, data=data, headers=head, method=method), timeout=35)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                raw = response.read()
                status = response.status
                elapsed = round((time.monotonic() - started) * 1000, 3)
                content_type = response.headers.get('Content-Type', '')
                value = json.loads(raw) if 'application/json' in content_type else raw.decode()
                request_id = response.headers.get('X-Request-Id')
            if stage:
                measurements.append({'stage': stage, 'method': method, 'path': path,
                                     'status': status, 'response_bytes': len(raw), 'elapsed_ms': elapsed})
            if expected is None:
                assert 200 <= status < 300, (stage, method, path, status)
            else:
                assert status == expected, (stage, method, path, status)
            return value, request_id

        def replay(stage, route, body=None, key='note-1'):
            return request(stage, route['method'], route['path'].replace('{key}', key),
                           route.get('body_example') if body is None else body)

        with open(root / 'service.log', 'w') as log:
            server = subprocess.Popen([str(ROOT / 'target/debug/bog-cloud-server')], env=environment, stdout=log, stderr=log)
            try:
                for _ in range(200):
                    try:
                        startup_health_checks += 1
                        request(None, 'GET', '/v1')
                        break
                    except (OSError, AssertionError):
                        if server.poll() is not None:
                            raise RuntimeError('local server exited')
                        time.sleep(.1)
                else:
                    raise RuntimeError('local startup timeout')
                request('discovery-root', 'GET', '/', headers={'Accept': 'text/markdown'})
                request('discovery-llms', 'GET', '/llms.txt')
                request('discovery-agent', 'GET', '/agent.md')
                created, _ = request('default-create', 'POST', '/v1/bogs', {'name': 'journey-default', 'wait': True})
                assert created['status'] == 'ready'
                routes = created['routes']
                put = next(r for r in routes if r['method'] == 'PUT')
                get = next(r for r in routes if (r.get('body_example') or {}).get('action') == 'get')
                replay('default-write', put)
                got, _ = replay('default-read', get)
                assert got['data']['text'] == 'Hello Bog'
                default_bog_id = created['id']
                first_write_count = len(measurements)
                first_write_bytes = sum(r['response_bytes'] for r in measurements)
                definition, _ = request('notes-recipe', 'GET', '/examples/notes.json')
                created, _ = request('notes-create', 'POST', '/v1/bogs', {'name': 'journey-notes', 'definition': definition, 'wait': True})
                assert created['status'] == 'ready'
                routes = created['routes']
                put = next(r for r in routes if r['method'] == 'PUT')
                # Replay the actual published example first, then an app-specific note.
                replay('notes-route-put-example', put)
                replay('notes-write', put, {'title': 'Hello Bog', 'body': 'A quiet cabin', 'updated_at': 1})
                text_search = next(r for r in routes if r['path'].endswith('/resources/text/search'))
                found, _ = replay('notes-keyword-search', text_search)
                assert found['data'][0]['key'] == 'note-1'
                top = next(r for r in routes if (r.get('body_example') or {}).get('action') == 'top')
                top_result, _ = replay('notes-top', top, {**top['body_example'], 'include_fields': ['/title']})
                assert top_result['data'][0]['value']['/title'] == 'Hello Bog'
                wait = next(r for r in routes if (r.get('body_example') or {}).get('action') == 'wait')
                waited, _ = replay('notes-wait', wait)
                assert waited['changed'] is False and 'cursor' in waited and 'data' not in waited
                batch_get = next(r for r in routes if (r.get('body_example') or {}).get('action') == 'batch_get')
                got, _ = replay('notes-batch-get', batch_get)
                assert got['data'][0]['value']['title'] == 'Hello Bog'
                listing = next(r for r in routes if (r.get('body_example') or {}).get('action') == 'list')
                got, _ = replay('notes-key-bounds', listing, {'action': 'list', 'after': 'note-0', 'before': 'note-2'})
                assert [r['key'] for r in got['data']] == ['note-1']
                bad, request_id = request('expected-docs-404', 'GET', f"/v1/bogs/{created['id']}/docs", expected=404)
                # Non-docs input must not invent a docs listing route.
                assert bad.get('did_you_mean') is None
                bad, _ = request('expected-default-docs-404', 'GET', f'/v1/bogs/{default_bog_id}/docs', expected=404)
                suggestion = bad['did_you_mean']
                assert suggestion['method'] == 'POST'
                got, _ = replay('repaired-docs-list', suggestion)
                assert got['data'][0]['key'] == 'note-1'
                failed, request_id = request('expected-invalid-query', 'POST', listing['path'], {'action': 'list', 'limit': 'invalid'}, expected=400)
                request_id = failed.get('request_id') or request_id
                assert request_id
                trace, _ = request('request-lookup', 'GET', '/v1/requests/' + request_id)
                assert trace['status'] == 400 and trace['code'] == 'invalid_request'
                request('inventory', 'GET', '/v1/bogs')
                output = {'scope': 'deterministic disposable localhost; operator auth; no browser approval or fresh agent',
                          'server_restarts': 0, 'startup_health_checks_excluded': startup_health_checks,
                          'authentication_requests': 0, 'human_wait_ms': None, 'readiness_poll_requests': 0,
                          'first_default_write_and_read': {'requests': first_write_count, 'response_bytes': first_write_bytes,
                          'elapsed_ms': round(sum(r['elapsed_ms'] for r in measurements[:first_write_count]), 3)},
                          'one_guide_default_subset': {'requests': 4,
                              'response_bytes': sum(r['response_bytes'] for r in measurements if r['stage'] in ('discovery-agent', 'default-create', 'default-write', 'default-read')),
                              'note': 'Subset of same run; excludes root/index, auth and defined recipe; not an independent fresh-agent trial'},
                          'totals': {'requests': len(measurements), 'response_bytes': sum(r['response_bytes'] for r in measurements),
                                     'expected_4xx': sum(r['status'] >= 400 for r in measurements)}, 'requests': measurements}
                print(json.dumps(output, indent=2))
            finally:
                server.terminate()
                server.wait()


if __name__ == '__main__':
    run()
