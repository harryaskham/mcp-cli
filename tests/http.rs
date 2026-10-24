//! Streamable HTTP conformance: sessions, headers, auth, Origin/Host, limits,
//! malformed input, and two independent servers.
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mcp_cli::http::{HttpServer, HttpServerConfig, HttpStartError, HttpToken};
use mcp_cli::{McpServer, StdioServerConfig, Tool, ToolMetadata, ToolOutput, ToolRouter};
use serde_json::{Value, json};

struct Ctx {
    principal: String,
    dropped: Arc<AtomicUsize>,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn mcp() -> Arc<McpServer<Ctx>> {
    let mut router = ToolRouter::new();
    router.add_tool(Tool::new_raw(
        ToolMetadata::new(
            "whoami",
            "The calling principal",
            json!({ "type": "object", "properties": {} }),
        ),
        |ctx: &Ctx, _| Ok(ToolOutput::new(json!({ "principal": ctx.principal }))),
    ));
    Arc::new(McpServer::new(
        StdioServerConfig {
            server_name: "test".into(),
            server_version: "1".into(),
        },
        router,
    ))
}

fn start(
    tokens: &[(&str, &str)],
    tweak: impl FnOnce(&mut HttpServerConfig),
) -> (HttpServer<Ctx>, Arc<AtomicUsize>) {
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut config = HttpServerConfig {
        tokens: tokens
            .iter()
            .map(|(p, t)| HttpToken {
                principal: (*p).into(),
                token: (*t).into(),
            })
            .collect(),
        allowed_origins: vec!["http://allowed.example".into()],
        workers: 4,
        ..HttpServerConfig::default()
    };
    tweak(&mut config);
    let counter = Arc::clone(&dropped);
    let server = HttpServer::start(mcp(), config, move |principal| {
        Ok(Ctx {
            principal: principal.into(),
            dropped: Arc::clone(&counter),
        })
    })
    .expect("starts");
    (server, dropped)
}

struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

fn send(addr: SocketAddr, method: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut stream = TcpStream::connect(addr).expect("connects");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut text = format!(
        "{method} /mcp HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("Host")) {
        let _ = write!(text, "Host: 127.0.0.1:{}\r\n", addr.port());
    }
    for (name, value) in headers {
        let _ = write!(text, "{name}: {value}\r\n");
    }
    text.push_str("\r\n");
    text.push_str(body);
    stream.write_all(text.as_bytes()).unwrap();
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_owned(),
    }
}

const JSON: (&str, &str) = ("Content-Type", "application/json");
const ACCEPT: (&str, &str) = ("Accept", "application/json, text/event-stream");

fn initialize(addr: SocketAddr, token: &str) -> String {
    let auth = format!("Bearer {token}");
    let reply = send(
        addr,
        "POST",
        &[JSON, ACCEPT, ("Authorization", &auth)],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
    );
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["result"]["protocolVersion"], "2025-06-18");
    reply
        .headers
        .get("mcp-session-id")
        .expect("session id")
        .clone()
}

fn call(addr: SocketAddr, token: &str, session: &str) -> Reply {
    let auth = format!("Bearer {token}");
    send(
        addr,
        "POST",
        &[
            JSON,
            ACCEPT,
            ("Authorization", &auth),
            ("Mcp-Session-Id", session),
            ("MCP-Protocol-Version", "2025-06-18"),
        ],
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#,
    )
}

#[test]
fn sessions_carry_their_principal() {
    let (server, _) = start(&[("alice", "token-a"), ("bob", "token-b")], |_| {});
    let addr = server.local_addr();
    let a = initialize(addr, "token-a");
    let reply = call(addr, "token-a", &a);
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.json()["result"]["structuredContent"]["data"]["principal"],
        "alice"
    );
    assert_eq!(
        call(addr, "token-b", &a).status,
        404,
        "another principal cannot use the session"
    );
    let b = initialize(addr, "token-b");
    assert_ne!(a, b);
    assert_eq!(
        call(addr, "token-b", &b).json()["result"]["structuredContent"]["data"]["principal"],
        "bob"
    );
}

