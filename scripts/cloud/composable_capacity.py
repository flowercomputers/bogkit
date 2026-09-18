#!/usr/bin/env python3
"""Bounded local production-image measurement; never use an existing data root."""
import argparse
import concurrent.futures
import hashlib
import json
import pathlib
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

OWNER = 'capacity-local-fixture-not-a-production-secret'
ROOT = pathlib.Path(__file__).resolve().parents[2]
SAMPLE = r'''
for f in memory.current memory.peak memory.events memory.max cpu.max cpu.stat; do
  printf 'CGROUP\t%s\t' "$f"; tr '\n' ';' < /sys/fs/cgroup/$f; printf '\n'
done
for p in /proc/[0-9]*; do
  [ -r "$p/status" ] || continue
  exe=$(readlink "$p/exe" 2>/dev/null) || continue
  case "$exe" in */bog-cloud-server|*/bog-records-worker)
    awk -v pid="${p##*/}" -v exe="$exe" '/^VmRSS:/ {printf "RSS\t%s\t%s\t%s\n",pid,exe,$2}' "$p/status";;
  esac
done
'''


def docker(*args):
    p = subprocess.run(['docker', *args], capture_output=True, text=True, timeout=60)
    if p.returncode:
        raise RuntimeError('docker command failed: ' + args[0])
    return p.stdout


def seed(root):
    root = root.resolve()
    if not any(root.is_relative_to(p.resolve()) for p in [pathlib.Path('/tmp'), pathlib.Path('/private/tmp')]):
        raise ValueError('fixture root must be under /tmp, dedicated to this run')
    db = sqlite3.connect(f'file:{root / "registry.sqlite"}?mode=rw', uri=True)
    tokens = []
    with db:
        if db.execute('SELECT count(*) FROM bogs').fetchone()[0] or db.execute('SELECT count(*) FROM accounts').fetchone()[0]:
            raise ValueError('fixture requires a fresh registry with no Bogs or accounts')
        for i in range(3):
            account, workspace, token_id = [str(uuid.uuid4()) for _ in range(3)]
            token = f'bog_agent_{token_id}.' + 'a' * 64
            now = int(time.time())
            db.execute("INSERT INTO accounts(id,issuer,subject,created_at) VALUES(?,'local-capacity',?,?)", (account, account, now))
            db.execute("INSERT INTO workspaces(id,name,personal_account_id,created_at) VALUES(?,'local-capacity',?,?)", (workspace, account, now))
            db.execute("INSERT INTO memberships VALUES(?,?,'owner')", (workspace, account))
            db.execute("INSERT INTO agent_tokens(id,account_id,name,secret_hash,created_at,expires_at) VALUES(?,?,'local-capacity',?,?,?)", (token_id, account, hashlib.sha256(token.encode()).digest(), now, now + 86400))
            tokens.append(token)
    db.close()
    return tokens


