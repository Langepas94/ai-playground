//! Tool definitions, input schemas, validation, and dispatch.
//!
//! The JSON Schemas declared here are the **single source of truth**: tool
//! registration ([`tool_defs`]) and pre-flight validation both read them, and
//! the documentation tests assert against them — so docs cannot drift from the
//! code.
//!
//! Flow for every call: validate args against the tool's schema → if invalid,
//! return an `isError` result *without* making any HTTP request → otherwise
//! build and send the request via [`crate::client::TrackerClient`].

use crate::client::{Method, TrackerClient};
use crate::error::TrackerError;
use serde_json::{json, Value};

/// A registered tool: unique name, human description, and JSON Schema for its
/// input. Mirrors the MCP `Tool` shape without depending on any transport SDK.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Unique tool name (the MCP `name`).
    pub name: &'static str,
    /// One-line human description (the MCP `description`).
    pub description: &'static str,
    /// JSON Schema for the tool input (the MCP `inputSchema`).
    pub input_schema: Value,
}

/// The MCP result of a tool call.
///
/// Carries a single text content item plus the `isError` flag. Convert to the
/// wire shape with [`CallToolOutput::to_mcp_value`].
#[derive(Debug, Clone)]
pub struct CallToolOutput {
    /// Text content: pretty JSON on success, a readable message on failure.
    pub text: String,
    /// `true` when the call failed (maps to MCP `isError: true`).
    pub is_error: bool,
}

impl CallToolOutput {
    /// Render to the MCP wire shape:
    /// `{ "content": [{ "type": "text", "text": ... }], "isError"?: true }`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tracker_mcp::CallToolOutput;
    /// let ok = CallToolOutput { text: "{}".into(), is_error: false };
    /// let v = ok.to_mcp_value();
    /// assert_eq!(v["content"][0]["type"], "text");
    /// assert!(v.get("isError").is_none());
    /// ```
    pub fn to_mcp_value(&self) -> Value {
        let mut v = json!({
            "content": [{ "type": "text", "text": self.text }]
        });
        if self.is_error {
            v["isError"] = Value::Bool(true);
        }
        v
    }
}

/// All five tool definitions, in a stable order.
///
/// # Examples
///
/// ```
/// use tracker_mcp::tool_defs;
/// let defs = tool_defs();
/// assert_eq!(defs.len(), 5);
/// assert!(defs.iter().all(|d| !d.description.is_empty()));
/// ```
pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "issue_get",
            description: "Get one Yandex Tracker issue by its key.",
            input_schema: issue_get_schema(),
        },
        ToolDef {
            name: "issue_search",
            description: "Search issues using Tracker query language, with paging.",
            input_schema: issue_search_schema(),
        },
        ToolDef {
            name: "issue_create",
            description: "Create a new issue in a queue.",
            input_schema: issue_create_schema(),
        },
        ToolDef {
            name: "issue_update",
            description: "Update fields of an existing issue.",
            input_schema: issue_update_schema(),
        },
        ToolDef {
            name: "comment_add",
            description: "Add a comment to an issue.",
            input_schema: comment_add_schema(),
        },
    ]
}

/// Look up one tool definition by name.
pub fn tool_def(name: &str) -> Option<ToolDef> {
    tool_defs().into_iter().find(|d| d.name == name)
}

fn issue_get_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["key"],
        "properties": {
            "key": {
                "type": "string",
                "description": "Issue key, e.g. TASK-42.",
                "example": "TASK-42"
            }
        }
    })
}

fn issue_search_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["query"],
        "properties": {
            "query": {
                "type": "string",
                "description": "Tracker query language expression.",
                "example": "Queue: TASK AND Status: Open"
            },
            "per_page": {
                "type": "integer",
                "description": "Results per page.",
                "minimum": 1,
                "maximum": 100,
                "default": 50,
                "example": 50
            },
            "page": {
                "type": "integer",
                "description": "1-based page number.",
                "minimum": 1,
                "default": 1,
                "example": 1
            }
        }
    })
}

fn issue_create_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["queue", "summary"],
        "properties": {
            "queue": {
                "type": "string",
                "description": "Target queue key.",
                "example": "TASK"
            },
            "summary": {
                "type": "string",
                "description": "Issue title.",
                "example": "Fix login redirect loop"
            },
            "description": {
                "type": "string",
                "description": "Issue body (optional).",
                "example": "Steps to reproduce: ..."
            },
            "assignee": {
                "type": "string",
                "description": "Assignee login or id (optional).",
                "example": "vasya"
            },
            "priority": {
                "type": "string",
                "description": "Priority key.",
                "enum": ["trivial", "minor", "normal", "critical", "blocker"],
                "default": "normal",
                "example": "normal"
            }
        }
    })
}

fn issue_update_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["key", "fields"],
        "properties": {
            "key": {
                "type": "string",
                "description": "Issue key to update.",
                "example": "TASK-42"
            },
            "fields": {
                "type": "object",
                "description": "Field name → value map applied to the issue.",
                "example": { "summary": "New title", "priority": { "key": "critical" } }
            }
        }
    })
}

fn comment_add_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["key", "text"],
        "properties": {
            "key": {
                "type": "string",
                "description": "Issue key to comment on.",
                "example": "TASK-42"
            },
            "text": {
                "type": "string",
                "description": "Comment body.",
                "example": "Deployed to staging."
            }
        }
    })
}

/// Validate `args` against a tool's JSON Schema.
///
/// Returns [`TrackerError::Validation`] with a field-level message on failure.
/// Called before any HTTP request, so bad input never reaches the network.
fn validate(schema: &Value, args: &Value) -> Result<(), TrackerError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| TrackerError::Validation(format!("internal schema error: {e}")))?;
    let errors: Vec<String> = validator
        .iter_errors(args)
        .map(|e| {
            let path = e.instance_path().to_string();
            if path.is_empty() {
                e.to_string()
            } else {
                format!("{path}: {e}")
            }
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(TrackerError::Validation(errors.join("; ")))
    }
}

