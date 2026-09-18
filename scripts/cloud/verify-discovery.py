#!/usr/bin/env python3
"""Read-only production discovery checks; no credentials or GitHub codes emitted."""
import json
import urllib.error
import urllib.parse
import urllib.request

CLOUD = 'https://cloud.bog.new'
LEGACY = 'https://flower-bog-cloud.fly.dev'
MCP = 'https://mcp.bog.new'

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

opener = urllib.request.build_opener(NoRedirect)

def request(url, method='GET'):
    req = urllib.request.Request(url, method=method)
    try:
        response = opener.open(req, timeout=20)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()

def document(url):
    status, _, body = request(url)
    assert status == 200, (url, status)
    return json.loads(body)

for issuer in (CLOUD, LEGACY):
    metadata = document(issuer + '/.well-known/oauth-authorization-server')
    assert metadata['issuer'] == issuer, f'Issuer mismatch at {issuer}'
    for field, path in [('authorization_endpoint', '/oauth/authorize'),
                        ('token_endpoint', '/oauth/token'),
                        ('registration_endpoint', '/oauth/register'),
                        ('revocation_endpoint', '/oauth/revoke')]:
        assert metadata[field] == issuer + path, (issuer, field)
    assert 'S256' in metadata['code_challenge_methods_supported']
    print('PASS exact issuer and endpoint discovery:', issuer)

for origin in (CLOUD, MCP, LEGACY):
    resource = document(origin + '/.well-known/oauth-protected-resource/mcp')
    assert resource['resource'] == origin + '/mcp'
    expected_issuer = LEGACY if origin == LEGACY else CLOUD
    assert resource['authorization_servers'] == [expected_issuer]
    status, headers, _ = request(origin + '/mcp', method='POST')
    assert status == 401
    assert origin + '/.well-known/oauth-protected-resource/mcp' in headers['www-authenticate']
    print('PASS protected-resource discovery and challenge:', origin)

status, headers, _ = request(MCP + '/.well-known/oauth-authorization-server')
assert status == 404 and CLOUD in headers['link']
assert document(CLOUD + '/.well-known/oauth-protected-resource')['resource'] == CLOUD
assert document(LEGACY + '/.well-known/oauth-protected-resource')['resource'] == LEGACY
status, headers, _ = request(CLOUD + '/auth/login')
assert status == 303
url = urllib.parse.urlparse(headers['location'])
params = urllib.parse.parse_qs(url.query)
assert url.scheme == 'https' and url.netloc == 'github.com'
assert params['redirect_uri'] == [CLOUD + '/auth/callback']
assert params.get('scope', ['']) == ['']
assert params['code_challenge_method'] == ['S256']
assert '__Host-bog_login=' in headers['set-cookie']
for origin in (LEGACY, MCP):
    status, headers, _ = request(origin + '/console')
    assert status == 307 and headers['location'] == CLOUD + '/console'
    assert 'set-cookie' not in headers
print('PASS canonical GitHub callback, no repository scopes, and console redirects')
