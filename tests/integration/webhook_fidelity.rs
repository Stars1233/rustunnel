//! HMAC-signed-webhook fidelity — end-to-end integration test.
//!
//! # What this tests
//!
//! The invariant that real webhook providers (Twilio, Stripe, GitHub…)
//! depend on: **the proxy must not change bytes**, and it must give the
//! backend enough information (`X-Forwarded-*`) to reconstruct the exact
//! public URL the provider signed.
//!
//! Twilio's scheme is the strictest of the lot — HMAC-SHA1 over
//! `URL + sorted(param_name + param_value)` — so one flipped byte in the
//! form body (e.g. a `%2B` decoded to `+` and re-emitted literally) or a
//! wrong reconstructed URL kills the signature. This suite:
//!
//! 1. signs a Twilio-style form POST against the public tunnel URL,
//! 2. sends it through the full proxy chain (HTTPS edge and plain-HTTP
//!    edge in `proxy` mode),
//! 3. captures the *raw bytes* that reach the local service (no HTTP
//!    library parsing on the receiving side),
//! 4. re-validates the signature the way a real backend would — URL
//!    rebuilt from `X-Forwarded-Proto`/`X-Forwarded-Host`, params parsed
//!    from the received body bytes,
//! 5. asserts the body arrived byte-identical.
//!
//! Also covers the plain-HTTP fallback: unresolvable hosts must get a 308
//! (method-preserving) redirect to the HTTPS listener — a 301 here turned
//! followed webhook POSTs into GETs.

#[path = "../common/mod.rs"]
mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use common::*;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Twilio-style body: `+` phone numbers (percent-encoded), a literal `+`
/// meaning space, an empty param, raw UTF-8, and pre-encoded reserved chars.
const FORM_BODY: &str = "Body=hello+world&From=%2B31615940830&FromZip=&To=%2B31682223345&Uni=%C3%A9&Special=a%20b%26c%3Dd";
const AUTH_TOKEN: &str = "twilio-test-auth-token";

// ── raw-byte capture server ───────────────────────────────────────────────────

/// A local "service" that never parses the request: it records the exact
/// bytes on the wire (head + body) and answers 200.
async fn start_raw_capture_server() -> (SocketAddr, Arc<Mutex<Vec<Vec<u8>>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind raw capture server");
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));

    let store = captured.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 65536];
                // Read until end-of-headers, then Content-Length more bytes.
                let (head_len, content_len) = loop {
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = &raw[..pos];
                        let clen = std::str::from_utf8(head)
                            .ok()
                            .and_then(|h| {
                                h.lines().find_map(|l| {
                                    let (k, v) = l.split_once(':')?;
                                    k.eq_ignore_ascii_case("content-length")
                                        .then(|| v.trim().parse::<usize>().ok())?
                                })
                            })
                            .unwrap_or(0);
                        break (pos + 4, clen);
                    }
                };
                while raw.len() < head_len + content_len {
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                }
                store.lock().await.push(raw);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nOK")
                    .await;
            });
        }
    });

    (addr, captured)
}

// ── Twilio-style signature helpers ────────────────────────────────────────────

/// Compute the Twilio request signature: base64(HMAC-SHA1(auth_token,
/// url + concat(sorted(param_name + param_value)))).
fn twilio_signature(auth_token: &str, url: &str, form_body: &str) -> String {
    let mut params: Vec<(String, String)> = form_body
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect();
    params.sort();

    let mut data = url.to_string();
    for (k, v) in &params {
        data.push_str(k);
        data.push_str(v);
    }

    let mut mac =
        Hmac::<Sha1>::new_from_slice(auth_token.as_bytes()).expect("hmac accepts any key length");
    mac.update(data.as_bytes());
    base64_encode(&mac.finalize().into_bytes())
}

/// Minimal application/x-www-form-urlencoded decoder (`+` → space, `%XX`).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ── raw-request inspection helpers ────────────────────────────────────────────

struct ReceivedRequest {
    head: String,
    body: Vec<u8>,
}

fn parse_raw(raw: &[u8]) -> ReceivedRequest {
    let pos = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("received request has header terminator");
    ReceivedRequest {
        head: String::from_utf8_lossy(&raw[..pos]).into_owned(),
        body: raw[pos + 4..].to_vec(),
    }
}

