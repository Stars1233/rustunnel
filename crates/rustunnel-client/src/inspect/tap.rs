//! Passive HTTP/1.x tap over a proxied connection.
//!
//! The tunnel client forwards raw bytes. To show requests in the UIs we need
//! method/path/status/headers/bodies, so the proxy feeds every byte it copies
//! through a [`ConnTap`] — an observer that parses HTTP framing but never
//! modifies, reorders, or delays the stream. Anything that stops looking like
//! HTTP/1.x (protocol upgrades, HTTP/2 prior knowledge, malformed traffic)
//! drops the connection into passthrough mode and the bytes keep flowing.
//!
//! Both directions of one connection share a [`ConnTap`]: requests and
//! responses are paired FIFO, which is what HTTP/1.1 keep-alive and pipelining
//! guarantee.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{Body, Exchange, Inspector};

/// Upper bound on a request/response head. Beyond this we stop parsing rather
/// than buffer unboundedly.
const MAX_HEAD: usize = 64 * 1024;

/// Maximum headers parsed per message.
const MAX_HEADERS: usize = 96;

/// Maximum requests awaiting a response on one connection.
///
/// Real pipelining depth is tiny; a queue this deep means the response side
/// stopped producing parseable replies, and the queue would otherwise grow for
/// as long as the connection lives.
const MAX_PENDING: usize = 64;

// ── body framing ──────────────────────────────────────────────────────────────

/// How the body of the message currently being parsed is delimited.
#[derive(Debug)]
enum Framing {
    /// `Content-Length: n`.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked(ChunkScanner),
    /// Response body that ends when the connection closes.
    UntilEof,
}

impl Framing {
    /// Consume as much of `buf` as belongs to this body, capturing it into
    /// `sink`. Returns the number of bytes consumed and whether the body ended.
    fn feed(&mut self, buf: &[u8], sink: Option<&mut Body>) -> (usize, bool) {
        match self {
            Framing::Length(remaining) => {
                let take = (*remaining).min(buf.len() as u64) as usize;
                if let Some(sink) = sink {
                    sink.push(&buf[..take]);
                }
                *remaining -= take as u64;
                (take, *remaining == 0)
            }
            Framing::UntilEof => {
                if let Some(sink) = sink {
                    sink.push(buf);
                }
                (buf.len(), false)
            }
            Framing::Chunked(scanner) => scanner.feed(buf, sink),
        }
    }
}

/// Incremental `Transfer-Encoding: chunked` scanner.
///
/// Tracks chunk boundaries across arbitrary buffer splits so we know where the
/// body ends and the next message on a keep-alive connection begins.
#[derive(Debug)]
struct ChunkScanner {
    state: ChunkState,
    /// Partial size or trailer line.
    line: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    /// Reading the hex chunk-size line.
    Size,
    /// Reading chunk data; `n` bytes remain.
    Data(u64),
    /// Consuming the CRLF that follows chunk data; `n` bytes seen so far.
    DataCrlf(u8),
    /// Reading trailer headers after the final chunk.
    Trailer,
    /// Body complete, or malformed and abandoned.
    Done,
}

impl ChunkScanner {
    fn new() -> Self {
        Self {
            state: ChunkState::Size,
            line: Vec::new(),
        }
    }

    fn feed(&mut self, buf: &[u8], mut sink: Option<&mut Body>) -> (usize, bool) {
        let mut i = 0;
        while i < buf.len() {
            match self.state {
                ChunkState::Size => {
                    let byte = buf[i];
                    i += 1;
                    self.line.push(byte);
                    if byte == b'\n' {
                        match parse_chunk_size(&self.line) {
                            Some(0) => self.state = ChunkState::Trailer,
                            Some(n) => self.state = ChunkState::Data(n),
                            None => self.state = ChunkState::Done,
                        }
                        self.line.clear();
                    } else if self.line.len() > 64 {
                        // A chunk-size line this long is not valid HTTP.
                        self.state = ChunkState::Done;
                    }
                }
                ChunkState::Data(remaining) => {
                    let take = remaining.min((buf.len() - i) as u64) as usize;
                    if let Some(sink) = sink.as_deref_mut() {
                        sink.push(&buf[i..i + take]);
                    }
                    i += take;
                    let left = remaining - take as u64;
                    self.state = if left == 0 {
                        ChunkState::DataCrlf(0)
                    } else {
                        ChunkState::Data(left)
                    };
                }
                ChunkState::DataCrlf(seen) => {
                    i += 1;
                    self.state = if seen + 1 >= 2 {
                        ChunkState::Size
                    } else {
                        ChunkState::DataCrlf(seen + 1)
                    };
                }
                ChunkState::Trailer => {
                    let byte = buf[i];
                    i += 1;
                    self.line.push(byte);
                    if byte == b'\n' {
                        let line_is_blank = matches!(self.line.as_slice(), b"\r\n" | b"\n");
                        self.line.clear();
                        if line_is_blank {
                            self.state = ChunkState::Done;
                            return (i, true);
                        }
                    } else if self.line.len() > MAX_HEAD {
                        self.state = ChunkState::Done;
                        return (i, true);
                    }
                }
                ChunkState::Done => return (i, true),
            }
        }
        (i, self.state == ChunkState::Done)
    }
}

