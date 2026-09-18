import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from bog_client import Client, Failure, BogError

class ClientTests(unittest.TestCase):
    def test_add_search_preserves_definition_and_waits(self):
        original={'version':1,'input':'docs','resources':{'docs':{'terminal':{'kind':'table'}}},'expose':{'put':{'target':'docs','action':'put'}}}
        before=copy.deepcopy(original); client=Client('https://bog.example','secret','bog')
        replies=[{'definition':original,'revision':2},{'compatible':True},{'job_id':'job','status':'building'},{'job_id':'job','status':'activating'},{'job_id':'job','status':'succeeded'}]
        with patch.object(client,'request',side_effect=replies) as req, patch('bog_client.time.sleep'):
            self.assertEqual(client.add_search('semantic_search',['/title'])['status'],'succeeded')
        self.assertEqual(original,before)
        body=req.call_args_list[2].args[2]
        self.assertEqual(body['expected_revision'],2)
        self.assertEqual(body['definition']['expose']['put'],before['expose']['put'])
        self.assertEqual(body['definition']['resources']['docs'],before['resources']['docs'])
    def test_conflict_does_not_apply(self):
        client=Client('https://bog.example','secret','bog')
        with patch.object(client,'request',side_effect=[{'definition':{'resources':{},'expose':{}},'revision':2},BogError(409,'request_rejected')]) as req:
            with self.assertRaises(BogError):client.add_search('words',['/title'],'bm25')
        self.assertEqual(req.call_count,2)
    def test_failed_job_never_reports_success(self):
        client=Client('https://bog.example','secret','bog')
        with patch.object(client,'request',side_effect=[{'definition':{'resources':{},'expose':{}},'revision':2},{},{'job_id':'job','status':'failed'}]):
            with self.assertRaisesRegex(Failure,'failed'):client.add_search('words',['/title'],'bm25')
    def test_config_permissions_and_symlinks(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'private.json';p.write_text(json.dumps({'BOG_CLOUD_URL':'https://bog.example','BOG_ID':'bog','BOG_CLOUD_TOKEN':'secret'}));p.chmod(0o600)
            self.assertEqual(Client.from_config(p).bog_id,'bog')
            p.chmod(0o644)
            with self.assertRaises(Failure):Client.from_config(p)
            link=Path(d)/'link';link.symlink_to(p)
            with self.assertRaises(OSError):Client.from_config(link)

    def test_capacity_backoff_keeps_same_revision_and_body(self):
        client=Client('https://bog.example','secret','bog')
        replies=[{'definition':{'resources':{},'expose':{}},'revision':2},{},BogError(429,'capacity',2),{'job_id':'job','status':'succeeded'}]
        with patch.object(client,'request',side_effect=replies) as req, patch('bog_client.time.sleep') as sleep:
            client.add_search('words',['/title'],'bm25')
        sleep.assert_called_once_with(2)
        self.assertEqual(req.call_args_list[2],req.call_args_list[3])

class WireParityTests(unittest.TestCase):
    def test_shared_contract_over_real_http(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        fixtures=json.loads((Path(__file__).resolve().parents[2]/'clients/fixtures/requests.json').read_text())
        received=[]
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def dispatch(self):
                raw=self.rfile.read(int(self.headers.get('Content-Length','0')))
                received.append({'http':self.command,'path':self.path,'body':json.loads(raw) if raw else None,'auth':self.headers.get('Authorization')})
                self.send_response(200);self.end_headers();self.wfile.write(b'{"ok":true}')
            do_GET=do_PUT=do_POST=do_DELETE=dispatch
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=server.serve_forever);thread.start()
        try:
            c=Client('http://127.0.0.1:'+str(server.server_port),'private','bog')
            for f in fixtures:
                self.assertEqual(getattr(c,f['method'])(*f['args']),{'ok':True})
                actual=received[-1];self.assertEqual(actual.pop('auth'),'Bearer private')
                self.assertEqual(actual,{k:f[k] for k in ('http','path','body')})
        finally: server.shutdown();server.server_close();thread.join()

    def test_dotenv_and_validation(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'config.env';path.write_text('BOG_CLOUD_URL="http://127.0.0.1:1234"\nBOG_CLOUD_TOKEN="secret"\nBOG_ID="bog"\n');path.chmod(0o600)
            self.assertEqual(Client.from_config(path).bog_id,'bog')
            path.write_text('{"BOG_CLOUD_URL":"https://bog.example","BOG_ID":3,"BOG_CLOUD_TOKEN":"secret"}')
            with self.assertRaises(Failure):Client.from_config(path)

    def test_invalid_paging_and_batch_rejected_before_request(self):
        c=Client('https://bog.example','secret','bog')
        with patch.object(c,'request') as req:
            for keys in ([],['a']*101,[3]):
                with self.assertRaises(Failure):c.batch_get(keys)
            req.assert_not_called()
            c.list(after='a',before='z')
            self.assertEqual(req.call_args.args[2],{'limit':100,'after':'a','before':'z'})

class HistoryTests(unittest.TestCase):
    def test_cursor_before_snapshot_and_reset_refetch(self):
        from unittest.mock import Mock
        spec=importlib.util.spec_from_file_location('bog_sync',Path(__file__).resolve().parents[2]/'starters/cloud-chat/sync.py')
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        client=Mock();client.wait.side_effect=[{'cursor':'initial'},{'cursor':'reset','changed':False,'reset':True}];client.top.return_value={'data':[]}
        iterator=module.snapshots(client);next(iterator);next(iterator);iterator.close()
        self.assertEqual([c[0] for c in client.mock_calls],['wait','top','wait','top'])
        self.assertEqual(client.wait.call_args.args,('initial',))

class UncertainWriteTests(unittest.TestCase):
    def test_write_transport_failure_is_never_retried(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        seen=[]
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args): pass
            def do_PUT(self):
                seen.append(self.path);self.close_connection=True
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=server.serve_forever);thread.start()
        try:
            client=Client('http://127.0.0.1:'+str(server.server_port),'private','bog')
            with self.assertRaisesRegex(Failure,'may have completed'):client.put('key',{'private':'record'})
            self.assertEqual(len(seen),1)
        finally:server.shutdown();server.server_close();thread.join()

class LifecycleClientTests(unittest.TestCase):
    def test_create_options_and_confirmations(self):
        c=Client('https://bog.example','secret')
        with patch.object(c,'request',return_value={'id':'bog','status':'starting','app_access':{'handoff_id':'handoff'}}) as req:
            result=c.create('name','stable',wait=False,sandbox=True,app_access={'scope':'read','label':'app'})
            self.assertEqual(result['app_access']['handoff_id'],'handoff')
            self.assertEqual(req.call_args.args[2],{'name':'name','wait':False,'sandbox':True,'app_access':{'scope':'read','label':'app'}})
        with patch.object(c,'request') as req:
            with self.assertRaises(Failure):c.delete_sandbox('other')
            with self.assertRaises(Failure):c.execute_cleanup('preview','other')
            req.assert_not_called()

    def test_cli_private_install_reuses_helper_with_json_stdout(self):
        import bog_cli
        import contextlib
        import io
        output=io.StringIO()
        with patch.object(bog_cli,'install_private') as install,contextlib.redirect_stdout(output):
            self.assertEqual(bog_cli.main(['--auth-file','auth','install','--handoff','handoff','--output','config','--format','dotenv']),0)
        self.assertEqual(json.loads(output.getvalue()),{'status':'installed','configuration':'config'})
        self.assertIn('--auth-file',install.call_args.args[0])

class PrivateInteropAndThrottleTests(unittest.TestCase):
    def test_helper_dotenv_roundtrip_with_quoted_characters(self):
        from bog_app_access import write_private
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'app.env'
            token="private'\\$`literal`"
            write_private(path,{'BOG_CLOUD_URL':'https://bog.example','BOG_ID':'bog','BOG_CLOUD_TOKEN':token},output_format='dotenv')
            self.assertEqual(Client.from_config(path)._token,token)

    def test_explicit_throttle_retry_is_bounded_and_preserves_request(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        import threading
        seen=[]
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_PUT(self):
                seen.append((self.path,self.headers.get('Idempotency-Key'),self.rfile.read(int(self.headers.get('Content-Length','0')))))
                self.send_response(429 if len(seen)<3 else 200)
                if len(seen)==1:self.send_header('Retry-After','0')
                self.end_headers()
                self.wfile.write(b'{"error":{"code":"rate_limited"}}' if len(seen)==1 else b'{"error":{"code":"rate_limited"},"retry_after_ms":1}' if len(seen)==2 else b'{"ok":true}')
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=threading.Thread(target=server.serve_forever);thread.start()
        try:
            c=Client('http://127.0.0.1:'+str(server.server_port),'private','bog')
            self.assertEqual(c.request('PUT',c.path('/docs/key'),{'value':1},'same'),{'ok':True})
            self.assertEqual(len(seen),3);self.assertTrue(all(v==seen[0] for v in seen))
        finally:server.shutdown();server.server_close();thread.join()
        for error,expected in [(BogError(429,'rate_limited',0),3),(BogError(429,'rate_limited',None),1),(BogError(429,'rate_limited',60),1),(BogError(409,'conflict',0),1)]:
            with patch.object(c,'_request_once',side_effect=error) as call,patch('bog_client.time.sleep'):
                with self.assertRaises(BogError):c.put('key',{'value':1})
                self.assertEqual(call.call_count,expected)