/// Validate and execute a tool, returning the MCP result contract.
///
/// On success the result `text` is pretty-printed JSON from Tracker. On any
/// failure (unknown tool, invalid input, HTTP/transport error) the result has
/// `is_error: true` and a readable, **token-free** message. This function
/// never panics and never returns `Err`.
///
/// # Examples
///
/// ```no_run
/// # async fn run() {
/// use serde_json::json;
/// use tracker_mcp::{Config, TokenKind, OrgKind, TrackerClient, call_tool};
///
/// let cfg = Config::new("tkn", TokenKind::OAuth, "org", OrgKind::XOrgId);
/// let client = TrackerClient::new(cfg);
/// let out = call_tool(&client, "issue_get", &json!({ "key": "TASK-1" })).await;
/// println!("isError={} {}", out.is_error, out.text);
/// # }
/// ```
pub async fn call_tool(client: &TrackerClient, name: &str, args: &Value) -> CallToolOutput {
    match dispatch(client, name, args).await {
        Ok(value) => CallToolOutput {
            text: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            is_error: false,
        },
        Err(e) => CallToolOutput {
            text: e.to_string(),
            is_error: true,
        },
    }
}

/// Inner dispatch: validate, build the request, call HTTP, return parsed JSON.
async fn dispatch(client: &TrackerClient, name: &str, args: &Value) -> Result<Value, TrackerError> {
    let def =
        tool_def(name).ok_or_else(|| TrackerError::Validation(format!("unknown tool '{name}'")))?;
    validate(&def.input_schema, args)?;

    match name {
        "issue_get" => {
            let key = str_field(args, "key")?;
            client
                .request(Method::Get, &format!("issues/{key}"), &key, &[], None)
                .await
        }
        "issue_search" => {
            let query = str_field(args, "query")?;
            let per_page = args.get("per_page").and_then(Value::as_i64).unwrap_or(50);
            let page = args.get("page").and_then(Value::as_i64).unwrap_or(1);
            let q = [
                ("perPage", per_page.to_string()),
                ("page", page.to_string()),
            ];
            client
                .request(
                    Method::Post,
                    "issues/_search",
                    "search",
                    &q,
                    Some(&json!({ "query": query })),
                )
                .await
        }
        "issue_create" => {
            let queue = str_field(args, "queue")?;
            let mut body = json!({
                "queue": queue,
                "summary": str_field(args, "summary")?,
            });
            if let Some(d) = args.get("description").and_then(Value::as_str) {
                body["description"] = json!(d);
            }
            if let Some(a) = args.get("assignee").and_then(Value::as_str) {
                body["assignee"] = json!(a);
            }
            let priority = args
                .get("priority")
                .and_then(Value::as_str)
                .unwrap_or("normal");
            body["priority"] = json!({ "key": priority });
            client
                .request(Method::Post, "issues", &queue, &[], Some(&body))
                .await
        }
        "issue_update" => {
            let key = str_field(args, "key")?;
            let fields = args
                .get("fields")
                .cloned()
                .ok_or_else(|| TrackerError::Validation("missing 'fields'".to_string()))?;
            client
                .request(
                    Method::Patch,
                    &format!("issues/{key}"),
                    &key,
                    &[],
                    Some(&fields),
                )
                .await
        }
        "comment_add" => {
            let key = str_field(args, "key")?;
            let text = str_field(args, "text")?;
            client
                .request(
                    Method::Post,
                    &format!("issues/{key}/comments"),
                    &key,
                    &[],
                    Some(&json!({ "text": text })),
                )
                .await
        }
        other => Err(TrackerError::Validation(format!("unknown tool '{other}'"))),
    }
}

/// Read a required string field (schema validation guarantees it exists).
fn str_field(args: &Value, field: &str) -> Result<String, TrackerError> {
    args.get(field)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| TrackerError::Validation(format!("missing '{field}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_required() {
        let err = validate(&issue_get_schema(), &json!({})).unwrap_err();
        assert!(matches!(err, TrackerError::Validation(_)));
    }

    #[test]
    fn rejects_additional_properties() {
        let err = validate(&issue_get_schema(), &json!({ "key": "X", "bogus": 1 })).unwrap_err();
        assert!(matches!(err, TrackerError::Validation(_)));
    }

    #[test]
    fn rejects_out_of_range_per_page() {
        let err = validate(
            &issue_search_schema(),
            &json!({ "query": "x", "per_page": 999 }),
        )
        .unwrap_err();
        assert!(matches!(err, TrackerError::Validation(_)));
    }

    #[test]
    fn rejects_bad_priority_enum() {
        let err = validate(
            &issue_create_schema(),
            &json!({ "queue": "Q", "summary": "S", "priority": "urgent" }),
        )
        .unwrap_err();
        assert!(matches!(err, TrackerError::Validation(_)));
    }

    #[test]
    fn accepts_valid_input() {
        validate(&issue_get_schema(), &json!({ "key": "TASK-1" })).unwrap();
        validate(
            &issue_search_schema(),
            &json!({ "query": "Queue: X", "per_page": 10, "page": 2 }),
        )
        .unwrap();
    }

    #[test]
    fn all_schemas_well_formed() {
        for d in tool_defs() {
            assert_eq!(d.input_schema["type"], "object");
            assert_eq!(d.input_schema["additionalProperties"], false);
            assert!(d.input_schema["required"].is_array());
            // jsonschema must accept the schema itself.
            jsonschema::validator_for(&d.input_schema).unwrap();
        }
    }
}
