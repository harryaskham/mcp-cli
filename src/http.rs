//! Streamable HTTP transport (MCP 2025-06-18) for an [`McpServer`].
//!
//! Each [`HttpServer`] owns its listener, worker threads and session table;
//! starting several (on different ports, with different principals) and
//! stopping one never affects another. Every session is bound to the principal
//! that authenticated its `initialize` and gets its own context from the
//! caller's factory, so tools see who is calling.
//!
//! What it does:
//! - `POST <path>` carries one JSON-RPC message (or a batch). Requests are
//!   answered with `application/json`; a POST of only notifications or responses
//!   is `202 Accepted`.
//! - `initialize` creates a session and returns `Mcp-Session-Id`; every later
//!   request must send it (missing: 400, unknown, expired or another principal's:
//!   404). `DELETE <path>` ends a session.
//! - `MCP-Protocol-Version`, when sent, must be a supported version (else 400).
//! - Bearer tokens map to principals (401 without a valid one, unless
//!   unauthenticated loopback use was explicitly allowed). `Origin` must be in the
//!   allowlist when present and `Host` must name the bound address (403),
//!   against DNS rebinding.
//! - Binding a non-loopback address requires `tls_terminated_upstream`: this
//!   server speaks plain HTTP and expects a TLS proxy in front.
//! - Bounded: body size (413), sessions and in-flight requests (503 with
//!   `Retry-After`), idle sessions expire.
//! - `GET` (a server-to-client stream) is not offered: 405.
//!
//! Handlers are synchronous: `notifications/cancelled` is accepted but cannot
//! interrupt a running call, so keep tool calls bounded (return a receipt for
//! long work).
use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{McpServer, SUPPORTED_PROTOCOL_VERSIONS};

/// A bearer token and the principal it authenticates.
#[derive(Clone)]
pub struct HttpToken {
    pub principal: String,
    pub token: String,
}

impl std::fmt::Debug for HttpToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpToken")
            .field("principal", &self.principal)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Listener and policy for one HTTP server.
#[derive(Clone, Debug)]
pub struct HttpServerConfig {
    /// Default `127.0.0.1:0` (an ephemeral loopback port).
    pub bind: SocketAddr,
    /// The MCP endpoint path. Default `/mcp`.
    pub path: String,
    pub tokens: Vec<HttpToken>,
    /// Accept requests without a token (as principal `anonymous`). Loopback only.
    pub allow_unauthenticated: bool,
    /// Exact `Origin` values allowed; requests without `Origin` are allowed.
    pub allowed_origins: Vec<String>,
    /// Required to bind a non-loopback address: TLS is terminated in front.
    pub tls_terminated_upstream: bool,
    pub max_body_bytes: usize,
    pub max_sessions: usize,
    pub max_in_flight: usize,
    pub session_idle: Duration,
    pub workers: usize,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            path: "/mcp".into(),
            tokens: Vec::new(),
            allow_unauthenticated: false,
            allowed_origins: Vec::new(),
            tls_terminated_upstream: false,
            max_body_bytes: 1 << 20,
            max_sessions: 64,
            max_in_flight: 32,
            session_idle: Duration::from_secs(30 * 60),
            workers: 8,
        }
    }
}

/// Why a server did not start.
#[derive(Debug, thiserror::Error)]
pub enum HttpStartError {
    #[error("refusing to listen on {0} without TLS in front (set tls_terminated_upstream)")]
    PlaintextNonLoopback(SocketAddr),
    #[error("unauthenticated access is only allowed on loopback")]
    UnauthenticatedNonLoopback,
    #[error("no bearer tokens configured")]
    NoTokens,
    #[error("cannot listen on {0}: {1}")]
    Bind(SocketAddr, String),
}

struct Session<Ctx> {
    principal: String,
    ctx: Ctx,
    protocol: String,
    last_used: Mutex<Instant>,
}

/// Builds a session's context from its authenticated principal.
type ContextFactory<Ctx> = Box<dyn Fn(&str) -> Result<Ctx, String> + Send + Sync>;

struct Shared<Ctx> {
    server: Arc<McpServer<Ctx>>,
    config: HttpServerConfig,
    hosts: Vec<String>,
    sessions: Mutex<HashMap<String, Arc<Session<Ctx>>>>,
    in_flight: AtomicUsize,
    stopping: AtomicBool,
    make_ctx: ContextFactory<Ctx>,
}

/// A running server. Dropping it shuts it down.
pub struct HttpServer<Ctx: Send + Sync + 'static> {
    listener: Arc<tiny_http::Server>,
    shared: Arc<Shared<Ctx>>,
    workers: Vec<JoinHandle<()>>,
    addr: SocketAddr,
}

