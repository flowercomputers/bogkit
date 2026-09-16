#!/usr/bin/env python3
"""Real Codex-client check using ephemeral settings, with independent REST assertions."""
import json
import os
import pathlib
import subprocess
import tomllib
import urllib.request
import uuid

base = os.environ['BOG_CLOUD_URL'].rstrip('/')
token = os.environ['BOG_CLOUD_TOKEN']
run_id = str(uuid.uuid4())
name = 'codex-acceptance-' + run_id
config_path = pathlib.Path.home() / '.codex/config.toml'
config = tomllib.loads(config_path.read_text()) if config_path.exists() else {}
args = ['codex', 'exec', '--ephemeral', '--skip-git-repo-check', '--sandbox', 'read-only', '--json', '-C', '/tmp']
if os.environ.get('BOG_CODEX_MODEL'):
    args += ['--model', os.environ['BOG_CODEX_MODEL']]
for server in config.get('mcp_servers', {}):
    args += ['-c', 'mcp_servers.' + server + '.enabled=false']
args += ['-c', 'mcp_servers.bog_cloud_acceptance.url=' + json.dumps(base + '/mcp'),
         '-c', 'mcp_servers.bog_cloud_acceptance.bearer_token_env_var="BOG_CLOUD_TOKEN"']
# These exact fixture mutations are explicitly part of this acceptance run.
# Approval overrides are process-local; no user settings or other server policies change.
for tool in ['create_bog','upsert_record','delete_record','batch']:
    args += ['-c', 'mcp_servers.bog_cloud_acceptance.tools.' + tool + '.approval_mode="approve"']
prompt = f'''Use only the bog_cloud_acceptance MCP tools; do not run shell commands, read files, or use other connectors. This is an authorized disposable database acceptance test.
Create a records-v1 Bog named {name} with idempotency_key {run_id}. Poll describe_bog until ready.
Upsert key alpha with data {{"nested":{{"items":[1,true,null,"moss"]}}}}; read it back and verify equality.
Replace alpha with {{"replaced":true}} and verify total is 1.
Batch upsert beta={{"n":2}} and gamma={{"n":3}}, then verify total is3.
Delete beta and verify total is2. Read alpha and gamma and verify their exact contents.
Leave alpha and gamma for an independent REST check. Return the Bog UUID and a short PASS or precise failure. Do not issue extra resources or access any preexisting Bog.'''
result = subprocess.run(args + [prompt], env=os.environ.copy(), capture_output=True, text=True, timeout=300)
log = pathlib.Path(os.environ.get('BOG_CODEX_ACCEPTANCE_LOG', '/tmp/bog-cloud-codex-events.jsonl'))
log.write_text(result.stdout)
os.chmod(log, 0o600)
if result.returncode:
    error_log=log.with_suffix('.stderr')
    error_log.write_text(result.stderr)
    os.chmod(error_log,0o600)
    print(json.dumps({'result': 'FAIL', 'exit_code': result.returncode, 'events_file': str(log)}))
    raise SystemExit(1)

def get(path):
    req = urllib.request.Request(base + path, headers={'Authorization': 'Bearer ' + token})
    with urllib.request.urlopen(req, timeout=30) as response:
        return json.load(response)

bogs = get('/v1/bogs')['bogs']
bog = next(b for b in bogs if b['name'] == name)
path = '/v1/bogs/' + bog['id']
assert get(path + '/docs/alpha')['data'] == {'replaced': True}
assert get(path + '/docs/gamma')['data'] == {'n': 3}
assert get(path + '/views/total')['data']['value'] == 2
calls = []
for line in result.stdout.splitlines():
    try:
        event = json.loads(line)
    except ValueError:
        continue
    item = event.get('item', {})
    if event.get('type') == 'item.completed' and item.get('type') == 'mcp_tool_call':
        calls.append(item.get('tool'))
assert len(calls) >= 8, 'No sufficient actual MCP client tool-call evidence'
print(json.dumps({'result': 'PASS', 'client': subprocess.check_output(['codex', '--version'], text=True).strip(),
                  'bog_id': bog['id'], 'mcp_calls': len(calls), 'independent_rest_check': 'PASS'}))
