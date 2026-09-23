"""Supported Bog Cloud command line. Private credentials are never printed."""
import argparse
import contextlib
import json
import os
import secrets
import stat
import uuid
import urllib.request
import urllib.error
from pathlib import Path
import sys
from bog_client import Client, Failure
from bog_app_access import main as install_private


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--config'); p.add_argument('--auth-file'); p.add_argument('--origin', default='https://cloud.bog.new'); p.add_argument('--bog-id'); p.add_argument('--workspace-id')
    sub = p.add_subparsers(dest='command', required=True)
    sub.add_parser('connect')
    s = sub.add_parser('try'); s.add_argument('--name', required=True); s.add_argument('--output', required=True); s.add_argument('--definition')
    sub.add_parser('claim-link')
    s = sub.add_parser('install'); s.add_argument('--handoff', required=True); s.add_argument('--output', required=True); s.add_argument('--format', choices=['json','dotenv'], default='json'); s.add_argument('--replace', action='store_true')
    s = sub.add_parser('top'); s.add_argument('--resource', required=True); s.add_argument('--limit', type=int, default=100); s.add_argument('--offset', type=int, default=0); s.add_argument('--include-fields', nargs='+')
    s = sub.add_parser('query'); s.add_argument('--resource', default='docs'); s.add_argument('--input', required=True)
    s = sub.add_parser('cleanup-preview'); s.add_argument('--name-prefix', required=True)
    s = sub.add_parser('cleanup-execute'); s.add_argument('--preview-id', required=True); s.add_argument('--confirm', required=True)
    s = sub.add_parser('delete-sandbox'); s.add_argument('--confirm', required=True)
    for name in ('resources', 'routes', 'diagnostics'): sub.add_parser(name)
    for name in ('get', 'remove'):
        s = sub.add_parser(name); s.add_argument('key')
    s = sub.add_parser('put'); s.add_argument('key'); s.add_argument('--input', required=True, help='JSON file, or - for stdin')
    s = sub.add_parser('batch'); s.add_argument('--input', required=True)
    s = sub.add_parser('batch-get'); s.add_argument('keys', nargs='+'); s.add_argument('--resource', default='docs')
    s = sub.add_parser('list'); s.add_argument('--resource', default='docs'); s.add_argument('--limit', type=int, default=100); s.add_argument('--after'); s.add_argument('--before')
    s = sub.add_parser('search'); s.add_argument('--resource', required=True); s.add_argument('--query', required=True); s.add_argument('--limit', type=int, default=3); s.add_argument('--include-fields', nargs='+')
    s = sub.add_parser('wait'); s.add_argument('--cursor'); s.add_argument('--timeout', type=int, default=25)
    s = sub.add_parser('request-status', aliases=['explain']); s.add_argument('request_id')
    s = sub.add_parser('create'); s.add_argument('--name', required=True); s.add_argument('--idempotency-key', required=True); s.add_argument('--definition'); s.add_argument('--wait', action=argparse.BooleanOptionalAction, default=True); s.add_argument('--sandbox', action='store_true'); s.add_argument('--app-access', choices=['read','write']); s.add_argument('--app-label')
    s = sub.add_parser('add-search'); s.add_argument('--name', required=True); s.add_argument('--fields', nargs='+', required=True); s.add_argument('--kind', choices=['semantic','bm25'], default='semantic')
    a = p.parse_args(argv)
    try:
        if a.command == 'install':
            argv = ['--origin', a.origin, '--handoff', a.handoff, '--output', a.output, '--format', a.format]
            if a.auth_file: argv += ['--auth-file', a.auth_file]
            if a.replace: argv += ['--replace']
            with contextlib.redirect_stdout(sys.stderr): install_private(argv)
            result = {'status': 'installed', 'configuration': a.output}
        elif a.command == 'connect':
            if not a.auth_file: raise Failure('Connect requires --auth-file.')
            with contextlib.redirect_stdout(sys.stderr):
                install_private(['--connect', '--origin', a.origin, '--auth-file', a.auth_file])
            result = {'status':'connected'}
        elif a.command == 'try':
            from bog_app_access import origin, NoRedirect
            output = Path(a.output).absolute()
            if not output.parent.is_dir() or output.is_symlink(): raise Failure('Choose a private output file in an existing directory.')
            if output.exists():
                info=output.stat()
                if not stat.S_ISREG(info.st_mode) or info.st_uid!=os.getuid() or stat.S_IMODE(info.st_mode)!=0o600: raise Failure('Pending output must be an owned mode-600 regular file.')
                state = json.loads(output.read_text())
                if state.get('status') != 'pending' or state.get('origin') != origin(a.origin) or state.get('name') != a.name: raise Failure('Output already exists.')
            else:
                state = {'status':'pending','origin':origin(a.origin),'name':a.name,'idempotency_key':str(uuid.uuid4()),'recovery_secret':secrets.token_hex(32)}
                with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os,'O_NOFOLLOW',0), 0o600),'w') as handle:
                    json.dump(state,handle); handle.write('\n'); handle.flush(); os.fsync(handle.fileno())
            definition = json.loads(Path(a.definition).read_text()) if a.definition else None
            body = {'name':a.name,'recovery_secret':state['recovery_secret']}
            if definition is not None: body['definition'] = definition
            request = urllib.request.Request(origin(a.origin)+'/v1/claimable-bogs',data=json.dumps(body).encode(),headers={'Content-Type':'application/json','Idempotency-Key':state['idempotency_key']},method='POST')
            try:
                with urllib.request.build_opener(NoRedirect).open(request,timeout=35) as response: created=json.load(response)
            except urllib.error.HTTPError as error:
                raise Failure('Temporary Bog creation failed; retry with the same output file after checking capacity.') from error
            config = {'BOG_CLOUD_URL':origin(a.origin),'BOG_ID':created['id'],'BOG_CLOUD_TOKEN':created['credential']['access_token'],'expires_at':created['expires_at']}
            replacement = output.with_name('.'+output.name+'.'+uuid.uuid4().hex)
            with os.fdopen(os.open(replacement,os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os,'O_NOFOLLOW',0),0o600),'w') as handle:
                json.dump(config,handle); handle.write('\n'); handle.flush(); os.fsync(handle.fileno())
            os.replace(replacement,output)
            result={'status':'created','bog_id':created['id'],'expires_at':created['expires_at'],'configuration':str(output)}
        else:
            if a.config: c = Client.from_config(a.config)
            elif a.auth_file:
                with contextlib.redirect_stdout(sys.stderr): c = Client.from_auth(a.auth_file, a.origin, a.workspace_id)
            else: raise Failure('Select --config or --auth-file.')
            if a.bog_id: c.bog_id = a.bog_id
            def read_json(path): return json.load(sys.stdin) if path == '-' else json.loads(Path(path).read_text())
            if a.command in ('resources','routes','diagnostics'): result = getattr(c,a.command)()
            elif a.command in ('get','remove'): result = getattr(c,a.command)(a.key)
            elif a.command == 'put': result = c.put(a.key,read_json(a.input))
            elif a.command == 'batch': result = c.batch(read_json(a.input))
            elif a.command == 'batch-get': result = c.batch_get(a.keys,a.resource)
            elif a.command == 'list': result = c.list(a.resource,a.limit,a.after,a.before)
            elif a.command == 'search': result = c.search(a.resource,a.query,a.limit,a.include_fields)
            elif a.command == 'wait': result = c.wait(a.cursor,a.timeout)
            elif a.command in ('request-status','explain'): result = c.request_status(a.request_id)
            elif a.command == 'create': result = c.create(a.name,a.idempotency_key,read_json(a.definition) if a.definition else None, wait=a.wait, sandbox=a.sandbox, app_access={'scope':a.app_access,'label':a.app_label or a.name} if a.app_access else None)
            elif a.command == 'top': result = c.top(a.resource,a.limit,a.offset,a.include_fields)
            elif a.command == 'query': result = c.query(a.resource,**read_json(a.input))
            elif a.command == 'cleanup-preview': result = c.preview_cleanup(a.name_prefix)
            elif a.command == 'cleanup-execute': result = c.execute_cleanup(a.preview_id,a.confirm)
            elif a.command == 'delete-sandbox': result = c.delete_sandbox(a.confirm)
            elif a.command == 'claim-link': result = c.claim_link()
            elif a.command == 'add-search': result = c.add_search(a.name,a.fields,a.kind)
        print(json.dumps(result))
    except (Failure,OSError,ValueError,KeyError,TypeError,KeyboardInterrupt):
        print(json.dumps({'error':'Operation failed. Check configuration, permission and request state before retrying writes.'}),file=sys.stderr)
        return 1
    return 0

if __name__ == '__main__':
    os.umask(0o077)
    sys.exit(main())