impl<Ctx: Send + Sync + 'static> HttpServer<Ctx> {
    /// Starts listening. `make_ctx` builds each session's context from the
    /// authenticated principal; an error refuses that `initialize` (403).
    pub fn start<F>(
        server: Arc<McpServer<Ctx>>,
        config: HttpServerConfig,
        make_ctx: F,
    ) -> Result<Self, HttpStartError>
    where
        F: Fn(&str) -> Result<Ctx, String> + Send + Sync + 'static,
    {
        let loopback = config.bind.ip().is_loopback();
        if !loopback && !config.tls_terminated_upstream {
            return Err(HttpStartError::PlaintextNonLoopback(config.bind));
        }
        if config.allow_unauthenticated && !loopback {
            return Err(HttpStartError::UnauthenticatedNonLoopback);
        }
        if config.tokens.is_empty() && !config.allow_unauthenticated {
            return Err(HttpStartError::NoTokens);
        }
        let listener = tiny_http::Server::http(config.bind)
            .map_err(|e| HttpStartError::Bind(config.bind, e.to_string()))?;
        let addr = listener.server_addr().to_ip().unwrap_or(config.bind);
        let mut hosts = vec![
            format!("127.0.0.1:{}", addr.port()),
            format!("localhost:{}", addr.port()),
            format!("[::1]:{}", addr.port()),
        ];
        if !addr.ip().is_unspecified() {
            hosts.push(addr.to_string());
        }
        let listener = Arc::new(listener);
        let shared = Arc::new(Shared {
            server,
            hosts,
            sessions: Mutex::new(HashMap::new()),
            in_flight: AtomicUsize::new(0),
            stopping: AtomicBool::new(false),
            make_ctx: Box::new(make_ctx),
            config,
        });
        let workers = (0..shared.config.workers.max(1))
            .map(|_| {
                let listener = Arc::clone(&listener);
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    while !shared.stopping.load(Ordering::SeqCst) {
                        match listener.recv() {
                            Ok(request) => handle(&shared, request),
                            Err(_) => break,
                        }
                    }
                })
            })
            .collect();
        Ok(Self {
            listener,
            shared,
            workers,
            addr,
        })
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.shared.sessions.lock().map_or(0, |s| s.len())
    }

    /// Stops accepting, finishes the requests in progress and drops every
    /// session (and so its context). Other servers are unaffected.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        for _ in &self.workers {
            self.listener.unblock();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        if let Ok(mut sessions) = self.shared.sessions.lock() {
            sessions.clear();
        }
    }
}

impl<Ctx: Send + Sync + 'static> Drop for HttpServer<Ctx> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn header<'a>(request: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn reply(request: tiny_http::Request, status: u16, body: Option<&Value>, extra: &[(&str, String)]) {
    let text = body.map(Value::to_string).unwrap_or_default();
    let mut response = tiny_http::Response::from_string(text).with_status_code(status);
    if body.is_some() {
        response.add_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json")
                .expect("static header"),
        );
    }
    for (name, value) in extra {
        if let Ok(h) = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(h);
        }
    }
    let _ = request.respond(response);
}

fn error_body(code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": code, "message": message } })
}

/// Constant-time comparison for bearer tokens.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn principal<Ctx>(shared: &Shared<Ctx>, request: &tiny_http::Request) -> Option<String> {
    let presented = header(request, "Authorization").and_then(|v| {
        v.strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
    });
    match presented {
        Some(token) => shared
            .config
            .tokens
            .iter()
            .find(|t| same(&t.token, token.trim()))
            .map(|t| t.principal.clone()),
        None if shared.config.allow_unauthenticated => Some("anonymous".into()),
        None => None,
    }
}

fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    bytes.iter().fold(String::with_capacity(32), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn handle<Ctx: Send + Sync + 'static>(shared: &Shared<Ctx>, request: tiny_http::Request) {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    if path != shared.config.path {
        return reply(request, 404, None, &[]);
    }
    // DNS rebinding: a browser page can reach loopback but cannot choose Host or hide Origin.
    if let Some(host) = header(&request, "Host") {
        let host = host.to_ascii_lowercase();
        if !shared.hosts.contains(&host) && shared.config.bind.ip().is_loopback() {
            return reply(
                request,
                403,
                Some(&error_body(-32600, "host not allowed")),
                &[],
            );
        }
    }
    if let Some(origin) = header(&request, "Origin") {
        if !shared.config.allowed_origins.iter().any(|o| o == origin) {
            return reply(
                request,
                403,
                Some(&error_body(-32600, "origin not allowed")),
                &[],
            );
        }
    }
    let Some(principal) = principal(shared, &request) else {
        return reply(
            request,
            401,
            Some(&error_body(-32600, "authentication required")),
            &[("WWW-Authenticate", "Bearer".into())],
        );
    };
    expire(shared);
    match request.method() {
        tiny_http::Method::Post => post(shared, request, &principal),
        tiny_http::Method::Delete => {
            let id = header(&request, "Mcp-Session-Id").map(str::to_owned);
            let removed = id.and_then(|id| {
                let mut sessions = shared.sessions.lock().ok()?;
                (sessions.get(&id)?.principal == principal)
                    .then(|| sessions.remove(&id))
                    .flatten()
            });
            reply(
                request,
                if removed.is_some() { 204 } else { 404 },
                None,
                &[],
            );
        }
        _ => reply(request, 405, None, &[("Allow", "POST, DELETE".into())]),
    }
}