/// Parse a chunk-size line (`"1a\r\n"` or `"1a;ext=1\r\n"`).
fn parse_chunk_size(line: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(line).ok()?;
    let text = text.trim_end_matches(['\r', '\n']);
    let text = text.split(';').next()?.trim();
    if text.is_empty() {
        return None;
    }
    u64::from_str_radix(text, 16).ok()
}

// ── per-direction parser ──────────────────────────────────────────────────────

#[derive(Debug)]
enum Mode {
    /// Accumulating a message head.
    Head,
    /// Reading a message body.
    Body(Framing),
    /// Not HTTP (or no longer HTTP) — stop looking.
    Passthrough,
}

#[derive(Debug)]
struct DirParser {
    mode: Mode,
    /// Bytes seen but not yet consumed by the parser.
    buffer: Vec<u8>,
}

impl DirParser {
    fn new() -> Self {
        Self {
            mode: Mode::Head,
            buffer: Vec::new(),
        }
    }

    fn passthrough(&mut self) {
        self.mode = Mode::Passthrough;
        self.buffer = Vec::new();
    }

    /// Offset just past the `\r\n\r\n` (or `\n\n`) ending the head, if present.
    fn head_end(&self) -> Option<usize> {
        let buf = &self.buffer;
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
    }
}

// ── in-flight state ───────────────────────────────────────────────────────────

/// A request whose response has not completed yet.
struct PendingRequest {
    method: String,
    path: String,
    host: Option<String>,
    headers: Vec<(String, String)>,
    body: Body,
    started_at: DateTime<Utc>,
    start: Instant,
}

/// Response head awaiting its body.
struct PendingResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

struct TapState {
    inspector: Arc<Inspector>,
    tunnel: String,
    conn_id: Uuid,
    client_addr: String,
    request: DirParser,
    response: DirParser,
    /// Requests awaiting a response, oldest first.
    pending: VecDeque<PendingRequest>,
    current_response: Option<PendingResponse>,
}

impl TapState {
    // ── request direction (tunnel → local service) ───────────────────────

    fn feed_request(&mut self, bytes: &[u8]) {
        if matches!(self.request.mode, Mode::Passthrough) {
            return;
        }
        self.request.buffer.extend_from_slice(bytes);

        loop {
            match std::mem::replace(&mut self.request.mode, Mode::Head) {
                Mode::Passthrough => {
                    self.request.passthrough();
                    return;
                }
                Mode::Head => {
                    let Some(end) = self.request.head_end() else {
                        if self.request.buffer.len() > MAX_HEAD {
                            self.request.passthrough();
                        }
                        return;
                    };
                    let head: Vec<u8> = self.request.buffer.drain(..end).collect();
                    match parse_request_head(&head) {
                        Some((method, path, headers)) => {
                            let framing = request_framing(&headers);
                            let host = header_value(&headers, "host");
                            if self.pending.len() >= MAX_PENDING {
                                self.pending.pop_front();
                            }
                            self.pending.push_back(PendingRequest {
                                method,
                                path,
                                host,
                                headers,
                                body: Body::default(),
                                started_at: Utc::now(),
                                start: Instant::now(),
                            });
                            match framing {
                                Some(framing) => self.request.mode = Mode::Body(framing),
                                // No body: the next bytes start a new request.
                                None => self.request.mode = Mode::Head,
                            }
                        }
                        None => {
                            self.request.passthrough();
                            return;
                        }
                    }
                }
                Mode::Body(mut framing) => {
                    if self.request.buffer.is_empty() {
                        self.request.mode = Mode::Body(framing);
                        return;
                    }
                    let sink = self.pending.back_mut().map(|p| &mut p.body);
                    let (consumed, done) = framing.feed(&self.request.buffer, sink);
                    self.request.buffer.drain(..consumed);
                    self.request.mode = if done {
                        Mode::Head
                    } else {
                        Mode::Body(framing)
                    };
                    if !done {
                        return;
                    }
                }
            }
        }
    }

