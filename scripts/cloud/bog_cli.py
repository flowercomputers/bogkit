"""Supported Bog Cloud command line. Private credentials are never printed."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import sys
from bog_client import Client, Failure
from bog_app_access import main as install_private


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--config'); p.add_argument('--auth-file'); p.add_argument('--origin', default='https://cloud.bog.new'); p.add_argument('--bog-id'); p.add_argument('--workspace-id')
    sub = p.add_subparsers(dest='command', required=True)
    sub.add_parser('connect')
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
            elif a.command == 'add-search': result = c.add_search(a.name,a.fields,a.kind)
        print(json.dumps(result))
    except (Failure,OSError,ValueError,KeyError,TypeError,KeyboardInterrupt):
        print(json.dumps({'error':'Operation failed. Check configuration, permission and request state before retrying writes.'}),file=sys.stderr)
        return 1
    return 0

if __name__ == '__main__':
    os.umask(0o077)
    sys.exit(main())