#[test]
fn authentication_origin_and_host_are_checked() {
    let (server, _) = start(&[("alice", "token-a")], |_| {});
    let addr = server.local_addr();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    assert_eq!(send(addr, "POST", &[JSON, ACCEPT], body).status, 401);
    let unauthorised = send(
        addr,
        "POST",
        &[JSON, ACCEPT, ("Authorization", "Bearer nope")],
        body,
    );
    assert_eq!(unauthorised.status, 401);
    assert_eq!(
        unauthorised
            .headers
            .get("www-authenticate")
            .map(String::as_str),
        Some("Bearer")
    );
    let evil = send(
        addr,
        "POST",
        &[
            JSON,
            ACCEPT,
            ("Authorization", "Bearer token-a"),
            ("Origin", "http://evil.example"),
        ],
        body,
    );
    assert_eq!(evil.status, 403);
    let rebinding = send(
        addr,
        "POST",
        &[
            JSON,
            ACCEPT,
            ("Authorization", "Bearer token-a"),
            ("Host", "attacker.example:80"),
        ],
        body,
    );
    assert_eq!(rebinding.status, 403);
    let allowed = send(
        addr,
        "POST",
        &[
            JSON,
            ACCEPT,
            ("Authorization", "Bearer token-a"),
            ("Origin", "http://allowed.example"),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    );
    assert_eq!(allowed.status, 200);
}

#[test]
fn session_headers_are_required_and_enforced() {
    let (server, dropped) = start(&[("alice", "t")], |_| {});
    let addr = server.local_addr();
    let auth = ("Authorization", "Bearer t");
    let list = r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#;
    assert_eq!(
        send(addr, "POST", &[JSON, ACCEPT, auth], list).status,
        400,
        "no session id"
    );
    assert_eq!(
        send(
            addr,
            "POST",
            &[JSON, ACCEPT, auth, ("Mcp-Session-Id", "0123")],
            list
        )
        .status,
        404,
        "unknown session"
    );
    let id = initialize(addr, "t");
    assert_eq!(
        send(
            addr,
            "POST",
            &[
                JSON,
                ACCEPT,
                auth,
                ("Mcp-Session-Id", &id),
                ("MCP-Protocol-Version", "1999-01-01")
            ],
            list
        )
        .status,
        400
    );
    assert_eq!(
        send(
            addr,
            "POST",
            &[JSON, ACCEPT, auth, ("Mcp-Session-Id", &id)],
            list
        )
        .status,
        200,
        "version header optional"
    );
    let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let accepted = send(
        addr,
        "POST",
        &[JSON, ACCEPT, auth, ("Mcp-Session-Id", &id)],
        note,
    );
    assert_eq!(accepted.status, 202);
    assert_eq!(accepted.body, "");
    let cancel = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#;
    assert_eq!(
        send(
            addr,
            "POST",
            &[JSON, ACCEPT, auth, ("Mcp-Session-Id", &id)],
            cancel
        )
        .status,
        202
    );
    assert_eq!(
        send(addr, "DELETE", &[auth, ("Mcp-Session-Id", &id)], "").status,
        204
    );
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "ending a session drops its context"
    );
    assert_eq!(
        send(
            addr,
            "POST",
            &[JSON, ACCEPT, auth, ("Mcp-Session-Id", &id)],
            list
        )
        .status,
        404,
        "ended sessions are gone"
    );
    assert_eq!(send(addr, "GET", &[auth], "").status, 405);
}

#[test]
fn malformed_and_oversized_requests_are_refused() {
    let (server, _) = start(&[("alice", "t")], |c| c.max_body_bytes = 256);
    let addr = server.local_addr();
    let auth = ("Authorization", "Bearer t");
    let parse = send(addr, "POST", &[JSON, ACCEPT, auth], "{not json");
    assert_eq!(parse.status, 400);
    assert_eq!(parse.json()["error"]["code"], -32700);
    assert_eq!(
        send(
            addr,
            "POST",
            &[("Content-Type", "text/plain"), ACCEPT, auth],
            "{}"
        )
        .status,
        415
    );
    assert_eq!(
        send(addr, "POST", &[JSON, ("Accept", "image/png"), auth], "{}").status,
        406
    );
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(400)
    );
    assert_eq!(send(addr, "POST", &[JSON, ACCEPT, auth], &big).status, 413);
}

#[test]
fn limits_apply_backpressure() {
    let (server, _) = start(&[("alice", "t")], |c| c.max_sessions = 1);
    let addr = server.local_addr();
    initialize(addr, "t");
    let auth = ("Authorization", "Bearer t");
    let again = send(
        addr,
        "POST",
        &[JSON, ACCEPT, auth],
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    );
    assert_eq!(again.status, 503);
    assert!(again.headers.contains_key("retry-after"));
}

#[test]
fn two_servers_are_independent() {
    let (first, first_dropped) = start(&[("alice", "token-a")], |_| {});
    let (second, _) = start(&[("bob", "token-b")], |_| {});
    let (a, b) = (first.local_addr(), second.local_addr());
    assert_ne!(a.port(), b.port());
    let session_a = initialize(a, "token-a");
    let session_b = initialize(b, "token-b");
    assert_eq!(
        call(b, "token-b", &session_a).status,
        404,
        "sessions do not cross servers"
    );
    assert_eq!(
        send(
            b,
            "POST",
            &[JSON, ACCEPT, ("Authorization", "Bearer token-a")],
            "{}"
        )
        .status,
        401,
        "tokens do not cross servers"
    );
    first.shutdown();
    assert_eq!(
        first_dropped.load(Ordering::SeqCst),
        1,
        "shutdown drops the server's sessions"
    );
    assert!(
        TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_err()
            || send(a, "POST", &[JSON], "{}").status == 0,
        "the stopped server no longer answers"
    );
    assert_eq!(
        call(b, "token-b", &session_b).json()["result"]["structuredContent"]["data"]["principal"],
        "bob",
        "the other server keeps serving"
    );
}

#[test]
fn unsafe_listeners_do_not_start() {
    let config = |bind: &str| HttpServerConfig {
        bind: bind.parse().unwrap(),
        tokens: vec![HttpToken {
            principal: "p".into(),
            token: "t".into(),
        }],
        ..HttpServerConfig::default()
    };
    let make = |_: &str| -> Result<Ctx, String> { Err("unused".into()) };
    assert!(matches!(
        HttpServer::start(mcp(), config("0.0.0.0:0"), make),
        Err(HttpStartError::PlaintextNonLoopback(_))
    ));
    let open = HttpServerConfig {
        allow_unauthenticated: true,
        tls_terminated_upstream: true,
        ..config("0.0.0.0:0")
    };
    assert!(matches!(
        HttpServer::start(mcp(), open, make),
        Err(HttpStartError::UnauthenticatedNonLoopback)
    ));
    let tokenless = HttpServerConfig {
        tokens: vec![],
        ..HttpServerConfig::default()
    };
    assert!(matches!(
        HttpServer::start(mcp(), tokenless, make),
        Err(HttpStartError::NoTokens)
    ));
    let (taken, _) = start(&[("p", "t")], |_| {});
    let collision = HttpServerConfig {
        bind: taken.local_addr(),
        ..config("127.0.0.1:0")
    };
    assert!(matches!(
        HttpServer::start(mcp(), collision, make),
        Err(HttpStartError::Bind(..))
    ));
}