fn expire<Ctx>(shared: &Shared<Ctx>) {
    let idle = shared.config.session_idle;
    if let Ok(mut sessions) = shared.sessions.lock() {
        sessions.retain(|_, s| s.last_used.lock().is_ok_and(|t| t.elapsed() < idle));
    }
}

struct InFlight<'a>(&'a AtomicUsize);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn post<Ctx: Send + Sync + 'static>(
    shared: &Shared<Ctx>,
    mut request: tiny_http::Request,
    principal: &str,
) {
    if !header(&request, "Content-Type")
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"))
    {
        return reply(
            request,
            415,
            Some(&error_body(-32600, "Content-Type must be application/json")),
            &[],
        );
    }
    if let Some(accept) = header(&request, "Accept") {
        let accept = accept.to_ascii_lowercase();
        if ![
            "application/json",
            "text/event-stream",
            "*/*",
            "application/*",
        ]
        .iter()
        .any(|t| accept.contains(t))
        {
            return reply(
                request,
                406,
                Some(&error_body(-32600, "Accept must allow application/json")),
                &[],
            );
        }
    }
    let limit = shared.config.max_body_bytes;
    if request.body_length().is_some_and(|n| n > limit) {
        return reply(
            request,
            413,
            Some(&error_body(-32600, "request body too large")),
            &[],
        );
    }
    let mut body = Vec::new();
    if request
        .as_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .is_err()
    {
        return reply(
            request,
            400,
            Some(&error_body(-32700, "unreadable body")),
            &[],
        );
    }
    if body.len() > limit {
        return reply(
            request,
            413,
            Some(&error_body(-32600, "request body too large")),
            &[],
        );
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return reply(
                request,
                400,
                Some(&error_body(-32700, &format!("parse error: {error}"))),
                &[],
            );
        }
    };
    if shared.in_flight.fetch_add(1, Ordering::SeqCst) >= shared.config.max_in_flight {
        shared.in_flight.fetch_sub(1, Ordering::SeqCst);
        return reply(
            request,
            503,
            Some(&error_body(-32000, "busy")),
            &[("Retry-After", "1".into())],
        );
    }
    let _guard = InFlight(&shared.in_flight);

    let is_initialize = message.get("method").and_then(Value::as_str) == Some("initialize");
    if is_initialize {
        let count = shared.sessions.lock().map_or(usize::MAX, |s| s.len());
        if count >= shared.config.max_sessions {
            return reply(
                request,
                503,
                Some(&error_body(-32000, "too many sessions")),
                &[("Retry-After", "5".into())],
            );
        }
        let ctx = match (shared.make_ctx)(principal) {
            Ok(ctx) => ctx,
            Err(reason) => return reply(request, 403, Some(&error_body(-32600, &reason)), &[]),
        };
        let response = match shared.server.handle_request_value(&ctx, message) {
            Ok(response) => response,
            Err(error) => {
                return reply(
                    request,
                    500,
                    Some(&error_body(-32603, &error.to_string())),
                    &[],
                );
            }
        };
        let Some(response) = response else {
            return reply(request, 202, None, &[]);
        };
        let protocol = response["result"]["protocolVersion"]
            .as_str()
            .unwrap_or("2025-03-26")
            .to_owned();
        let id = new_session_id();
        if let Ok(mut sessions) = shared.sessions.lock() {
            sessions.insert(
                id.clone(),
                Arc::new(Session {
                    principal: principal.to_owned(),
                    ctx,
                    protocol,
                    last_used: Mutex::new(Instant::now()),
                }),
            );
        }
        return reply(request, 200, Some(&response), &[("Mcp-Session-Id", id)]);
    }

    let Some(id) = header(&request, "Mcp-Session-Id").map(str::to_owned) else {
        return reply(
            request,
            400,
            Some(&error_body(-32600, "Mcp-Session-Id is required")),
            &[],
        );
    };
    let session = shared
        .sessions
        .lock()
        .ok()
        .and_then(|s| s.get(&id).cloned())
        .filter(|s| s.principal == principal);
    let Some(session) = session else {
        return reply(
            request,
            404,
            Some(&error_body(-32600, "unknown session")),
            &[],
        );
    };
    if let Some(version) = header(&request, "MCP-Protocol-Version") {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) || version != session.protocol {
            return reply(
                request,
                400,
                Some(&error_body(-32600, "unsupported MCP-Protocol-Version")),
                &[],
            );
        }
    }
    if let Ok(mut last) = session.last_used.lock() {
        *last = Instant::now();
    }
    match shared.server.handle_request_value(&session.ctx, message) {
        Ok(Some(response)) => reply(request, 200, Some(&response), &[]),
        Ok(None) => reply(request, 202, None, &[]),
        Err(error) => reply(
            request,
            500,
            Some(&error_body(-32603, &error.to_string())),
            &[],
        ),
    }
}
