#!/usr/bin/env python3
"""Explicit actual-service smoke; creates and deletes only its own sandbox.

BOG_TEST_ORIGIN and BOG_TEST_AUTH_FILE select a private management authorization.
Only check names are printed; private app credentials stay in a temporary file.
"""
import contextlib
import json
import os
from pathlib import Path
import sys
import tempfile
import uuid
from bog_client import Client, BogError
from bog_app_access import main as install_private


def main():
    base=os.environ['BOG_TEST_ORIGIN'];auth=os.environ['BOG_TEST_AUTH_FILE']
    client=Client.from_auth(auth,base,os.environ.get('BOG_TEST_WORKSPACE_ID'))
    definition=json.loads((Path(__file__).resolve().parents[2]/'docs/examples/composable/todo-search.json').read_text())
    with tempfile.TemporaryDirectory(prefix='bog-service-smoke-') as directory:
        try:
            result=client.create('client-smoke-'+str(uuid.uuid4()),str(uuid.uuid4()),definition,sandbox=True,wait=True,app_access={'scope':'read','label':'Python client smoke'})
            assert result['status']=='ready'
            config=Path(directory)/'app.env'
            with contextlib.redirect_stdout(sys.stderr):
                install_private(['--origin',base,'--auth-file',auth,'--handoff',result['app_access']['handoff_id'],'--output',str(config),'--format','dotenv'])
            app=Client.from_config(config)
            cursor=client.wait(timeout=0)['cursor']
            client.put('b',{'title':'smoke alpha','notes':'findable','priority':9,'completed':False})
            client.put('c',{'title':'smoke beta','notes':'findable','priority':3,'completed':False})
            assert client.wait(cursor,0)['changed']
            batch=app.batch_get(['b','missing','b'])['data']
            assert len(batch)==3 and batch[1]['value'] is None and batch[0]==batch[2]
            page=app.list(after='a',before='d',limit=10)
            assert 'smoke alpha' in json.dumps(page)
            rank=app.top('priority',10,0,['/title'])
            assert rank['data'][0]['value']['/title']=='smoke alpha'
            search=app.search('text_search','smoke',3,['/title'])
            assert '/title' in json.dumps(search)
            app.routes();app.diagnostics()
            try:app.put('forbidden',{'title':'no'})
            except BogError as error:assert error.status==403
            else:raise AssertionError('read scope accepted write')
            print(json.dumps({'status':'passed','checks':['sandbox-readiness','private-install-dotenv','batch-get-null-duplicates','range','rank-projection','search-projection','change-wait','routes','diagnostics','read-scope']}))
        finally:
            if client.bog_id:client.delete_sandbox(client.bog_id)


if __name__=='__main__':
    try:main()
    except Exception:
        print('Service smoke failed; inspect the test service and retained request observations.',file=sys.stderr)
        sys.exit(1)