    // ── response direction (local service → tunnel) ──────────────────────

    fn feed_response(&mut self, bytes: &[u8]) {
        if matches!(self.response.mode, Mode::Passthrough) {
            return;
        }
        self.response.buffer.extend_from_slice(bytes);

        loop {
            match std::mem::replace(&mut self.response.mode, Mode::Head) {
                Mode::Passthrough => {
                    self.response.passthrough();
                    return;
                }
                Mode::Head => {
                    let Some(end) = self.response.head_end() else {
                        if self.response.buffer.len() > MAX_HEAD {
                            self.response.passthrough();
                        }
                        return;
                    };
                    let head: Vec<u8> = self.response.buffer.drain(..end).collect();
                    let Some((status, headers)) = parse_response_head(&head) else {
                        self.response.passthrough();
                        return;
                    };

                    // Interim responses (100 Continue, 103 Early Hints) carry no
                    // body and do not complete the request they belong to.
                    if (100..200).contains(&status) && status != 101 {
                        self.response.mode = Mode::Head;
                        continue;
                    }

                    // Protocol upgrade (WebSocket, h2c): record the handshake,
                    // then stop parsing — the rest of the connection is not HTTP.
                    if status == 101 {
                        self.current_response = Some(PendingResponse {
                            status,
                            headers,
                            body: Body::default(),
                        });
                        self.complete_exchange();
                        self.request.passthrough();
                        self.response.passthrough();
                        return;
                    }

                    let method = self.pending.front().map(|p| p.method.as_str());
                    let framing = response_framing(status, method, &headers);
                    self.current_response = Some(PendingResponse {
                        status,
                        headers,
                        body: Body::default(),
                    });
                    match framing {
                        Some(framing) => self.response.mode = Mode::Body(framing),
                        None => {
                            self.complete_exchange();
                            self.response.mode = Mode::Head;
                        }
                    }
                }
                Mode::Body(mut framing) => {
                    if self.response.buffer.is_empty() {
                        self.response.mode = Mode::Body(framing);
                        return;
                    }
                    let sink = self.current_response.as_mut().map(|r| &mut r.body);
                    let (consumed, done) = framing.feed(&self.response.buffer, sink);
                    self.response.buffer.drain(..consumed);
                    if done {
                        self.complete_exchange();
                        self.response.mode = Mode::Head;
                    } else {
                        self.response.mode = Mode::Body(framing);
                        return;
                    }
                }
            }
        }
    }

    /// Pair the finished response with its request and record the exchange.
    fn complete_exchange(&mut self) {
        let Some(response) = self.current_response.take() else {
            return;
        };
        // A response with no matching request means we started tapping
        // mid-stream; record what we know rather than dropping it.
        let request = self.pending.pop_front();
        let (method, path, host, request_headers, request_body, started_at, duration_ms) =
            match request {
                Some(req) => (
                    req.method,
                    req.path,
                    req.host,
                    req.headers,
                    req.body,
                    req.started_at,
                    req.start.elapsed().as_millis() as u64,
                ),
                None => (
                    "?".to_string(),
                    "?".to_string(),
                    None,
                    Vec::new(),
                    Body::default(),
                    Utc::now(),
                    0,
                ),
            };

        self.inspector.record(Exchange {
            id: 0,
            conn_id: self.conn_id,
            tunnel: self.tunnel.clone(),
            client_addr: self.client_addr.clone(),
            method,
            path,
            host,
            status: response.status,
            request_headers,
            response_headers: response.headers,
            request_body,
            response_body: response.body,
            duration_ms,
            started_at,
            replayed: false,
        });
    }

