#!/usr/bin/env python3
"""Explicit, bounded production canary. Auth is environment-only; output is metadata.

Run --create, optionally restart the service, then --verify, then --cleanup with
one --state path. Never runs automatically. Exactly one disposable Bog is made.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]
REST = 'https://cloud.bog.new'
MCP = 'https://mcp.bog.new/mcp'


def check(condition, label):
    if not condition:
        raise RuntimeError(label)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def fixture(name):
    return json.loads((ROOT / 'docs/examples/composable' / name).read_text())


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise RuntimeError('redirect refused')


class Probe:
    def __init__(self, path):
        self.path = Path(path)
        self.owner = os.environ['BOG_CLOUD_TOKEN']
        check(bool(self.owner), 'missing management token')
        self.state = json.loads(self.path.read_text()) if self.path.exists() else {}
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        self.session = None
        self.protocol = '2025-11-25'
        self.requests = []
        self.credentials = {}
        self.isolation_bog = None

    def save(self):
        # State contains only generated identifiers and hashes, never credentials/data.
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, 'w') as stream:
            json.dump(self.state, stream)

    @property
    def base(self):
        return '/v1/bogs/' + str(uuid.UUID(self.state['bog_id']))

    def http(self, method, path, body=None, token=None, expected=(200,), key=None, mcp=False):
        rid = str(uuid.uuid4())
        headers = {'Authorization': 'Bearer ' + (token or self.owner),
                   'Content-Type': 'application/json', 'X-Request-Id': rid}
        if key:
            headers['Idempotency-Key'] = key
        if mcp:
            headers['Accept'] = 'application/json, text/event-stream'
            headers['MCP-Protocol-Version'] = self.protocol
            if self.session:
                headers['Mcp-Session-Id'] = self.session
        request = urllib.request.Request(MCP if mcp else REST + path, method=method,
                    headers=headers, data=None if body is None else json.dumps(body).encode())
        try:
            response = self.opener.open(request, timeout=180)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            raw = response.read(2 * 1024 * 1024 + 1)
            self.requests.append({'method': method, 'path': '/mcp' if mcp else path,
                                  'operation': body.get('method') if mcp and isinstance(body, dict) else None,
                                  'client_request_id': rid,
                                  'server_request_id': response.headers.get('X-Request-Id'),
                                  'status': response.status})
            check(response.status in expected, 'unexpected HTTP status')
            check(len(raw) <= 2 * 1024 * 1024, 'response too large')
            if mcp and response.headers.get('Mcp-Session-Id'):
                self.session = response.headers['Mcp-Session-Id']
            if not raw:
                return None
            if 'text/event-stream' in response.headers.get('Content-Type', ''):
                values = [json.loads(line[5:].strip()) for line in raw.decode().splitlines() if line.startswith('data:')]
                return next(v for v in values if v.get('id') == body.get('id'))
            return json.loads(raw)

    def rpc(self, method, params):
        result = self.http('POST', '', {'jsonrpc': '2.0', 'id': str(uuid.uuid4()),
                                      'method': method, 'params': params}, mcp=True)
        check('error' not in result, 'MCP protocol error')
        return result['result']

    def tool(self, name, **arguments):
        result = self.rpc('tools/call', {'name': name, 'arguments': arguments})
        check(not result.get('isError'), 'MCP tool error')
        structured = result['structuredContent']
        self.requests.append({'mcp_request_id': structured['request_id'], 'status': structured['status']})
        return structured['data']

    def connect(self):
        result = self.rpc('initialize', {'protocolVersion': self.protocol, 'capabilities': {},
                 'clientInfo': {'name': 'bounded-composable-canary', 'version': '1'}})
        self.protocol = result['protocolVersion']
        self.http('POST', '', {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
                  expected=(200, 202, 204), mcp=True)
        names = {t['name'] for t in self.rpc('tools/list', {})['tools']}
        check({'discover_capabilities', 'validate_definition', 'describe_definition',
               'list_resources', 'query_resource', 'search_resource', 'plan_definition_update',
               'apply_definition_update', 'definition_update_status', 'create_bog_from_definition'} <= names, 'missing MCP tools')
        check(self.tool('discover_capabilities')['enabled'], 'MCP composition disabled')

    def mint(self, scope):
        credential = self.http('POST', self.base + '/tokens', {'scope': scope})
        self.credentials[credential['id']] = credential['token']
        self.state.setdefault('credential_ids', []).append(credential['id'])
        self.save()
        return credential['token']

    def revoke(self):
        for cid in list(self.state.get('credential_ids', [])):
            self.http('DELETE', self.base + '/tokens/' + str(uuid.UUID(cid)), expected=(204, 404))
            if cid in self.credentials:
                self.http('GET', self.base + '/resources', token=self.credentials.pop(cid), expected=(401,))
            self.state['credential_ids'].remove(cid)
            self.save()

    def create(self):
        check(not self.state, 'state already exists; use verify or cleanup')
        run = str(uuid.uuid4())
        self.state = {'run_id': run, 'credential_ids': []}
        self.save()
        check(self.http('GET', '/v1/components')['enabled'], 'composition disabled')
        definition = fixture('todo.json')
        check(self.http('POST', '/v1/definitions/validate', {'definition': definition})['valid'], 'invalid definition')
        body = {'name': 'composable-live-' + run, 'definition': definition}
        created = self.http('POST', '/v1/bogs', body, expected=(202,), key=run)
        check(created.get('kind') == 'defined' and created.get('template') is None, 'defined Bog identity is misleading')
        self.state['bog_id'] = str(uuid.UUID(created['id']))
        self.save()
        repeated = self.http('POST', '/v1/bogs', body, expected=(202,), key=run)
        check(repeated['id'] == self.state['bog_id'], 'idempotency mismatch')
        for _ in range(90):
            if self.http('GET', self.base)['status'] == 'ready':
                break
            time.sleep(1)
        else:
            raise RuntimeError('Bog readiness timeout')
        writer = self.mint('write')
        self.http('POST', self.base + '/batch', {'ops': fixture('todo-records.json')}, token=writer)
        self.connect()
        self.tool('batch', bog_id=self.state['bog_id'], ops=fixture('todo-records.json'))
        check(self.tool('validate_definition', definition=definition)['valid'], 'MCP validation failed')
        for name, via_mcp in [('todo-search.json', False), ('todo-semantic.json', True)]:
            active = self.http('GET', self.base + '/definition')
            update = {'definition': fixture(name), 'expected_revision': active['revision']}
            if via_mcp:
                self.tool('plan_definition_update', bog_id=self.state['bog_id'], **update)
                job = self.tool('apply_definition_update', bog_id=self.state['bog_id'], **update)
            else:
                self.http('POST', self.base + '/definition/plan', update)
                job = self.http('POST', self.base + '/definition/apply', update, expected=(202,))
            self.state.setdefault('job_ids', []).append(job['job_id'])
            self.save()
            for _ in range(120):
                result = self.http('GET', self.base + '/definition/jobs/' + job['job_id'])
                check(result['status'] != 'failed', 'definition update failed')
                if result['status'] in ('succeeded', 'completed', 'active'):
                    break
                time.sleep(1)
            else:
                raise RuntimeError('definition update timeout')
        self.verify(connected=True)

    def verify(self, connected=False):
        if not connected:
            self.connect()
        check(self.http('GET', self.base)['name'] == 'composable-live-' + self.state['run_id'], 'Bog identity mismatch')
        reader = self.mint('read')
        for record in fixture('todo-records.json'):
            got = self.http('GET', self.base + '/docs/' + record['key'], token=reader)['data']
            check(digest(got) == digest(record['data']), 'record mismatch')
        self.http('PUT', self.base + '/docs/denied', {}, token=reader, expected=(403,))
        self.http('GET', '/v1/bogs/' + (self.isolation_bog or str(uuid.uuid4())) + '/docs/milk', token=reader, expected=(404,))
        self.http('POST', self.base + '/resources/private_stats/query', {}, token=reader, expected=(404,))
        for resource in ['open', 'open_count', 'priority', 'text_search', 'semantic_search']:
            search = resource.endswith('search')
            query = {'query': 'release changelog', 'limit': 3, 'include_fields': ['/title']} if search else {}
            if resource == 'semantic_search':
                query['max_distance'] = 2
            rest = self.http('POST', self.base + '/resources/' + resource + ('/search' if search else '/query'), query, token=reader)['data']
            remote = self.tool('search_resource' if search else 'query_resource', bog_id=self.state['bog_id'], resource=resource, query=query)['data']
            check(rest == remote, 'REST/MCP disagreement')
            if resource == 'open_count':
                check(rest == 2, 'count mismatch')
            elif resource == 'priority':
                check(rest[0]['key'] == 'release', 'ranking mismatch')
            elif resource == 'open':
                check(len(rest) == 2 and 'notes' not in json.dumps(rest), 'filter/projection mismatch')
            else:
                check(bool(rest), 'empty search')
                check(all('value' in hit for hit in rest), 'search projection missing')
                if resource == 'semantic_search':
                    check(all(abs(hit['score'] - (1 - hit['distance'])) < 1e-6 for hit in rest), 'semantic score mismatch')
        resources = self.tool('list_resources', bog_id=self.state['bog_id'])
        check('private_stats' not in json.dumps(resources), 'private resource exposed')
        check(all('response_schema' in op for resource in resources['resources'] for op in resource['operations']), 'missing response schema')
        metrics = self.http('GET', self.base + '/metrics', token=reader)
        check(bool(metrics), 'scoped metrics missing')
        self.http('POST', '/v1/bogs', {}, token=reader, expected=(403,))
        self.http('POST', self.base + '/tokens', {}, token=reader, expected=(403,))
        definition = self.tool('describe_definition', bog_id=self.state['bog_id'])
        rest_definition = self.http('GET', self.base + '/definition')
        rest_definition.pop('request_id', None)
        definition.pop('request_id', None)
        check(definition == rest_definition, 'definition transport mismatch')
        fingerprint = digest(definition)
        check(self.state.get('definition_hash', fingerprint) == fingerprint, 'definition changed')
        self.state['definition_hash'] = fingerprint
        self.save()
        for job in self.state['job_ids']:
            status = self.tool('definition_update_status', bog_id=self.state['bog_id'], job_id=job)
            check(status['status'] in ('succeeded', 'completed', 'active'), 'job not complete')
            check(status.get('finished_at') and 'processed_records' in status and not status.get('writes_paused'), 'job progress missing')

    def cleanup(self):
        bog = self.http('GET', self.base)
        check(bog['name'] == 'composable-live-' + str(uuid.UUID(self.state['run_id'])), 'cleanup identity mismatch')
        self.revoke()
        self.http('DELETE', self.base, {'confirm': self.state['bog_id']}, expected=(200, 202, 204))
        self.http('GET', self.base, expected=(404,))
        self.path.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    phases = parser.add_mutually_exclusive_group(required=True)
    for phase in ('create', 'verify', 'cleanup'):
        phases.add_argument('--' + phase, action='store_true')
    parser.add_argument('--state', required=True, help='private local file containing IDs/hashes only')
    parser.add_argument('--isolation-bog', type=lambda value: str(uuid.UUID(value)),
                        help='optional existing other Bog UUID; only a forbidden-read assertion is made')
    args = parser.parse_args()
    probe = None
    try:
        probe = Probe(args.state)
        probe.isolation_bog = args.isolation_bog
        try:
            if args.create:
                probe.create()
            elif args.verify:
                probe.verify()
            else:
                probe.cleanup()
        finally:
            if probe.state.get('bog_id') and probe.state.get('credential_ids'):
                probe.revoke()
        print(json.dumps({'result': 'PASS', 'requests': probe.requests}))
    except Exception:
        # Never print exception text: remote payloads and auth must stay private.
        print(json.dumps({'result': 'FAIL', 'requests': probe.requests if probe else []}))
        raise SystemExit(1)


if __name__ == '__main__':
    main()
