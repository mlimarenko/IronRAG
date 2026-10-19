//! Shared in-process SystemOne mock server for the crate's transport tests.
//!
//! One `TcpListener` implementation serves the SystemOne client transport
//! tests, the semantic-rerank dispatch tests and the graph entity-typing tests,
//! so response scripting, request capture and the retry/body-read semantics
//! have a single owner. It is compiled only into test builds and is intentionally
//! crate-internal.

use std::{
    net::Ipv4Addr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

/// One scripted HTTP response. A server serves them in order and repeats the
/// last scripted response for every further request.
#[derive(Clone)]
pub(crate) struct MockResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    pub(crate) delay: Option<Duration>,
    /// Advertised `Content-Length` when it must differ from `body.len()`. The
    /// cut-body test advertises more bytes than it writes and then closes the
    /// connection, which is a transport body-read failure, not a status failure.
    pub(crate) advertised_content_length: Option<usize>,
}

/// HTTP method, path, headers and body of one captured request.
#[derive(Clone)]
pub(crate) struct CapturedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl CapturedRequest {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// In-process mock server bound to a loopback port for the lifetime of a test.
pub(crate) struct MockServer {
    pub(crate) base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    /// Connections whose read has finished, whether or not they carried an HTTP
    /// request. It lets a test observe that a connection without a request was
    /// processed instead of racing the per-connection handler.
    connections: Arc<AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// Serves `responses` in order. Once the queue is exhausted the last
    /// response repeats; an empty queue accepts and counts connections without
    /// answering.
    pub(crate) async fn start(responses: Vec<MockResponse>) -> Self {
        Self::start_with_headers(responses, Vec::new()).await
    }

    /// Convenience for the common single `200` response.
    pub(crate) async fn start_single(body: String, delay: Option<Duration>) -> Self {
        Self::start(vec![MockResponse {
            status: 200,
            body,
            delay,
            advertised_content_length: None,
        }])
        .await
    }

    /// Starts a server that appends `extra_headers` to every response, so a test
    /// can script a header such as an oversized `Retry-After` that the response
    /// body itself cannot carry.
    pub(crate) async fn start_with_headers(
        responses: Vec<MockResponse>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.expect("bind mock server");
        let port = listener.local_addr().expect("mock server address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted_connections = Arc::clone(&connections);
        let responses = Arc::new(responses);
        let extra_headers = Arc::new(extra_headers);
        let next_response = Arc::new(AtomicUsize::new(0));
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut socket, _)) = accepted else { break };
                        let captured = Arc::clone(&captured);
                        let responses = Arc::clone(&responses);
                        let extra_headers = Arc::clone(&extra_headers);
                        let next_response = Arc::clone(&next_response);
                        let accepted_connections = Arc::clone(&accepted_connections);
                        // One task per connection so a delayed response cannot
                        // stop the listener from accepting a retry.
                        tokio::spawn(async move {
                            let request = read_http_request(&mut socket).await;
                            accepted_connections.fetch_add(1, Ordering::SeqCst);
                            if let Some(request) = request {
                                captured.lock().expect("mock request log").push(request);
                            }
                            let index = next_response.fetch_add(1, Ordering::SeqCst);
                            let scripted = responses.get(index).or_else(|| responses.last());
                            let Some(response) = scripted else { return };
                            if let Some(delay) = response.delay {
                                tokio::time::sleep(delay).await;
                            }
                            let content_length = response
                                .advertised_content_length
                                .unwrap_or(response.body.len());
                            let mut bytes = format!(
                                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                                response.status,
                                reason(response.status),
                                content_length,
                            );
                            for (name, value) in extra_headers.iter() {
                                bytes.push_str(name);
                                bytes.push_str(": ");
                                bytes.push_str(value);
                                bytes.push_str("\r\n");
                            }
                            bytes.push_str("\r\n");
                            bytes.push_str(&response.body);
                            let _ = socket.write_all(bytes.as_bytes()).await;
                            let _ = socket.shutdown().await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://{}:{port}", Ipv4Addr::LOCALHOST),
            requests,
            connections,
            shutdown: Some(shutdown),
            handle,
        }
    }

    /// Captured decision requests, in arrival order.
    pub(crate) fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("mock request log").clone()
    }

    pub(crate) fn request_count(&self) -> usize {
        self.requests.lock().expect("mock request log").len()
    }

    /// Connections whose read has finished. A retry always opens a connection,
    /// so this counts attempts even when a client abandons an attempt before the
    /// server has read its request.
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// Bounded wait until the per-connection handlers have finished reading
    /// `expected` connections, so a test can assert what a request-less
    /// connection did without racing the handler.
    pub(crate) async fn wait_for_connections(&self, expected: usize) {
        let reached = tokio::time::timeout(Duration::from_secs(2), async {
            while self.connections.load(Ordering::SeqCst) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(reached.is_ok(), "mock server did not finish reading {expected} connection(s)");
    }

    /// Bounded wait until the mock server has captured `expected` requests, so a
    /// count assertion never races the per-connection handler.
    pub(crate) async fn wait_for_requests(&self, expected: usize) {
        let reached = tokio::time::timeout(Duration::from_secs(2), async {
            while self.request_count() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(reached.is_ok(), "mock server did not receive {expected} request(s)");
    }

    pub(crate) async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.handle.await;
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        529 => "Site is overloaded",
        _ => "Status",
    }
}

/// Reads one HTTP request from a connection.
///
/// Returns `None` when the connection carried no request at all — an empty read
/// (a probe, preconnect or reset) or a fragment without a request line. Only a
/// parsed request is a decision request, so callers must not count `None`.
async fn read_http_request(socket: &mut TcpStream) -> Option<CapturedRequest> {
    let mut raw = Vec::new();
    loop {
        let mut chunk = [0_u8; 2048];
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            let content_length = parse_content_length(&raw[..header_end]);
            if raw.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    if raw.is_empty() {
        return None;
    }
    let request = parse_http_request(&raw);
    (!request.method.is_empty() && !request.path.is_empty()).then_some(request)
}

fn parse_content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default()
}

fn parse_http_request(raw: &[u8]) -> CapturedRequest {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    CapturedRequest { method, path, headers, body: body.as_bytes().to_vec() }
}