    /// Connection closed: flush a response that was delimited by EOF.
    fn finish(&mut self) {
        if matches!(self.response.mode, Mode::Body(Framing::UntilEof)) {
            self.complete_exchange();
        }
    }
}

// ── public handle ─────────────────────────────────────────────────────────────

/// Observer handle shared by both copy directions of one proxied connection.
#[derive(Clone)]
pub struct ConnTap {
    state: Arc<Mutex<TapState>>,
}

impl ConnTap {
    pub fn new(
        inspector: Arc<Inspector>,
        tunnel: String,
        conn_id: Uuid,
        client_addr: String,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(TapState {
                inspector,
                tunnel,
                conn_id,
                client_addr,
                request: DirParser::new(),
                response: DirParser::new(),
                pending: VecDeque::new(),
                current_response: None,
            })),
        }
    }

    /// Bytes travelling from the tunnel towards the local service.
    pub fn observe_request(&self, bytes: &[u8]) {
        if let Ok(mut state) = self.state.lock() {
            state.feed_request(bytes);
        }
    }

    /// Bytes travelling from the local service back to the tunnel.
    pub fn observe_response(&self, bytes: &[u8]) {
        if let Ok(mut state) = self.state.lock() {
            state.feed_response(bytes);
        }
    }

    /// Connection closed — flush anything delimited by EOF.
    pub fn finish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.finish();
        }
    }
}

// ── head parsing ──────────────────────────────────────────────────────────────

type Headers = Vec<(String, String)>;

/// Parse a request head into `(method, path, headers)`.
fn parse_request_head(head: &[u8]) -> Option<(String, String, Headers)> {
    let mut header_buf = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut header_buf);
    match req.parse(head) {
        Ok(httparse::Status::Complete(_)) => {}
        // Incomplete cannot happen (we only parse a terminated head) and any
        // parse error means this is not HTTP/1.x.
        _ => return None,
    }
    Some((
        req.method?.to_string(),
        req.path?.to_string(),
        collect_headers(req.headers),
    ))
}

/// Parse a response head into `(status, headers)`.
fn parse_response_head(head: &[u8]) -> Option<(u16, Headers)> {
    let mut header_buf = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut res = httparse::Response::new(&mut header_buf);
    match res.parse(head) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return None,
    }
    Some((res.code?, collect_headers(res.headers)))
}

fn collect_headers(headers: &[httparse::Header<'_>]) -> Headers {
    headers
        .iter()
        .filter(|h| !h.name.is_empty())
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).into_owned(),
            )
        })
        .collect()
}

