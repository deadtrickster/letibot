//! JSON-RPC 2.0 over stdio, which is ACP's only transport
//! ([Transports](https://agentclientprotocol.com/protocol/v1/transports)): one message per line,
//! nothing on stdout that is not a message, and stderr for logging.
//!
//! Read tolerantly in the direction this adapter is a server: an unknown field is ignored, and a
//! line that is not a message is answered with a parse error carrying `id: null` rather than
//! ending the connection, because a client with a newer idea of the protocol is still a client.

use serde_json::{Map, Value, json};

/// What arrived on stdin.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Something to answer: `initialize`, `session/new`, `session/prompt`, or a response to a
    /// request this adapter sent.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// Something not to answer: `session/cancel`, and anything a newer client adds.
    Notification { method: String, params: Value },
    /// The client's answer to one of this adapter's requests - the permission one, today.
    Response { id: Value, result: Value },
    /// The client refused a request this adapter sent.
    Failure { id: Value, message: String },
}

/// Why a line was not a message. Kept as the sentence a person reads, because the only caller
/// puts it in an error response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    pub detail: String,
    /// The id the line named, when it could be read: a request that fails to parse is answered
    /// with its own id if there is one, and `null` when there is not.
    pub id: Option<Value>,
}

/// One line, as the message it is.
pub fn parse(line: &str) -> Result<Incoming, Malformed> {
    let value: Value = serde_json::from_str(line).map_err(|e| Malformed {
        detail: e.to_string(),
        id: None,
    })?;
    let object = value.as_object().ok_or_else(|| Malformed {
        detail: "a message must be a JSON object".into(),
        id: None,
    })?;
    let id = object.get("id").cloned();
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        return Ok(match id {
            Some(id) => Incoming::Request {
                id,
                method: method.to_string(),
                params,
            },
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        });
    }
    let Some(id) = id else {
        return Err(Malformed {
            detail: "a message with neither a method nor an id".into(),
            id: None,
        });
    };
    match object.get("error") {
        Some(error) => Ok(Incoming::Failure {
            id,
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the client refused the request")
                .to_string(),
        }),
        None => Ok(Incoming::Response {
            id,
            result: object.get("result").cloned().unwrap_or(Value::Null),
        }),
    }
}

/// A request of this adapter's own.
pub fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

pub fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub fn error(id: Value, code: i32, message: &str) -> Value {
    let mut error = Map::new();
    error.insert("code".into(), json!(code));
    error.insert("message".into(), json!(message));
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request and a notification differ by one field, and both arrive on the same pipe.
    #[test]
    fn a_request_is_answered_and_a_notification_is_not() {
        let request = parse(r#"{"jsonrpc":"2.0","id":7,"method":"session/prompt","params":{}}"#)
            .expect("a request");
        match request {
            Incoming::Request { id, method, .. } => {
                assert_eq!(id, json!(7));
                assert_eq!(method, "session/prompt");
            }
            other => panic!("expected a request, got {other:?}"),
        }
        let notification = parse(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{}}"#)
            .expect("a notification");
        assert!(matches!(notification, Incoming::Notification { .. }));
    }

    /// The client's answer to the adapter's own permission request, and its refusal of one.
    #[test]
    fn a_response_is_told_from_a_failure() {
        let ok =
            parse(r#"{"jsonrpc":"2.0","id":"a-1","result":{"outcome":{"outcome":"cancelled"}}}"#)
                .expect("a response");
        assert!(matches!(ok, Incoming::Response { .. }));
        let refused =
            parse(r#"{"jsonrpc":"2.0","id":"a-1","error":{"code":-32601,"message":"no"}}"#)
                .expect("an error");
        match refused {
            Incoming::Failure { message, .. } => assert_eq!(message, "no"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// A line that is not JSON is a parse error with no id, and not the end of the connection.
    #[test]
    fn a_broken_line_is_reported_rather_than_fatal() {
        let malformed = parse("{not json").expect_err("not a message");
        assert_eq!(malformed.id, None);
        assert!(!malformed.detail.is_empty());
        assert_eq!(
            error(Value::Null, -32700, &malformed.detail),
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": malformed.detail}})
        );
    }
}
