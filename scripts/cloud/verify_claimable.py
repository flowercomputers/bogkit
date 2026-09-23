#!/usr/bin/env python3
"""Disposable local claimable-Bog journey; no production credentials or resources."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]

def main():
    with tempfile.TemporaryDirectory(prefix='bog-claimable-', dir='/tmp') as directory:
        base_dir=Path(directory)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
        base=f'http://127.0.0.1:{port}'
        env={**os.environ,'BOG_CLOUD_ROOT':str(base_dir/'data'),'BOG_WORKER_BINARY':str(ROOT/'target/debug/bog-records-worker'),'BOG_CLOUD_OWNER_TOKEN':secrets.token_hex(40),'BOG_CLOUD_PUBLIC_ORIGIN':base,'BOG_CLOUD_BIND':f'127.0.0.1:{port}','BOG_CLOUD_CLAIMABLE':'true','BOG_CLOUD_COMPOSABLE':'true','BOG_GITHUB_CLIENT_ID':'local-test','BOG_GITHUB_CLIENT_SECRET':'not-a-real-secret','BOG_GITHUB_REDIRECT_URI':'https://cloud.bog.new/auth/callback'}
        def request(method,path,body=None,token=None,headers=None):
            head=dict(headers or {})
            if token:head['Authorization']='Bearer '+token
            data=json.dumps(body).encode() if body is not None else None
            if data is not None:head['Content-Type']='application/json'
            req=urllib.request.Request(base+path,data=data,headers=head,method=method)
            try:res=urllib.request.urlopen(req,timeout=35)
            except urllib.error.HTTPError as error:res=error
            with res:
                raw=res.read(); status=res.status
            try:value=json.loads(raw)
            except ValueError:value=raw.decode()
            return status,value
        def start():
            log=open(base_dir/'server.log','a')
            server=subprocess.Popen([str(ROOT/'target/debug/bog-cloud-server')],env=env,stdout=log,stderr=log)
            log.close()
            for _ in range(200):
                if server.poll() is not None:raise RuntimeError('server exited; inspect local log')
                try:
                    if request('GET','/healthz')[0]==200:return server
                except OSError:pass
                time.sleep(.1)
            raise RuntimeError('local server did not start')
        def create(name,definition=None):
            nonce=secrets.token_hex(32);key=str(uuid.uuid4())
            body={'name':name,'recovery_secret':nonce}
            if definition is not None:body['definition']=definition
            status,result=request('POST','/v1/claimable-bogs',body,headers={'Idempotency-Key':key})
            assert status==201,(status,result)
            return result,key,nonce
        server=start()
        try:
            assert request('GET','/')[0]==200
            assert request('GET','/llms.txt')[0]==200
            assert request('GET','/openapi.json')[1]['paths']['/v1/claimable-bogs']['post']['security']==[]
            first,key,nonce=create('claimable-test')
            bog=first['id']; token=first['credential']['access_token']
            status,retry=request('POST','/v1/claimable-bogs',{'name':'claimable-test','recovery_secret':nonce},headers={'Idempotency-Key':key})
            assert status==201 and retry['id']==bog
            new_token=retry['credential']['access_token']
            assert token!=new_token and request('GET',f'/v1/bogs/{bog}',token=token)[0]==401
            probes=[create(f'probe-{n}')[0] for n in range(3)]
            assert request('GET',f'/v1/bogs/{probes[0]["id"]}',token=new_token)[0] in (401,403,404)
            status,limited=request('POST','/v1/claimable-bogs',{'name':'over-limit','recovery_secret':secrets.token_hex(32)},headers={'Idempotency-Key':str(uuid.uuid4())})
            assert status==429 and limited['retry_after_ms']>0,(status,limited)
            for _ in range(50):
                status,detail=request('GET',f'/v1/bogs/{bog}',token=new_token)
                if status==200 and detail.get('status')=='ready':break
                time.sleep(.1)
            else:raise RuntimeError('Bog did not become ready')
            status,write=request('PUT',f'/v1/bogs/{bog}/docs/n1',{'title':'Hello','body':'Temporary note'},token=new_token)
            assert status in (200,201),(status,write)
            assert request('GET',f'/v1/bogs/{bog}/docs/n1',token=new_token)[0]==200
            status,issued=request('POST',f'/v1/claimable-bogs/{bog}/claim',token=new_token)
            assert status==200,(status,issued)
            code=issued['claim_url'].rsplit('/',1)[-1]
            assert request('GET',f'/claim/{code}/status')[1]['state']=='active'
            secret=secrets.token_hex(32);csrf=secrets.token_hex(32); subject='123456'
            db=sqlite3.connect(base_dir/'data/auth-sessions/native-sessions.sqlite3')
            with db:db.execute('INSERT INTO native_sessions VALUES(?,?,?,?)',(hashlib.sha256(secret.encode()).digest(),csrf,subject,int(time.time())+3600))
            db.close()
            cookie={'Cookie':'__Host-bog_session='+secret,'Origin':'https://cloud.bog.new','x-csrf-token':csrf}
            status,session=request('GET','/console-session',headers=cookie)
            assert status==200,(status,session)
            workspace=session['workspaces'][0]['id']
            status,claimed=request('POST',f'/claim/{code}',{'workspace_id':workspace},headers=cookie)
            assert status==200 and claimed['id']==bog,(status,claimed)
            assert request('POST',f'/claim/{code}',{'workspace_id':workspace},headers=cookie)[0]==200
            assert request('GET',f'/claim/{code}/status')[1]['state']=='claimed'
            assert request('GET',f'/v1/bogs/{bog}',token=new_token)[0]==401
            assert request('GET',f'/v1/bogs/{bog}',headers=cookie)[0]==200
            assert request('GET',f'/v1/bogs/{bog}/docs/n1',headers=cookie)[0]==200
            status,issued=request('POST',f'/v1/claimable-bogs/{probes[0]["id"]}/claim',token=probes[0]['credential']['access_token'])
            assert status==200,(status,issued)
            other_code=issued['claim_url'].rsplit('/',1)[-1]
            assert request('POST',f'/claim/{other_code}',{'workspace_id':workspace,'name':'claimable-test'},headers=cookie)[0]==409
            status,renamed=request('POST',f'/claim/{other_code}',{'workspace_id':workspace,'name':'renamed-probe'},headers=cookie)
            assert status==200 and renamed['id']==probes[0]['id'],(status,renamed)
            python_config=base_dir/'temporary-python.json'
            python_run=subprocess.run([sys.executable,str(ROOT/'scripts/cloud/bog_cli.py'),'--origin',base,'try','--name','expire-test','--output',str(python_config)],capture_output=True,text=True,check=True)
            assert json.loads(python_run.stdout)['status']=='created'
            assert stat.S_IMODE(python_config.stat().st_mode)==0o600
            private=json.loads(python_config.read_text())
            second={'id':private['BOG_ID'],'credential':{'access_token':private['BOG_CLOUD_TOKEN']}}
            expiring=second['id']
            notes_definition=json.loads((ROOT/'docs/examples/composable/notes.json').read_text())
            definition_file=base_dir/'notes.json';definition_file.write_text(json.dumps(notes_definition))
            typescript_config=base_dir/'temporary-typescript.json'
            typescript_run=subprocess.run(['node',str(ROOT/'clients/typescript/src/cli.js'),'--origin',base,'try','--name','indexed-test','--output',str(typescript_config),'--definition',str(definition_file)],capture_output=True,text=True,check=True)
            assert json.loads(typescript_run.stdout)['status']=='created'
            assert stat.S_IMODE(typescript_config.stat().st_mode)==0o600
            private=json.loads(typescript_config.read_text())
            indexed={'id':private['BOG_ID'],'credential':{'access_token':private['BOG_CLOUD_TOKEN']}}
            indexed_token=indexed['credential']['access_token']
            status,route_data=request('GET',f'/v1/bogs/{indexed["id"]}/routes',token=indexed_token)
            assert status==200,(status,route_data)
            routes={route['op']:route for route in route_data['routes']}
            for _ in range(50):
                status,detail=request('GET',f'/v1/bogs/{indexed["id"]}',token=indexed_token)
                if status==200 and detail.get('status')=='ready':break
                time.sleep(.1)
            else:raise RuntimeError('Indexed Bog did not become ready')
            put_path=routes['put']['path'].replace('{key}','n1')
            assert request('PUT',put_path,{'title':'Woodland cabin','body':'Quiet forest retreat','updated_at':1},token=indexed_token)[0] in (200,201)
            status,search=request('POST',routes['text']['path'],{'query':'woodland','limit':3},token=indexed_token)
            assert status==200 and search.get('data'),(status,search)
            db=sqlite3.connect(base_dir/'data/registry.sqlite')
            with db:
                db.execute('UPDATE claimable_bogs SET expires_at=? WHERE bog_id=?',(int(time.time())-1,expiring))
                db.execute('UPDATE tokens SET expires_at=? WHERE bog_id=?',(int(time.time())-1,expiring))
            db.close()
            assert request('GET',f'/v1/bogs/{expiring}',token=second['credential']['access_token'])[0]==401
            server.terminate();server.wait(timeout=20)
            server=start()
            db=sqlite3.connect(base_dir/'data/registry.sqlite')
            row=db.execute('SELECT state,deleted_at FROM claimable_bogs c JOIN bogs b ON b.id=c.bog_id WHERE c.bog_id=?',(expiring,)).fetchone()
            db.close()
            assert row[0]=='expired' and row[1] is not None,row
            print(json.dumps({'status':'passed','checks':['public-create','idempotent-recovery','scope-isolation','capacity','write-read','claim','name-collision-and-rename','credential-revocation','data-retention','python-private-cli','typescript-private-cli','custom-definition-search','restart-expiry']}))
        finally:
            if server.poll() is None:
                server.terminate()
                try:server.wait(timeout=20)
                except subprocess.TimeoutExpired:server.kill();server.wait()

if __name__=='__main__':main()
