#!/usr/bin/env python3
"""Prepare or extend a project with explicit management authorization; no secrets printed."""
import argparse
import json
import os
from pathlib import Path
import sys
from bog_client import Client, Failure
from bog_app_access import main as install


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--origin',default='https://cloud.bog.new');p.add_argument('--auth-file',required=True);p.add_argument('--workspace-id')
    sub=p.add_subparsers(dest='command',required=True)
    create=sub.add_parser('create');create.add_argument('--name',required=True);create.add_argument('--idempotency-key',required=True);create.add_argument('--output',required=True)
    search=sub.add_parser('add-search');search.add_argument('--bog-id',required=True);search.add_argument('--name',required=True);search.add_argument('--kind',choices=['semantic','bm25'],default='semantic');search.add_argument('--fields',nargs='+',required=True)
    a=p.parse_args();c=Client.from_auth(a.auth_file,a.origin,a.workspace_id)
    if a.command=='add-search':
        c.bog_id=a.bog_id;job=c.add_search(a.name,a.fields,a.kind);print(json.dumps({'status':job['status'],'bog_id':c.bog_id}));return
    output=Path(a.output).absolute()
    if output.exists() or output.is_symlink() or not output.parent.is_dir() or output==Path(a.auth_file).absolute(): raise Failure('Select a new app configuration in an existing private directory, separate from the authorization file.')
    result=c.create(a.name,a.idempotency_key)
    handoff=c.request('POST',c.path('/app-access'),{'scope':'write','label':a.name})
    install(['--origin',a.origin,'--auth-file',a.auth_file,'--handoff',handoff['handoff_id'],'--output',str(output)])
    app=Client.from_config(output);resources=app.resources()
    print(json.dumps({'bog_id':c.bog_id,'status':'connected','configuration':str(output),'resources':[r['name'] for r in resources['resources']]}))

if __name__=='__main__':
    os.umask(0o077)
    try: main()
    except (Failure,OSError,ValueError,KeyError,KeyboardInterrupt) as e:
        print(str(e) if isinstance(e,Failure) else 'Project setup interrupted or invalid; inspect current project state before retrying.',file=sys.stderr);sys.exit(1)
