#!/usr/bin/env python3
"""Back up a test Bog, restore separately, and verify both across a Fly restart.

Requires BOG_CLOUD_URL/TOKEN, BOG_TEST_SOURCE_ID and BOG_TEST_MACHINE_ID.
Uses the deployed admin CLI; credentials never appear in command arguments.
"""
import json
import os
import subprocess
import time
import uuid

from rest_acceptance import request

app = os.environ.get('BOG_FLY_APP', 'flower-bog-cloud')
source = str(uuid.UUID(os.environ['BOG_TEST_SOURCE_ID']))
machine = os.environ['BOG_TEST_MACHINE_ID']


def admin(command):
    result = subprocess.run(
        ['flyctl', 'ssh', 'console', '--app', app, '--command',
         'bog-cloud-admin ' + command], capture_output=True, text=True, check=True,
        timeout=180)
    return json.loads(next(line for line in result.stdout.splitlines()
                           if line.startswith('{')))


def snapshot(bog, token=None):
    options = {} if token is None else {'token': token}
    prefix = '/v1/bogs/' + bog
    return {view: request('GET', prefix + '/views/' + view, **options)['data']
            for view in ['docs', 'total']}


def ready(bog):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        try:
            if request('GET', '/v1/bogs/' + bog)['status'] == 'ready':
                return
        except (OSError, AssertionError):
            pass
        time.sleep(1)
    raise AssertionError('Bog did not become ready after restart')


before = snapshot(source)
assert before['total']['value'] > 0, 'Use a nonempty disposable test Bog'
credential = request('POST', '/v1/bogs/' + source + '/tokens', {'scope': 'read'})
try:
    restored = os.environ.get('BOG_TEST_RESTORED_ID')
    if restored:
        restored = str(uuid.UUID(restored))
        archive = os.environ.get('BOG_TEST_ARCHIVE_ID', 'previously-verified')
    else:
        archive = admin('backup ' + source)['archive_id']
        restored = admin('restore ' + archive + ' --name recovery-acceptance')['id']
    print(json.dumps({'stage': 'restore', 'archive': archive, 'restored': restored}), flush=True)
    ready(restored)
    assert restored != source
    assert snapshot(source, credential['token']) == before
    assert snapshot(restored) == before
    request('GET', '/v1/bogs/' + restored + '/views/total',
            token=credential['token'], expected=404)
    subprocess.run(['flyctl', 'machine', 'restart', machine, '--app', app,
                    '--signal', 'SIGTERM', '--time', '60'],
                   capture_output=True, text=True, check=True, timeout=240)
    ready(source)
    ready(restored)
    assert snapshot(source, credential['token']) == before
    assert snapshot(restored) == before
    print(json.dumps({'result': 'PASS', 'source': source, 'restored': restored,
                      'archive': archive, 'machine': machine,
                      'checks': ['closed-store backup', 'independent restore',
                                 'matching records and Fold count',
                                 'credential isolation', 'machine restart',
                                 'persistent records and scoped credential']}))
finally:
    request('DELETE', '/v1/bogs/' + source + '/tokens/' + credential['id'], expected=204)
