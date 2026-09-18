#!/usr/bin/env python3
"""Loopback-only persistent notebook starter. No static directory is exposed."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import urllib.parse
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts/cloud'))
from bog_client import Client, BogError, Failure

PAGE = Path(__file__).with_name('index.html').read_bytes()


def handler(client, port):
    expected = f'127.0.0.1:{port}'
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def reply(self, code, value, html=False):
            data = value if html else json.dumps(value).encode()
            self.send_response(code)
            self.send_header('Content-Type', 'text/html; charset=utf-8' if html else 'application/json')
            self.send_header('Cache-Control', 'no-store')
            self.send_header('X-Content-Type-Options', 'nosniff')
            self.send_header('Content-Security-Policy', "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'")
            self.send_header('Content-Length', str(len(data)))
            self.end_headers(); self.wfile.write(data)
        def dispatch(self):
            if self.headers.get('Host') != expected or self.headers.get('Origin', 'http://' + expected) != 'http://' + expected or self.headers.get('Sec-Fetch-Site') == 'cross-site':
                return self.reply(403, {'error':'Cross-origin requests are not allowed.'})
            url=urllib.parse.urlsplit(self.path)
            try:
                if self.command == 'GET' and url.path == '/': return self.reply(200,PAGE,True)
                if self.command == 'GET' and url.path == '/api/notes': return self.reply(200,client.query('docs',limit=100))
                if self.command == 'GET' and url.path == '/api/search':
                    args=urllib.parse.parse_qs(url.query); query=args.get('q',[''])[0]; mode=args.get('mode',['semantic'])[0]
                    if mode not in ('semantic','words') or not query or len(query.encode())>4096: return self.reply(400,{'error':'Invalid search.'})
                    return self.reply(200,client.search('semantic_search' if mode=='semantic' else 'text_search',query,include_fields=['/title','/body']))
                if self.command == 'POST' and url.path == '/api/notes':
                    if self.headers.get('Content-Type') != 'application/json': return self.reply(415,{'error':'JSON required.'})
                    length=int(self.headers.get('Content-Length','0'))
                    if not 0 < length <= 16384: return self.reply(413,{'error':'Note too large.'})
                    value=json.loads(self.rfile.read(length))
                    if not isinstance(value,dict) or set(value)!={'key','title','body'} or any(not isinstance(value[k],str) for k in value) or not value['key'] or len(value['key'])>128 or len((value['title']+value['body']).encode())>8000: return self.reply(400,{'error':'Invalid note.'})
                    return self.reply(200,client.put(value['key'],{'title':value['title'],'body':value['body']}))
                return self.reply(404,{'error':'Not found.'})
            except BogError as error: return self.reply(error.status,{'error':str(error)})
            except (Failure,OSError,ValueError,KeyError,TypeError): return self.reply(400,{'error':'Request failed. Check input, connection, or renew the private credential.'})
        do_GET=dispatch
        do_POST=dispatch
    return Handler

if __name__=='__main__':
    parser=argparse.ArgumentParser(); parser.add_argument('--config',required=True); parser.add_argument('--port',type=int,default=4317); args=parser.parse_args()
    client=Client.from_config(args.config)
    client.resources()  # Verify access before announcing the preview.
    server=ThreadingHTTPServer(('127.0.0.1',args.port),handler(client,args.port))
    print(f'Notebook ready at http://127.0.0.1:{args.port}',flush=True)
    try: server.serve_forever()
    except KeyboardInterrupt: server.server_close()