fn header_value(headers: &Headers, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn is_chunked(headers: &Headers) -> bool {
    header_value(headers, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
}

fn content_length(headers: &Headers) -> Option<u64> {
    header_value(headers, "content-length")?.trim().parse().ok()
}

/// Body framing for a request. `None` means the request has no body.
fn request_framing(headers: &Headers) -> Option<Framing> {
    if is_chunked(headers) {
        return Some(Framing::Chunked(ChunkScanner::new()));
    }
    match content_length(headers) {
        Some(0) | None => None,
        Some(n) => Some(Framing::Length(n)),
    }
}

/// Body framing for a response. `None` means the response has no body.
///
/// Per RFC 9112 §6.3 a response to HEAD, and any 204/304, never has a body even
/// when it advertises a `Content-Length`.
fn response_framing(
    status: u16,
    request_method: Option<&str>,
    headers: &Headers,
) -> Option<Framing> {
    if status == 204 || status == 304 {
        return None;
    }
    if request_method.is_some_and(|m| m.eq_ignore_ascii_case("HEAD")) {
        return None;
    }
    if is_chunked(headers) {
        return Some(Framing::Chunked(ChunkScanner::new()));
    }
    match content_length(headers) {
        Some(0) => None,
        Some(n) => Some(Framing::Length(n)),
        // No length and not chunked: the body runs until the connection closes.
        None => Some(Framing::UntilEof),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tap() -> (ConnTap, Arc<Inspector>) {
        let inspector = Inspector::new(true, "edge.test:4040".into(), None);
        let tap = ConnTap::new(
            Arc::clone(&inspector),
            "web".into(),
            Uuid::nil(),
            "203.0.113.9:44321".into(),
        );
        (tap, inspector)
    }

    #[test]
    fn simple_request_response_is_captured() {
        let (tap, inspector) = tap();
        tap.observe_request(b"GET /hello?q=1 HTTP/1.1\r\nHost: demo.test\r\n\r\n");
        tap.observe_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
        );

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 1);
        let ex = &captured[0];
        assert_eq!(ex.method, "GET");
        assert_eq!(ex.path, "/hello?q=1");
        assert_eq!(ex.status, 200);
        assert_eq!(ex.host.as_deref(), Some("demo.test"));
        assert_eq!(ex.response_body.as_text(), Some("hello"));
        assert_eq!(ex.client_addr, "203.0.113.9:44321");
        assert!(!ex.replayed);
    }

    #[test]
    fn request_body_is_captured_and_paired() {
        let (tap, inspector) = tap();
        tap.observe_request(
            b"POST /api/items HTTP/1.1\r\nHost: demo.test\r\nContent-Length: 13\r\n\r\n{\"name\":\"x\"}\n",
        );
        tap.observe_response(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok");

        let ex = &inspector.exchanges()[0];
        assert_eq!(ex.method, "POST");
        assert_eq!(ex.status, 201);
        assert_eq!(ex.request_body.as_text(), Some("{\"name\":\"x\"}\n"));
        assert_eq!(ex.response_body.as_text(), Some("ok"));
    }

    #[test]
    fn keep_alive_connection_pairs_requests_in_order() {
        let (tap, inspector) = tap();
        tap.observe_request(b"GET /one HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na");
        tap.observe_request(b"GET /two HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");

        let captured = inspector.exchanges(); // newest first
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].path, "/one");
        assert_eq!(captured[1].status, 200);
        assert_eq!(captured[0].path, "/two");
        assert_eq!(captured[0].status, 404);
    }

    #[test]
    fn messages_split_across_arbitrary_buffer_boundaries() {
        let (tap, inspector) = tap();
        let request = b"POST /split HTTP/1.1\r\nHost: d\r\nContent-Length: 10\r\n\r\n0123456789";
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody";
        // One byte at a time — the worst case for an incremental parser.
        for byte in request {
            tap.observe_request(&[*byte]);
        }
        for byte in response {
            tap.observe_response(&[*byte]);
        }

        let ex = &inspector.exchanges()[0];
        assert_eq!(ex.path, "/split");
        assert_eq!(ex.request_body.as_text(), Some("0123456789"));
        assert_eq!(ex.response_body.as_text(), Some("body"));
    }

    #[test]
    fn chunked_response_is_decoded_and_terminated() {
        let (tap, inspector) = tap();
        tap.observe_request(b"GET /stream HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        );
        // A second request/response proves the scanner found the body's end.
        tap.observe_request(b"GET /after HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].response_body.as_text(), Some("hello world"));
        assert_eq!(captured[0].path, "/after");
    }

    #[test]
    fn chunked_with_extensions_and_trailers() {
        let (tap, inspector) = tap();
        tap.observe_request(b"GET /t HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\nabcd\r\n0\r\nX-Trace: 9\r\n\r\n",
        );
        tap.observe_request(b"GET /next HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 204 No Content\r\n\r\n");

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].response_body.as_text(), Some("abcd"));
        assert_eq!(captured[0].status, 204);
    }

    #[test]
    fn head_response_has_no_body_despite_content_length() {
        let (tap, inspector) = tap();
        tap.observe_request(b"HEAD /page HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n");
        // The next response must still be parsed correctly.
        tap.observe_request(b"GET /page HTTP/1.1\r\nHost: d\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].method, "HEAD");
        assert_eq!(captured[1].response_body.total, 0);
        assert_eq!(captured[0].response_body.as_text(), Some("hi"));
    }

    #[test]
    fn interim_100_continue_does_not_consume_the_request() {
        let (tap, inspector) = tap();
        tap.observe_request(
            b"POST /upload HTTP/1.1\r\nHost: d\r\nExpect: 100-continue\r\nContent-Length: 3\r\n\r\nabc",
        );
        tap.observe_response(b"HTTP/1.1 100 Continue\r\n\r\n");
        tap.observe_response(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n");

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].status, 201);
        assert_eq!(captured[0].method, "POST");
        assert_eq!(captured[0].request_body.as_text(), Some("abc"));
    }

    #[test]
    fn websocket_upgrade_is_recorded_then_stops_parsing() {
        let (tap, inspector) = tap();
        tap.observe_request(
            b"GET /ws HTTP/1.1\r\nHost: d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        );
        tap.observe_response(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        );
        // Post-upgrade frames are opaque binary and must not be parsed as HTTP.
        tap.observe_request(&[0x81, 0x05, b'h', b'e', b'l', b'l', b'o']);
        tap.observe_response(&[0x81, 0x02, 0xff, 0xfe]);

        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].status, 101);
        assert_eq!(captured[0].path, "/ws");
    }

    #[test]
    fn non_http_traffic_falls_back_to_passthrough() {
        let (tap, inspector) = tap();
        // A TLS ClientHello — not HTTP.
        tap.observe_request(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc]);
        tap.observe_response(&[0x16, 0x03, 0x03, 0x00, 0x5a]);
        assert!(inspector.exchanges().is_empty());
    }

    #[test]
    fn eof_delimited_response_is_flushed_on_close() {
        let (tap, inspector) = tap();
        tap.observe_request(b"GET /legacy HTTP/1.0\r\n\r\n");
        tap.observe_response(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nstreamed");
        assert!(inspector.exchanges().is_empty(), "not complete until EOF");

        tap.finish();
        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].response_body.as_text(), Some("streamed"));
    }

    #[test]
    fn oversized_head_stops_parsing_instead_of_buffering() {
        let (tap, inspector) = tap();
        let giant = vec![b'x'; MAX_HEAD + 1024];
        tap.observe_request(&giant);
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        // The response still parses; the request side gave up cleanly.
        assert_eq!(inspector.exchanges().len(), 1);
        assert_eq!(inspector.exchanges()[0].method, "?");
    }

    #[test]
    fn large_body_is_truncated_but_totals_are_exact() {
        let (tap, inspector) = tap();
        let payload = vec![b'z'; super::super::BODY_CAP + 5000];
        tap.observe_request(b"GET /big HTTP/1.1\r\nHost: d\r\n\r\n");
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        );
        tap.observe_response(head.as_bytes());
        tap.observe_response(&payload);

        let ex = &inspector.exchanges()[0];
        assert_eq!(ex.response_body.total, payload.len() as u64);
        assert_eq!(ex.response_body.bytes.len(), super::super::BODY_CAP);
        assert!(ex.response_body.truncated);
    }

    /// A connection whose responses never parse must not accumulate requests
    /// forever — the queue is capped and keeps the most recent entries.
    #[test]
    fn pending_requests_are_capped_when_responses_never_arrive() {
        let (tap, inspector) = tap();
        for i in 0..(MAX_PENDING + 40) {
            tap.observe_request(format!("GET /r{i} HTTP/1.1\r\nHost: d\r\n\r\n").as_bytes());
        }

        let queued = tap.state.lock().unwrap().pending.len();
        assert_eq!(queued, MAX_PENDING);

        // The next response pairs with the oldest *retained* request.
        tap.observe_response(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let captured = inspector.exchanges();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].path, format!("/r{}", 40));
    }

    #[test]
    fn chunk_size_parsing_handles_extensions_and_junk() {
        assert_eq!(parse_chunk_size(b"1a\r\n"), Some(26));
        assert_eq!(parse_chunk_size(b"0\r\n"), Some(0));
        assert_eq!(parse_chunk_size(b"ff;name=value\r\n"), Some(255));
        assert_eq!(parse_chunk_size(b"\r\n"), None);
        assert_eq!(parse_chunk_size(b"zz\r\n"), None);
    }

    #[test]
    fn response_framing_rules() {
        let chunked = vec![("Transfer-Encoding".to_string(), "chunked".to_string())];
        let length = vec![("Content-Length".to_string(), "42".to_string())];
        assert!(matches!(
            response_framing(200, Some("GET"), &chunked),
            Some(Framing::Chunked(_))
        ));
        assert!(matches!(
            response_framing(200, Some("GET"), &length),
            Some(Framing::Length(42))
        ));
        assert!(response_framing(304, Some("GET"), &length).is_none());
        assert!(response_framing(204, Some("GET"), &[].to_vec()).is_none());
        assert!(response_framing(200, Some("head"), &length).is_none());
        assert!(matches!(
            response_framing(200, Some("GET"), &[].to_vec()),
            Some(Framing::UntilEof)
        ));
    }
}