class Harness:
    def __init__(self, args):
        self.args = args
        self.stage = 'preflight'
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.report = {'scope': {'records_per_bog': args.records, 'total_bogs': 9,
                       'resident_limit': 8, 'not_maximum_capacity_certification': True,
                       'latency_is_local_docker_not_fly': True}, 'samples': [], 'requests': []}
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(self, method, path, token, body=None):
        started = time.monotonic()
        req = urllib.request.Request(self.args.url + path,
            data=None if body is None else json.dumps(body).encode(), method=method,
            headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                     'Idempotency-Key': 'capacity-' + str(uuid.uuid4())})
        try:
            with self.opener.open(req, timeout=180) as response:
                result = json.load(response)
                status = response.status
        except urllib.error.HTTPError as error:
            # Never log response bodies/headers: even accidental nonfixture data stays private.
            raise RuntimeError(f'{method} {path}: HTTP {error.code}') from None
        finally:
            with self.lock:
                self.report['requests'].append({'stage': self.stage, 'method': method,
                    'path': path, 'seconds': time.monotonic() - started})
        if not 200 <= status < 300 or (isinstance(result, dict) and 'error' in result):
            raise RuntimeError(f'{method} {path}: API error')
        return result

    def sample(self):
        raw = docker('exec', self.args.container, 'sh', '-c', SAMPLE)
        sample = {'stage': self.stage, 'time': time.time(), 'cgroup': {}, 'process_rss_kib': []}
        for line in raw.splitlines():
            fields = line.split('\t')
            if fields[0] == 'CGROUP':
                sample['cgroup'][fields[1]] = fields[2].rstrip(';')
            elif fields[0] == 'RSS':
                sample['process_rss_kib'].append({'pid': int(fields[1]), 'binary': fields[2], 'rss_kib': int(fields[3])})
        if sample['cgroup'].get('memory.max') != '1073741824' or sample['cgroup'].get('cpu.max') != '100000 100000':
            raise RuntimeError('expected cgroup v2 1 GiB / 1 CPU limits')
        self.report['samples'].append(sample)
        return sample

    def monitor(self):
        while not self.stop.wait(1):
            try:
                self.sample()
            except Exception as error:
                self.report.setdefault('sampling_errors', []).append(type(error).__name__)
                return

    def query(self, bog, resource, search=False):
        base, token, _ = bog
        return self.request('POST', base + '/resources/' + resource + ('/search' if search else '/query'), token,
                            {'query': 'publish software changelog', 'limit': 5} if search else {})

    def verify(self, bog):
        base, token, kind = bog
        got = self.request('GET', base + '/docs/record-0', token)
        if got['data'] != self.record(0):
            raise RuntimeError('acknowledged record differs after reopen')
        if kind != 'records-v1':
            if self.query(bog, 'open_count')['data'] != self.args.records:
                raise RuntimeError('maintained count differs')
        if kind in ('todo-search', 'todo-semantic'):
            if not self.query(bog, 'text_search', True)['data']:
                raise RuntimeError('populated text index returned no matches')
        if kind == 'todo-semantic':
            if not self.query(bog, 'semantic_search', True)['data']:
                raise RuntimeError('populated semantic index returned no matches')

    @staticmethod
    def record(i):
        return {'title': f'Publish software changelog {i}', 'notes': f'Release notes and backup verification task {i}', 'completed': False, 'priority': i % 10}

    def run(self):
        cfg = json.loads(docker('inspect', self.args.container))[0]
        env = dict(item.split('=', 1) for item in cfg['Config']['Env'] if '=' in item)
        if cfg['Config'].get('Labels', {}).get('bog.capacity') != 'local':
            raise ValueError('container must have --label bog.capacity=local')
        if env.get('BOG_CLOUD_OWNER_TOKEN') != OWNER or env.get('BOG_CLOUD_COMPOSABLE') != 'true':
            raise ValueError('container must use fixed local fixture owner and composable=true')
        if env.get('BOG_CLOUD_MAX_ACTIVE', '8') != '8':
            raise ValueError('resident limit must be 8')
        mounted = any(pathlib.Path(m['Source']).resolve() == self.args.fixture_root.resolve()
                      and m['Destination'] == env.get('BOG_CLOUD_ROOT', '/data/bog') for m in cfg['Mounts'])
        if not mounted:
            raise ValueError('fixture root must exactly match the container BOG_CLOUD_ROOT bind mount')
        image = json.loads(docker('image', 'inspect', cfg['Image']))[0]
        self.report['image'] = {'id': cfg['Image'], 'os': image['Os'], 'architecture': image['Architecture']}
        self.sample()
        tokens = seed(self.args.fixture_root)
        thread = threading.Thread(target=self.monitor, daemon=True)
        thread.start()
        bogs = []
        try:
            kinds = ['records-v1', 'todo', 'todo-search', 'todo-semantic', 'todo', 'todo-search', 'todo-semantic', 'todo', 'records-v1']
            for index, kind in enumerate(kinds):
                self.stage = f'populate-{index + 1}-{kind}'
                token = tokens[index // 3]
                payload = {'name': f'capacity-{index}'}
                if kind == 'records-v1':
                    payload['template'] = kind
                else:
                    payload['definition'] = json.loads((ROOT / f'docs/examples/composable/{kind}.json').read_text())
                created = self.request('POST', '/v1/bogs', token, payload)
                bog = ['/v1/bogs/' + created['id'], token, kind]
                bogs.append(bog)
                for start in range(0, self.args.records, 20):
                    self.request('POST', bog[0] + '/batch', token,
                        [{'op': 'upsert', 'key': f'record-{i}', 'data': self.record(i)}
                         for i in range(start, min(start + 20, self.args.records))])
                if kind == 'todo-semantic':
                    self.stage = 'semantic-first-query-after-population'
                self.verify(bog)
                if index == 1:
                    self.stage = 'additive-rebuild-populated-source'
                    before = self.request('GET', bog[0] + '/definition', token)
                    definition = json.loads((ROOT / 'docs/examples/composable/todo-semantic.json').read_text())
                    body = {'definition': definition, 'expected_revision': before['revision']}
                    self.request('POST', bog[0] + '/definition/plan', token, body)
                    job = self.request('POST', bog[0] + '/definition/apply', token, body)
                    deadline = time.monotonic() + 180
                    while True:
                        state = self.request('GET', bog[0] + '/definition/jobs/' + job['job_id'], token)
                        if state['status'] in ('succeeded', 'completed', 'active'):
                            break
                        if state['status'] == 'failed' or time.monotonic() > deadline:
                            raise RuntimeError('definition rebuild failed or timed out')
                        self.query(bog, 'open_count')
                        time.sleep(0.25)
                    bog[2] = 'todo-semantic'
                    self.verify(bog)
                if kind == 'todo-semantic':
                    self.stage = 'semantic-warm-query'
                    self.query(bog, 'semantic_search', True)
                sample = self.sample()
                workers = sum(p['binary'].endswith('/bog-records-worker') for p in sample['process_rss_kib'])
                if index == 7 and workers != 8:
                    raise RuntimeError(f'expected 8 resident workers, observed {workers}')
                if workers > 8:
                    raise RuntimeError('resident worker limit exceeded')
            self.stage = 'nine-bog-sequential-cycling'
            for _ in range(2):
                for bog in bogs:
                    self.verify(bog)
            self.stage = 'eight-resident-concurrent-reads'
            # Warm exactly these eight before concurrent reads, so this measures steady load.
            for bog in bogs[1:]:
                self.verify(bog)
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                list(pool.map(self.verify, bogs[1:]))
            self.sample()
            if self.args.restart:
                self.stop.set()
                thread.join(timeout=65)
                self.stage = 'container-restart'
                docker('restart', self.args.container)
                deadline = time.monotonic() + 45
                while True:
                    try:
                        with self.opener.open(self.args.url + '/v1', timeout=2) as response:
                            if response.status == 200:
                                break
                    except (urllib.error.URLError, TimeoutError):
                        if time.monotonic() > deadline:
                            raise RuntimeError('restart readiness timed out') from None
                        time.sleep(0.5)
                self.stop.clear()
                thread = threading.Thread(target=self.monitor, daemon=True)
                thread.start()
                self.stage = 'restart-persistence-all-records'
                for bog in bogs:
                    self.verify(bog)
                    for i in range(self.args.records):
                        if self.request('GET', bog[0] + f'/docs/record-{i}', bog[1])['data'] != self.record(i):
                            raise RuntimeError('acknowledged record lost after restart')
            self.sample()
            if self.report.get('sampling_errors'):
                raise RuntimeError('metrics sampling failed')
            for sample in self.report['samples']:
                events = dict(item.split() for item in sample['cgroup']['memory.events'].split(';') if item)
                if int(events.get('oom', 0)) or int(events.get('oom_kill', 0)):
                    raise RuntimeError('cgroup reported OOM')
            self.report['result'] = 'PASS'
        finally:
            self.stop.set()
            thread.join(timeout=65)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', default='http://127.0.0.1:18080')
    parser.add_argument('--container', required=True)
    parser.add_argument('--fixture-root', required=True, type=pathlib.Path)
    parser.add_argument('--records', type=int, default=100)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--restart', action='store_true', help='restart the dedicated container and verify every acknowledged record')
    args = parser.parse_args()
    parsed = urllib.parse.urlsplit(args.url)
    if parsed.scheme != 'http' or parsed.hostname not in ('127.0.0.1', 'localhost') or parsed.path or parsed.query or parsed.fragment or parsed.username:
        parser.error('URL must be a loopback HTTP origin')
    if not 100 <= args.records <= 1000:
        parser.error('records must be between 100 and 1000')
    h = Harness(args)
    try:
        h.run()
    except Exception as error:
        try:
            h.sample()
        except Exception:
            pass
        h.report['result'] = 'FAIL'
        h.report['failure'] = {'stage': h.stage, 'type': type(error).__name__, 'message': str(error) if isinstance(error, (ValueError, RuntimeError)) else 'request or local fixture failure'}
        raise SystemExit(1)
    finally:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(h.report, indent=2) + '\n')
        print(json.dumps({'result': h.report.get('result'), 'metrics_file': str(args.output)}))


if __name__ == '__main__':
    main()