impl ReceivedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    fn path(&self) -> &str {
        self.head.lines().next().unwrap().split(' ').nth(1).unwrap()
    }

    /// Reconstruct the public URL the way a backend behind a proxy does.
    fn reconstructed_url(&self) -> String {
        let proto = self.header("x-forwarded-proto").expect("X-Forwarded-Proto");
        let host = self.header("x-forwarded-host").expect("X-Forwarded-Host");
        format!("{proto}://{host}{}", self.path())
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Full Twilio flow over the HTTPS edge: sign against the public URL, POST
/// through the tunnel, validate server-side from the received raw bytes.
#[tokio::test]
async fn signed_webhook_survives_https_edge() {
    init_tracing();

    let (local_addr, captured) = start_raw_capture_server().await;
    let server = TestServer::start().await;

    let mut client = TestClient::connect(&server).await.expect("client auth");
    let session_id = client.session_id.unwrap();
    let (_, subdomain, _) = client
        .register_http_tunnel(Some("twiliotest"))
        .await
        .expect("tunnel registration");
    connect_data_bridge(&server, session_id, local_addr)
        .await
        .expect("data bridge ready");

    // "Twilio" signs against the public URL it was configured with.
    let host = format!("{subdomain}.{}", server.domain);
    let public_url = format!(
        "https://{host}:{}/api/webhooks/sms/inbound",
        server.https_port
    );
    let signature = twilio_signature(AUTH_TOKEN, &public_url, FORM_BODY);

    let resp = insecure_http_client()
        .post(format!(
            "https://127.0.0.1:{}/api/webhooks/sms/inbound",
            server.https_port
        ))
        .header("Host", format!("{host}:{}", server.https_port))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", &signature)
        .body(FORM_BODY)
        .send()
        .await
        .expect("signed POST through tunnel");
    assert_eq!(resp.status(), 200);

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1, "expected exactly one captured request");
    let req = parse_raw(&captured[0]);

    // 1. Body must be byte-identical — one flipped byte kills the HMAC.
    assert_eq!(
        req.body,
        FORM_BODY.as_bytes(),
        "request body was not byte-faithful through the tunnel"
    );

    // 2. Signature header must arrive intact.
    assert_eq!(req.header("x-twilio-signature"), Some(signature.as_str()));

    // 3. Backend-side validation: rebuild the URL from X-Forwarded-* and the
    //    received body bytes, recompute, compare. This is exactly what
    //    twilio's SDK validators do behind a proxy.
    let backend_sig = twilio_signature(
        AUTH_TOKEN,
        &req.reconstructed_url(),
        std::str::from_utf8(&req.body).unwrap(),
    );
    assert_eq!(
        backend_sig,
        signature,
        "backend-side signature validation failed: reconstructed URL {}",
        req.reconstructed_url()
    );

    assert_eq!(req.header("x-forwarded-proto"), Some("https"));
    assert!(req.header("x-forwarded-for").is_some());
}

/// Same flow over the plain-HTTP edge in `proxy` mode — an `http://` webhook
/// URL must work without a redirect hop (ngrok parity).
#[tokio::test]
async fn signed_webhook_survives_plain_http_edge() {
    init_tracing();

    let (local_addr, captured) = start_raw_capture_server().await;
    let server = TestServer::start().await;

    let mut client = TestClient::connect(&server).await.expect("client auth");
    let session_id = client.session_id.unwrap();
    let (_, subdomain, _) = client
        .register_http_tunnel(Some("twilioplain"))
        .await
        .expect("tunnel registration");
    connect_data_bridge(&server, session_id, local_addr)
        .await
        .expect("data bridge ready");

    let host = format!("{subdomain}.{}", server.domain);
    let public_url = format!(
        "http://{host}:{}/api/webhooks/sms/inbound",
        server.http_port
    );
    let signature = twilio_signature(AUTH_TOKEN, &public_url, FORM_BODY);

    let resp = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/api/webhooks/sms/inbound",
            server.http_port
        ))
        .header("Host", format!("{host}:{}", server.http_port))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", &signature)
        .body(FORM_BODY)
        .send()
        .await
        .expect("signed POST through plain-HTTP tunnel");
    assert_eq!(
        resp.status(),
        200,
        "plain-HTTP proxy mode must not redirect tunnel traffic"
    );

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    let req = parse_raw(&captured[0]);

    assert_eq!(req.body, FORM_BODY.as_bytes());
    assert_eq!(req.header("x-forwarded-proto"), Some("http"));

    let backend_sig = twilio_signature(
        AUTH_TOKEN,
        &req.reconstructed_url(),
        std::str::from_utf8(&req.body).unwrap(),
    );
    assert_eq!(backend_sig, signature);
}

/// Plain-HTTP requests that do not resolve to a tunnel must get a 308
/// (method- and body-preserving), never a 301.
#[tokio::test]
async fn plain_http_fallback_redirects_with_308() {
    init_tracing();
    let server = TestServer::start().await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://127.0.0.1:{}/hook", server.http_port))
        .header("Host", "notregistered.localhost")
        .body("a=1")
        .send()
        .await
        .expect("POST to unresolvable host");

    assert_eq!(resp.status(), 308);
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert_eq!(
        location,
        &format!("https://notregistered.localhost:{}/hook", server.https_port)
    );
}
