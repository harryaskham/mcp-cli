//! Rich content, annotations, raw tools and dynamic catalogs, exercised only
//! through the public API.

use std::sync::OnceLock;

use mcp_cli::{
    ContentBlock, ErrorCategory, JsonError, McpServer, SharedToolRouter, StdioServerConfig,
    StructuredError, Tool, ToolAnnotations, ToolMetadata, ToolOutput, ToolRouter, base64_encode,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
struct Shot {
    width: u32,
}

#[derive(Debug, Serialize)]
struct ShotInfo {
    width: u32,
    artifact: String,
}

#[derive(Debug)]
struct Failure(&'static str);

impl StructuredError for Failure {
    fn category(&self) -> ErrorCategory {
        ErrorCategory::ExecutionFailure
    }
    fn code(&self) -> String {
        "capture_failed".to_owned()
    }
    fn message(&self) -> String {
        self.0.to_owned()
    }
}

fn config() -> StdioServerConfig {
    StdioServerConfig {
        server_name: "rich".to_owned(),
        server_version: "0.0.1".to_owned(),
    }
}

fn frames(requests: &[Value]) -> Vec<u8> {
    requests
        .iter()
        .flat_map(|r| {
            let mut line = serde_json::to_vec(r).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

fn responses(output: &[u8]) -> Vec<Value> {
    std::str::from_utf8(output)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: u64, name: &str, arguments: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
}

const PIXELS: &[u8] = b"\x89PNG\r\n\x1a\nnot-really-an-image";

fn capture_router() -> ToolRouter<()> {
    let mut router = ToolRouter::new();
    router.add_tool(
        Tool::new_rich("capture", "Capture an image.", |(), input: Shot| {
            if input.width == 0 {
                return Err(Failure("nothing to capture"));
            }
            Ok(ToolOutput::new(ShotInfo {
                width: input.width,
                artifact: "a-1".to_owned(),
            })
            .with_content(ContentBlock::image(PIXELS, "image/png"))
            .with_content(ContentBlock::resource_link("artifact://a-1", "a-1.png")))
        })
        .with_title("Capture")
        .with_annotations(
            ToolAnnotations::read_only()
                .with_title("Capture")
                .with_open_world(false),
        ),
    );
    router.add_typed_tool("plain", "No annotations.", |(), input: Shot| {
        Ok::<_, Failure>(input.width)
    });
    router
}

#[test]
fn annotations_and_titles_are_advertised_only_when_set() {
    let server = McpServer::new(config(), capture_router());
    let listing = serde_json::to_value(server.tool_metadata()).unwrap();
    assert_eq!(listing[0]["title"], "Capture");
    assert_eq!(
        listing[0]["annotations"],
        json!({ "title": "Capture", "readOnlyHint": true, "openWorldHint": false })
    );
    let plain = listing[1].as_object().unwrap();
    assert!(
        !plain.contains_key("title") && !plain.contains_key("annotations"),
        "unset metadata stays absent: {plain:?}"
    );
    assert_eq!(
        serde_json::to_value(ToolAnnotations::mutating(true, false)).unwrap(),
        json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false })
    );
}

#[test]
fn rich_content_follows_the_envelope_and_never_enters_it() {
    let server = McpServer::new(config(), capture_router());
    let mut output = Vec::new();
    server
        .serve_transport(
            &(),
            std::io::Cursor::new(frames(&[call(1, "capture", &json!({ "width": 640 }))])),
            &mut output,
        )
        .unwrap();
    let result = &responses(&output)[0]["result"];
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(
        content[0]["type"], "text",
        "the envelope text block stays first"
    );
    let envelope: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(envelope, result["structuredContent"]);
    assert_eq!(envelope["data"], json!({ "width": 640, "artifact": "a-1" }));
    let encoded = base64_encode(PIXELS);
    assert_eq!(
        content[1],
        json!({ "type": "image", "data": encoded, "mimeType": "image/png" })
    );
    assert_eq!(
        content[2],
        json!({ "type": "resource_link", "uri": "artifact://a-1", "name": "a-1.png" })
    );
    assert!(
        !content[0]["text"].as_str().unwrap().contains(&encoded),
        "image bytes must not be in the text block"
    );
    assert!(
        !result["structuredContent"].to_string().contains(&encoded),
        "image bytes must not be in structuredContent"
    );
    assert_eq!(result["isError"], false);
}

#[test]
fn failures_carry_no_rich_content() {
    let server = McpServer::new(config(), capture_router());
    let mut output = Vec::new();
    server
        .serve_transport(
            &(),
            std::io::Cursor::new(frames(&[call(1, "capture", &json!({ "width": 0 }))])),
            &mut output,
        )
        .unwrap();
    let result = &responses(&output)[0]["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["structuredContent"]["error"]["code"],
        "capture_failed"
    );
}

#[test]
fn embedded_resources_and_audio_serialize_per_spec() {
    assert_eq!(
        serde_json::to_value(ContentBlock::text_resource(
            "file:///a.md",
            Some("text/markdown".into()),
            "# A"
        ))
        .unwrap(),
        json!({ "type": "resource", "resource": { "uri": "file:///a.md", "mimeType": "text/markdown", "text": "# A" } })
    );
    assert_eq!(
        serde_json::to_value(ContentBlock::blob_resource("blob:x", None, b"ab")).unwrap(),
        json!({ "type": "resource", "resource": { "uri": "blob:x", "blob": "YWI=" } })
    );
    assert_eq!(
        serde_json::to_value(ContentBlock::audio(b"a", "audio/wav")).unwrap(),
        json!({ "type": "audio", "data": "YQ==", "mimeType": "audio/wav" })
    );
    let round: ContentBlock =
        serde_json::from_value(json!({ "type": "image", "data": "YQ==", "mimeType": "image/png" }))
            .unwrap();
    assert_eq!(round, ContentBlock::image(b"a", "image/png"));
    assert_eq!(ContentBlock::image(b"abc", "image/png").payload_len(), 4);
}

#[test]
fn base64_matches_rfc_4648_vectors() {
    for (input, expected) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_encode(input.as_bytes()), expected);
    }
    assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
}

fn raw_schema() -> Value {
    json!({ "type": "object", "properties": { "x": { "type": "integer" } }, "required": ["x"], "additionalProperties": false })
}

#[test]
fn raw_tools_advertise_their_schema_verbatim_and_receive_raw_arguments() {
    let mut router = ToolRouter::new();
    let metadata = ToolMetadata::new("raw_echo", "Echo arguments.", raw_schema());
    router.add_tool(
        Tool::new_raw(metadata, |(), arguments: Value| {
            if arguments.get("x").and_then(Value::as_i64).is_none() {
                return Err(JsonError::new(
                    ErrorCategory::Validation,
                    "invalid_input",
                    "x must be an integer".to_owned(),
                )
                .with_details(json!({ "field": "x" })));
            }
            Ok(ToolOutput::new(json!({ "echo": arguments })))
        })
        .with_annotations(ToolAnnotations::mutating(false, true)),
    );
    let server = McpServer::new(config(), router);
    assert_eq!(server.tool_metadata()[0].input_schema, raw_schema());
    let mut output = Vec::new();
    let requests = frames(&[
        call(1, "raw_echo", &json!({ "x": 3, "extra": true })),
        call(2, "raw_echo", &json!({ "x": "three" })),
    ]);
    server
        .serve_transport(&(), std::io::Cursor::new(requests), &mut output)
        .unwrap();
    let replies = responses(&output);
    assert_eq!(
        replies[0]["result"]["structuredContent"]["data"],
        json!({ "echo": { "x": 3, "extra": true } }),
        "raw arguments are passed through"
    );
    assert_eq!(replies[1]["result"]["isError"], true);
    assert_eq!(
        replies[1]["result"]["structuredContent"]["error"]["code"],
        "invalid_input"
    );
    assert_eq!(
        replies[1]["result"]["structuredContent"]["error"]["details"],
        json!({ "field": "x" })
    );
}

#[test]
fn raw_tools_with_non_object_schemas_or_duplicate_names_are_refused() {
    let mut router = ToolRouter::<()>::new();
    let error = router
        .try_add_tool(Tool::new_raw(
            ToolMetadata::new("bad", "Bad.", json!({ "type": "string" })),
            |(), _| Ok(ToolOutput::new(Value::Null)),
        ))
        .unwrap_err();
    assert_eq!(error.code, "invalid_input_schema");
    router
        .try_add_tool(Tool::new_raw(
            ToolMetadata::new("one", "One.", raw_schema()),
            |(), _| Ok(ToolOutput::new(Value::Null)),
        ))
        .unwrap();
    let error = router
        .try_add_tool(Tool::new_raw(
            ToolMetadata::new("one", "Again.", raw_schema()),
            |(), _| Ok(ToolOutput::new(Value::Null)),
        ))
        .unwrap_err();
    assert_eq!(error.code, "duplicate_tool_name");
}

#[test]
fn removed_tools_become_unknown() {
    let mut router = capture_router();
    assert!(router.remove_tool("plain"));
    assert!(!router.remove_tool("plain"));
    assert!(router.tool("plain").is_none() && router.tool("capture").is_some());
    let envelope =
        serde_json::to_value(router.call_tool(&(), "plain", json!({ "width": 1 }))).unwrap();
    assert_eq!(envelope["error"]["code"], "unknown_tool");
}

/// A context whose tools can change the catalog they belong to.
struct App {
    tools: OnceLock<SharedToolRouter<App>>,
}

fn dynamic_app() -> (App, SharedToolRouter<App>) {
    let mut router = ToolRouter::new();
    router.add_tool(Tool::new_raw(
        ToolMetadata::new("enable", "Add a tool.", json!({ "type": "object" })),
        |app: &App, _| {
            // Runs while the server is mid-call: must not deadlock on the catalog.
            app.tools.get().unwrap().update(|tools| {
                tools.try_add_tool(Tool::new_raw(
                    ToolMetadata::new("extra", "Added at runtime.", json!({ "type": "object" })),
                    |_, _| Ok(ToolOutput::new(json!("extra ran"))),
                ))
            })?;
            Ok(ToolOutput::new(json!({ "enabled": "extra" })))
        },
    ));
    let shared = SharedToolRouter::new(router);
    let app = App {
        tools: OnceLock::new(),
    };
    assert!(app.tools.set(shared.clone()).is_ok());
    (app, shared)
}

#[test]
fn shared_catalogs_announce_changes_after_the_client_has_listed() {
    let (app, shared) = dynamic_app();
    let server = McpServer::with_shared_router(config(), shared.clone());
    let list = |id: u64| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" });
    let requests = frames(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        list(2),
        call(3, "enable", &json!({})),
        list(4),
        call(5, "extra", &json!({})),
        json!({ "jsonrpc": "2.0", "id": 6, "method": "ping" }),
    ]);
    let mut output = Vec::new();
    server
        .serve_transport(&app, std::io::Cursor::new(requests), &mut output)
        .unwrap();
    let lines = responses(&output);
    let methods: Vec<String> = lines
        .iter()
        .map(|l| {
            l.get("method")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{}", l["id"]), str::to_owned)
        })
        .collect();
    assert_eq!(
        methods,
        [
            "#1",
            "#2",
            "#3",
            "notifications/tools/list_changed",
            "#4",
            "#5",
            "#6"
        ],
        "exactly one notice, right after the change"
    );
    assert_eq!(
        lines[0]["result"]["capabilities"]["tools"]["listChanged"],
        true
    );
    assert_eq!(lines[1]["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(lines[4]["result"]["tools"].as_array().unwrap().len(), 2);
    assert_eq!(lines[5]["result"]["structuredContent"]["data"], "extra ran");
    assert_eq!(shared.generation(), 1);
}

#[test]
fn no_notice_before_initialize_and_owned_routers_never_announce() {
    let (app, shared) = dynamic_app();
    shared.update(|tools| tools.remove_tool("enable"));
    let server = McpServer::with_shared_router(config(), shared.clone());
    let mut output = Vec::new();
    server
        .serve_transport(
            &app,
            std::io::Cursor::new(frames(&[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
            ])),
            &mut output,
        )
        .unwrap();
    assert!(
        responses(&output).iter().all(|l| l.get("method").is_none()),
        "uninitialised sessions get no notices"
    );

    let owned = McpServer::new(config(), capture_router());
    let mut output = Vec::new();
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
    owned
        .serve_transport(
            &(),
            std::io::Cursor::new(frames(&[init, call(2, "plain", &json!({ "width": 1 }))])),
            &mut output,
        )
        .unwrap();
    let lines = responses(&output);
    assert_eq!(
        lines[0]["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert!(lines.iter().all(|l| l.get("method").is_none()));
}
