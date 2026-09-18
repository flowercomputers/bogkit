"""Run exact Cowork/Scatter search fixtures against a disposable local server.
Build bog-cloud and bog-cloud-records binaries first. No production writes.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
from bog_client import Client

REPO = Path(__file__).resolve().parents[2]


def run():
    with tempfile.TemporaryDirectory(prefix='bog-search-', dir='/tmp') as temporary:
        root = Path(temporary)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        token = secrets.token_urlsafe(40)
        env = {**os.environ, 'BOG_CLOUD_ROOT': str(root / 'data'),
               'BOG_WORKER_BINARY': str(REPO / 'target/debug/bog-records-worker'),
               'BOG_CLOUD_OWNER_TOKEN': token, 'BOG_CLOUD_BIND': f'127.0.0.1:{port}',
               'BOG_ALLOW_LEGACY_PUBLIC_OPERATOR': 'true', 'BOG_CLOUD_COMPOSABLE': 'true'}
        client = Client(f'http://127.0.0.1:{port}', token)
        log = open(root / 'server.log', 'w')
        server = None

        def start():
            process = subprocess.Popen([str(REPO / 'target/debug/bog-cloud-server')],
                                       env=env, stdout=log, stderr=log)
            for _ in range(200):
                try:
                    client.request('GET', '/v1/bogs')
                    return process
                except Exception:
                    if process.poll() is not None:
                        raise RuntimeError('local server exited; inspect build/configuration')
                    time.sleep(.1)
            process.terminate()
            process.wait()
            raise RuntimeError('local server startup timed out')

        report = {'scope': 'local disposable server, pinned built-in encoder', 'fixtures': {}}
        try:
            server = start()
            client.create('cowork-search-repro', 'cowork-search-repro')
            records = {
                'm1': {'user': 'ed', 'text': 'anyone want to grab ramen tonight', 'ts': 1},
                'm2': {'user': 'ash', 'text': 'the shop was slammed today, new jackets sold out', 'ts': 2},
                'm3': {'user': 'ed', 'text': 'deploying the database fix now', 'ts': 3},
            }
            for key, value in records.items():
                client.put(key, value)
            client.add_search('semantic_search', ['/text'])
            client.add_search('text_search', ['/text'], 'bm25')

            def observe():
                return {query: {kind: client.search(kind, query)['data']
                                for kind in ('semantic_search', 'text_search')}
                        for query in ('dinner plans', 'noodles for dinner')}

            initial = observe()
            report['fixtures']['cowork'] = {'records': records, 'fields': ['/text'],
                                             'results': initial,
                                             'diagnostics': client.diagnostics()['data']}
            # Mutation freshness is tested against an exact query, independent of
            # whether the model considers a paraphrase relevant.
            replacement = {**records['m1'], 'text': 'mountain telescope observatory'}
            client.put('m1', replacement)
            hits = client.search('semantic_search', replacement['text'])['data']
            assert hits[0]['key'] == 'm1', hits
            assert client.search('text_search', 'ramen')['data'] == []
            client.remove('m1')
            assert all(hit['key'] != 'm1' for hit in client.search('semantic_search', replacement['text'])['data'])
            assert all(r['searchable_records'] == 2 for r in client.diagnostics()['data']['search_resources'])
            server.terminate()
            server.wait()
            server = start()
            assert all(hit['key'] != 'm1' for hit in client.search('semantic_search', replacement['text'])['data'])
            assert all(r['searchable_records'] == 2 for r in client.diagnostics()['data']['search_resources'])
            client.put('m1', records['m1'])
            restored = observe()
            assert restored == initial, (initial, restored)
            report['fixtures']['cowork']['update_delete_restart_restore'] = 'passed'
            client.create('scatter-search-repro', 'scatter-search-repro')
            client.put('cabin', {'title': 'A cabin for deep work', 'body': 'An isolated shelter among trees, away from interruptions.'})
            client.add_search('semantic_search', ['/title', '/body'])
            client.add_search('text_search', ['/title', '/body'], 'bm25')
            report['fixtures']['scatter'] = {kind: client.search(kind, 'quiet woodland retreat')['data'] for kind in ('semantic_search', 'text_search')}
            assert report['fixtures']['scatter']['semantic_search'][0]['key'] == 'cabin'
            assert report['fixtures']['scatter']['text_search'] == []
            print(json.dumps(report, indent=2))
        finally:
            if server is not None and server.poll() is None:
                server.terminate()
                server.wait()
            log.close()


if __name__ == '__main__':
    run()
