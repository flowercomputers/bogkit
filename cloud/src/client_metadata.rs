//! Untrusted HTTPS client metadata: resolve once, reject non-public addresses,
//! then pin that resolution for the entire bounded fetch (including TLS SNI).
use crate::CloudError;
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

const MAX_BYTES: usize = 64 * 1024;
static FETCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);
fn invalid() -> CloudError {
    CloudError::new(
        "invalid_client",
        "invalid or unavailable client metadata document",
    )
}
#[derive(Deserialize)]
struct Document {
    client_id: String,
    client_name: String,
    #[serde(default)]
    application_type: Option<String>,
    redirect_uris: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
}
pub(crate) struct ClientMetadata {
    pub name: String,
    pub redirects: Vec<String>,
    pub native_loopback: bool,
}
impl ClientMetadata {
    pub fn allows_redirect(&self, requested: &str) -> bool {
        self.redirects.iter().any(|registered| {
            registered == requested
                || (self.native_loopback
                    && loopback_parts(registered)
                        .is_some_and(|parts| Some(parts) == loopback_parts(requested)))
        })
    }
}
// Compare the original spelling rather than normalized URLs: only the port may
// vary. Even equivalent host spellings, path escapes and query changes differ.
fn loopback_parts(value: &str) -> Option<(&str, &str)> {
    if value.len() > 2048 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let u = reqwest::Url::parse(value).ok()?;
    if u.fragment().is_some() || !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    let rest = value.strip_prefix("http://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, suffix) = rest.split_at(end);
    for host in ["127.0.0.1", "[::1]", "localhost"] {
        if let Some(port) = authority.strip_prefix(host)
            && (port.is_empty()
                || port.strip_prefix(':').is_some_and(|p| {
                    !p.is_empty()
                        && p.bytes().all(|c| c.is_ascii_digit())
                        && p.parse::<u16>().is_ok_and(|p| p > 0)
                }))
        {
            return Some((host, suffix));
        }
    }
    None
}
fn url(value: &str) -> Result<reqwest::Url, CloudError> {
    let u = reqwest::Url::parse(value).map_err(|_| invalid())?;
    if value.len() > 2048
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.path() == "/"
    {
        return Err(invalid());
    }
    Ok(u)
}
fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only global unicast, excluding protocol assignments, documentation,
            // transition mechanisms and their embedded private IPv4 destinations.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
fn validate_addresses(addresses: &[SocketAddr]) -> Result<(), CloudError> {
    if addresses.is_empty() || addresses.len() > 16 || addresses.iter().any(|a| !public(a.ip())) {
        return Err(invalid());
    }
    Ok(())
}
fn validate(client: &str, bytes: &[u8]) -> Result<ClientMetadata, CloudError> {
    let d: Document = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    // Some native clients omit application_type. A public client whose entire
    // redirect list is literal HTTP loopback can safely use the same port rule.
    let native_loopback = d.application_type.as_deref() == Some("native")
        || (d.application_type.is_none()
            && !d.redirect_uris.is_empty()
            && d.redirect_uris.iter().all(|r| loopback_parts(r).is_some()));
    if d.client_id != client
        || crate::agent_tokens::validate_name(&d.client_name).is_err()
        || d.redirect_uris.is_empty()
        || d.redirect_uris.len() > 8
        || d.redirect_uris.iter().any(|r| {
            r.len() > 2048
                || !(super::native_oauth::redirect_valid(r)
                    || (native_loopback && loopback_parts(r).is_some()))
        })
        || d.token_endpoint_auth_method
            .as_deref()
            .is_some_and(|v| v != "none")
        || d.grant_types.as_ref().is_some_and(|v| {
            !v.iter().any(|g| g == "authorization_code")
                || v.len() > 2
                || v.iter()
                    .any(|g| g != "authorization_code" && g != "refresh_token")
                || (v.len() == 2 && v[0] == v[1])
        })
        || d.response_types.as_ref().is_some_and(|v| v != &["code"])
    {
        return Err(invalid());
    }
    Ok(ClientMetadata {
        name: d.client_name,
        redirects: d.redirect_uris,
        native_loopback,
    })
}
fn client(host: &str, addresses: &[SocketAddr]) -> Result<reqwest::Client, CloudError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .resolve_to_addrs(host, addresses)
        .build()
        .map_err(|_| invalid())
}
async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, CloudError> {
    if response.status() != reqwest::StatusCode::OK
        || response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        || response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v.split(';').next().unwrap_or("").trim() != "application/json")
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
        if chunk.len() > MAX_BYTES - bytes.len() {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub(crate) async fn fetch(value: &str) -> Result<ClientMetadata, CloudError> {
    let _permit = FETCHES.try_acquire().map_err(|_| invalid())?;
    let u = url(value)?;
    tokio::time::timeout(Duration::from_secs(5), async {
        let host = u.host_str().ok_or_else(invalid)?;
        let port = u.port_or_known_default().unwrap_or(443);
        let addresses: Vec<_> = match host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
        {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| invalid())?
                .take(17)
                .collect(),
        };
        validate_addresses(&addresses)?;
        let response = client(host, &addresses)?
            .get(u.clone())
            .send()
            .await
            .map_err(|_| invalid())?;
        validate(value, &body(response).await?)
    })
    .await
    .map_err(|_| invalid())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_document_and_public_destinations() {
        let id = "https://client.example/metadata.json";
        let good = serde_json::json!({"client_id":id,"client_name":"Client","redirect_uris":["http://127.0.0.1:9999/callback"],"token_endpoint_auth_method":"none"});
        assert!(validate(id, &serde_json::to_vec(&good).unwrap()).is_ok());
        for (field, value) in [
            (
                "client_id",
                serde_json::json!("https://CLIENT.example/metadata.json"),
            ),
            (
                "redirect_uris",
                serde_json::json!(["https://user@client.example/cb"]),
            ),
            ("redirect_uris", serde_json::json!([])),
            (
                "token_endpoint_auth_method",
                serde_json::json!("client_secret_post"),
            ),
            ("grant_types", serde_json::json!(["implicit"])),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(validate(id, &serde_json::to_vec(&bad).unwrap()).is_err());
        }
        assert!(validate(id, br#"{"client_id":"x","client_id":"y"}"#).is_err());
        for value in [
            "http://client.example/a",
            "https://client.example/",
            "https://user@client.example/a",
            "https://client.example/a#x",
        ] {
            assert!(url(value).is_err());
        }
        for value in [
            "127.0.0.1",
            "10.1.2.3",
            "100.64.0.1",
            "169.254.169.254",
            "172.31.1.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2002:7f00:1::",
            "2001:db8::1",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public(value.parse().unwrap()), "{value}");
        }
        let good: SocketAddr = "8.8.8.8:443".parse().unwrap();
        assert!(validate_addresses(&[good]).is_ok());
        assert!(validate_addresses(&[good, "127.0.0.1:443".parse().unwrap()]).is_err());
        assert!(validate_addresses(&[]).is_err());
        assert!(validate_addresses(&[good; 17]).is_err());
    }
    #[test]
    fn official_native_client_shapes_allow_only_loopback_port_variation() {
        let codex = serde_json::json!({"client_id":"https://chatgpt.com/oauth/codex/client.json","client_uri":"https://chatgpt.com/codex","application_type":"native","redirect_uris":["http://127.0.0.1/callback","http://localhost/callback"],"token_endpoint_auth_method":"none","token_endpoint_auth_methods_supported":["none"],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"client_name":"Codex","logo_uri":"https://persistent.oaistatic.com/sonic/misc/openai-logo.png"});
        let claude = serde_json::json!({"client_id":"https://claude.ai/oauth/claude-code-client-metadata","client_name":"Claude Code","client_uri":"https://claude.ai","redirect_uris":["http://localhost/callback","http://127.0.0.1/callback"],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none"});
        for document in [codex, claude] {
            let metadata = validate(
                document["client_id"].as_str().unwrap(),
                &serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();
            for callback in [
                "http://127.0.0.1:53668/callback",
                "http://localhost:51180/callback",
            ] {
                assert!(metadata.allows_redirect(callback), "{callback}");
            }
            for callback in [
                "http://127.0.0.2:53668/callback",
                "http://127.1:53668/callback",
                "http://LOCALHOST:51180/callback",
                "http://localhost.example:51180/callback",
                "http://[::1]:51180/callback",
                "http://localhost:51180/other",
                "http://localhost:51180/callback/",
                "http://localhost:51180/call%62ack",
                "http://localhost:51180/callback?extra=1",
                "http://localhost:51180/callback#fragment",
                "http://user@localhost:51180/callback",
                "https://localhost:51180/callback",
                "http://localhost:0/callback",
            ] {
                assert!(!metadata.allows_redirect(callback), "{callback}");
            }
        }
        let metadata = ClientMetadata {
            name: "fixture".into(),
            redirects: vec![
                "http://[::1]/callback?key=value".into(),
                "https://client.example:9443/callback".into(),
            ],
            native_loopback: true,
        };
        assert!(metadata.allows_redirect("http://[::1]:53668/callback?key=value"));
        assert!(!metadata.allows_redirect("http://[::1]:53668/callback?key=changed"));
        assert!(!metadata.allows_redirect("http://[::1]:53668/callback"));
        assert!(!metadata.allows_redirect("https://client.example:9444/callback"));
        assert!(metadata.allows_redirect("https://client.example:9443/callback"));
        let dcr = ClientMetadata {
            native_loopback: false,
            ..metadata
        };
        assert!(!dcr.allows_redirect("http://[::1]:53668/callback?key=value"));
        assert!(dcr.allows_redirect("http://[::1]/callback?key=value"));
        let explicit_web = serde_json::json!({"client_id":"https://example.com/client.json","client_name":"Web","application_type":"web","redirect_uris":["http://127.0.0.1/callback"]});
        let web = validate(
            "https://example.com/client.json",
            &serde_json::to_vec(&explicit_web).unwrap(),
        )
        .unwrap();
        assert!(!web.allows_redirect("http://127.0.0.1:53668/callback"));
        let mixed = serde_json::json!({"client_id":"https://example.com/client.json","client_name":"Mixed","redirect_uris":["http://127.0.0.1/callback","https://example.com/callback"]});
        let mixed = validate(
            "https://example.com/client.json",
            &serde_json::to_vec(&mixed).unwrap(),
        )
        .unwrap();
        assert!(!mixed.allows_redirect("http://127.0.0.1:53668/callback"));
    }
    #[tokio::test]
    async fn private_destinations_fail_before_fetch() {
        for value in [
            "https://127.0.0.1/metadata.json",
            "https://localhost/metadata.json",
            "https://[::1]/metadata.json",
        ] {
            assert!(fetch(value).await.is_err());
        }
    }
    #[tokio::test]
    async fn pinned_fetch_rejects_redirects_status_types_and_streamed_oversize() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (headers, payload, expected) in [
            (
                "200 OK\r\nContent-Type: application/json",
                b"{}".to_vec(),
                true,
            ),
            (
                "302 Found\r\nLocation: http://127.0.0.1:1/secret\r\nContent-Type: application/json",
                b"{}".to_vec(),
                false,
            ),
            (
                "500 Internal Server Error\r\nContent-Type: application/json",
                b"{}".to_vec(),
                false,
            ),
            ("200 OK\r\nContent-Type: text/html", b"{}".to_vec(), false),
            (
                "200 OK\r\nContent-Type: application/json",
                vec![b' '; MAX_BYTES + 1],
                false,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                // No Content-Length: the streaming cap must still apply.
                let response = format!(
                    "HTTP/1.1 {headers}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                    payload.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&payload).await.unwrap();
                stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
            });
            // HTTP is used only to test the fetch machinery locally. Production
            // validates HTTPS and public addresses before constructing this client.
            let response = client("metadata.invalid", &[address])
                .unwrap()
                .get(format!(
                    "http://metadata.invalid:{}/client.json",
                    address.port()
                ))
                .send()
                .await
                .unwrap();
            assert_eq!(body(response).await.is_ok(), expected);
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn stalled_response_has_a_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
        });
        let response = client("metadata.invalid", &[address])
            .unwrap()
            .get(format!(
                "http://metadata.invalid:{}/client.json",
                address.port()
            ))
            .send()
            .await
            .unwrap();
        assert!(body(response).await.is_err());
        task.abort();
    }
}
