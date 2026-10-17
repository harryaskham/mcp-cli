//! `demo add 2 3` / `demo divide 1 0` print a JSON envelope; `demo mcp` serves the same
//! commands as MCP tools over stdio. One implementation, two surfaces.
use mcp_cli::{ErrorCategory, McpServer, StdioServerConfig, StructuredError, ToolRouter, write_json_result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
struct Pair {
    lhs: i64,
    rhs: i64,
}

#[derive(Debug, Serialize)]
struct Answer {
    value: i64,
}

#[derive(Debug)]
enum MathError {
    DivisionByZero,
}

impl StructuredError for MathError {
    fn category(&self) -> ErrorCategory {
        ErrorCategory::Validation
    }
    fn code(&self) -> String {
        "division_by_zero".to_owned()
    }
    fn message(&self) -> String {
        "cannot divide by zero".to_owned()
    }
}

fn add(p: Pair) -> Result<Answer, MathError> {
    Ok(Answer { value: p.lhs + p.rhs })
}

fn divide(p: Pair) -> Result<Answer, MathError> {
    if p.rhs == 0 { Err(MathError::DivisionByZero) } else { Ok(Answer { value: p.lhs / p.rhs }) }
}

fn router() -> ToolRouter<()> {
    let mut r = ToolRouter::new();
    r.add_typed_tool("math_add", "Add two integers.", |(), p: Pair| add(p));
    r.add_typed_tool("math_divide", "Divide one integer by another.", |(), p: Pair| divide(p));
    r.add_typed_tool("crash", "Panic, to show how a failing tool is reported.", |(), _: Pair| -> Result<Answer, MathError> {
        panic!("this tool always panics")
    });
    r
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pair = |a: &[String]| Pair { lhs: a[0].parse().unwrap(), rhs: a[1].parse().unwrap() };
    match args.first().map(String::as_str) {
        Some("add") => write_json_result(std::io::stdout(), add(pair(&args[1..]))).unwrap(),
        Some("divide") => write_json_result(std::io::stdout(), divide(pair(&args[1..]))).unwrap(),
        Some("mcp") => {
            let config = StdioServerConfig { server_name: "demo".to_owned(), server_version: "0.1.0".to_owned() };
            McpServer::new(config, router()).serve_stdio(&()).unwrap();
        }
        _ => eprintln!("usage: demo add A B | demo divide A B | demo mcp"),
    }
}
