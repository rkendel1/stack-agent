//! A minimal client for cli2's local protocol (version 1): one JSON object per line over a Unix
//! socket. It speaks the protocol and nothing else: no planning, no approval policy, no storage.
//!
//! This is deliberately not a link-time dependency on cli2. The protocol is cli2's versioned,
//! schema'd contract; speaking it keeps Chip free of cli2's crates and of its toolchain, and lets
//! the two be released independently. The client fails closed: it refuses a server that does not
//! speak version 1 or that lacks an operation the backend needs.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// Operations the backend needs from the server.
pub const REQUIRED_OPERATIONS: &[&str] = &[
    "project.scan",
    "capabilities.list",
    "intent.plan",
    "plan.get",
    "plan.approve",
    "plan.reject",
    "execution.start",
    "execution.get",
    "execution.cancel",
    "evidence.read",
];

/// The largest response line this client reads.
const MAX_LINE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The connection failed before the request was sent, or the server cannot be used (wrong
    /// version, missing operation). Nothing was asked of the server.
    Unavailable(String),
    /// The request may have reached the server and no usable answer came back. Whatever it asked
    /// for may have happened.
    Lost(String),
    /// The server answered with an error. `code` is its protocol error code.
    Server { code: String, message: String },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "cli2 is unavailable: {m}"),
            Self::Lost(m) => write!(f, "the answer from cli2 was lost: {m}"),
            Self::Server { code, message } => write!(f, "cli2 refused ({code}): {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next: u64,
    hello: Value,
}

impl Client {
    /// Connect, agree on protocol version 1 and check the server has what the backend needs.
    pub fn connect(socket: &Path, io_timeout: Duration) -> Result<Client, ClientError> {
        let stream = UnixStream::connect(socket).map_err(|e| {
            ClientError::Unavailable(format!("cannot connect to {}: {e}", socket.display()))
        })?;
        stream
            .set_read_timeout(Some(io_timeout))
            .and_then(|_| stream.set_write_timeout(Some(io_timeout)))
            .map_err(|e| ClientError::Unavailable(e.to_string()))?;
        let writer = stream
            .try_clone()
            .map_err(|e| ClientError::Unavailable(e.to_string()))?;
        let mut client = Client {
            reader: BufReader::new(stream),
            writer,
            next: 0,
            hello: Value::Null,
        };
        let hello = client
            .call(
                "hello",
                json!({"client": {"name": "stack-agent", "version": "chip-cli2"}, "versions": [PROTOCOL_VERSION]}),
            )
            .map_err(|e| match e {
                ClientError::Lost(m) => ClientError::Unavailable(format!("hello failed: {m}")),
                other => other,
            })?;
        if hello["protocol_version"].as_u64() != Some(PROTOCOL_VERSION) {
            return Err(ClientError::Unavailable(format!(
                "the server speaks protocol {}, not {PROTOCOL_VERSION}",
                hello["protocol_version"]
            )));
        }
        let have: Vec<&str> = hello["operations"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if let Some(missing) = REQUIRED_OPERATIONS.iter().find(|op| !have.contains(op)) {
            return Err(ClientError::Unavailable(format!(
                "the server does not offer {missing}"
            )));
        }
        client.hello = hello;
        Ok(client)
    }

    /// The server's `hello` answer (its instance id, operations and limits).
    pub fn hello(&self) -> &Value {
        &self.hello
    }

    /// One request and its answer. A failure to *send* is [`ClientError::Unavailable`]; once the
    /// request may have left, any failure to read a usable answer is [`ClientError::Lost`].
    pub fn call(&mut self, op: &str, params: Value) -> Result<Value, ClientError> {
        self.next += 1;
        let id = format!("c{}", self.next);
        let line = json!({"id": id, "op": op, "params": params}).to_string();
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .and_then(|_| self.writer.flush())
            .map_err(|e| ClientError::Lost(format!("sending {op}: {e}")))?;
        let mut buf = String::new();
        let n = (&mut self.reader)
            .take(MAX_LINE)
            .read_line(&mut buf)
            .map_err(|e| ClientError::Lost(format!("reading the answer to {op}: {e}")))?;
        if n == 0 || !buf.ends_with('\n') {
            return Err(ClientError::Lost(format!(
                "the connection ended before a complete answer to {op}"
            )));
        }
        let resp: Value = serde_json::from_str(&buf)
            .map_err(|e| ClientError::Lost(format!("the answer to {op} is not JSON: {e}")))?;
        if resp["id"] != id {
            return Err(ClientError::Lost(format!(
                "the answer to {op} belongs to another request"
            )));
        }
        if resp["ok"] == true {
            Ok(resp["result"].clone())
        } else {
            Err(ClientError::Server {
                code: resp["error"]["code"].as_str().unwrap_or("unknown").into(),
                message: resp["error"]["message"].as_str().unwrap_or("").into(),
            })
        }
    }
}
