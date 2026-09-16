#!/usr/bin/env python3
"""Exercise a disposable pair of Bogs; credentials are environment-only, never printed."""
import json
import os
import time
import uuid
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ['BOG_CLOUD_URL'].rstrip('/')
OWNER = os.environ['BOG_CLOUD_TOKEN']

def request(method, path, body=None, token=OWNER, headers=None, expected=200):
    h = {'Authorization': 'Bearer ' + token, **(headers or {})}
    data = None if body is None else json.dumps(body).encode()
    if data is not None:
        h['Content-Type'] = 'application/json'
    req = urllib.request.Request(BASE + path, data=data, headers=h, method=method)
    try:
        response = urllib.request.urlopen(req, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read(5 * 1024 * 1024)
        result = json.loads(raw) if raw else None
        assert response.status == expected, (method, path, response.status, expected)
        return result

def create(label):
    run_id = str(uuid.uuid4())
    body = {'name': 'acceptance-' + label + '-' + run_id, 'template': 'records-v1'}
    h = {'Idempotency-Key': run_id}
    a = request('POST', '/v1/bogs', body, headers=h, expected=202)
    b = request('POST', '/v1/bogs', body, headers=h, expected=202)
    assert a['id'] == b['id']
    path = '/v1/bogs/' + a['id']
    deadline = time.monotonic() + 45
    while request('GET', path)['status'] != 'ready':
        assert time.monotonic() < deadline, 'worker readiness timed out'
        time.sleep(0.2)
    return path

def main():
    a, b = create('a'), create('b')
    writer = request('POST', a + '/tokens', {'scope': 'write'})
    reader = request('POST', a + '/tokens', {'scope': 'read'})
    w, r = writer['token'], reader['token']
    key = urllib.parse.quote('123 😀?#%', safe='')
    record = {'title': 'remote check', 'nested': {'items': [1, True, None, '🌿']}}
    request('PUT', a + '/docs/' + key, record, w)
    assert request('GET', a + '/docs/' + key, token=r)['data'] == record
    request('PUT', a + '/docs/' + key, {'updated': True}, w)
    assert request('GET', a + '/views/total', token=r)['data']['value'] == 1
    request('GET', b + '/docs/' + key, token=r, expected=404)
    request('PUT', a + '/docs/denied', {}, r, expected=403)
    request('POST', a + '/batch', [
        {'op': 'upsert', 'key': 'new', 'data': {'v': 1}},
        {'op': 'upsert', 'key': 'bad/key', 'data': {'v': 2}},
    ], w, expected=400)
    request('GET', a + '/docs/new', token=r, expected=404)
    request('POST', a + '/batch', [
        {'op': 'upsert', 'key': 'new', 'data': {'v': 1}},
        {'op': 'upsert', 'key': 'second', 'data': {'v': 2}},
    ], w)
    assert request('GET', a + '/views/total', token=r)['data']['value'] == 3
    request('DELETE', a + '/docs/new', token=w)
    assert request('GET', a + '/views/total', token=r)['data']['value'] == 2
    request('DELETE', a + '/tokens/' + reader['id'], expected=204)
    request('GET', a + '/views/total', token=r, expected=401)
    request('DELETE', a + '/tokens/' + writer['id'], expected=204)
    print(json.dumps({'result': 'PASS', 'databases': [a, b], 'checks': [
        'idempotent creation', 'nested JSON', 'replacement', 'Fold count',
        'scope isolation', 'atomic batch rejection', 'batch', 'delete', 'revocation'
    ]}))

if __name__ == '__main__':
    main()
