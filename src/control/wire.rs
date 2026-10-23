//! Versioned local wire contract. No raw tracker credentials or engine errors.
use super::TorrentCommand;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_CONNECTIONS: usize = 8;
pub const MAX_OPERATIONS: usize = 512;

// Intentionally not Debug: the token must never be included in diagnostics.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: String,
    pub token: String,
    pub instance_id: Option<String>,
    pub method: Method,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "name",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Method {
    Discover,
    List {
        offset: usize,
        limit: usize,
        revision: Option<u64>,
        after_revision: Option<u64>,
    },
    Details {
        torrent_id: String,
        offset: usize,
        limit: usize,
    },
    Journal {
        torrent_id: String,
        offset: usize,
        limit: usize,
    },
    Peers {
        torrent_id: String,
        offset: usize,
        limit: usize,
    },
    Operations {
        offset: usize,
        limit: usize,
    },
    Operation {
        operation_id: String,
    },
    Mutate {
        torrent_id: String,
        command: TorrentCommand,
        expected_revision: u64,
        idempotency_key: String,
        deadline_unix_ms: u64,
    },
    Cancel {
        operation_id: String,
    },
    Preview {
        source: String,
    },
    Add {
        source: String,
        destination: String,
        start: bool,
        idempotency_key: String,
        deadline_unix_ms: u64,
    },
    Export {
        torrent_id: String,
        destination: String,
        idempotency_key: String,
        deadline_unix_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    IncompatibleProtocol,
    Unauthorized,
    PermissionDenied,
    Disabled,
    NotRunning,
    Busy,
    InvalidPage,
    UnknownTorrent,
    Conflict,
    Unsupported,
    PersistenceUnavailable,
    EngineRejected,
    UnknownOperation,
    IdempotencyConflict,
    DeadlineExceeded,
    ApprovalRequired,
    ReconciliationRequired,
    PathDenied,
    TooLarge,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: String,
    pub instance_id: String,
    pub result: Option<Value>,
    pub error: Option<ErrorCode>,
}

impl Response {
    pub fn error(code: ErrorCode) -> Self {
        Self {
            protocol_version: super::PROTOCOL_VERSION,
            request_id: String::new(),
            instance_id: String::new(),
            result: None,
            error: Some(code),
        }
    }
    pub fn success(value: Value) -> Self {
        Self {
            result: Some(value),
            error: None,
            ..Self::error(ErrorCode::InvalidRequest)
        }
    }
}

pub fn parse_request(bytes: &[u8]) -> Result<Request, ErrorCode> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    let request: Request = serde_json::from_slice(bytes).map_err(|_| ErrorCode::InvalidRequest)?;
    if request.protocol_version != super::PROTOCOL_VERSION {
        return Err(ErrorCode::IncompatibleProtocol);
    }
    if request.request_id.is_empty() || request.request_id.len() > 64 || request.token.len() > 128 {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(request)
}

pub fn encode_response(response: &Response) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(response).unwrap_or_default();
    if bytes.len() > MAX_RESPONSE_BYTES {
        let mut error = Response::error(ErrorCode::TooLarge);
        error.request_id.clone_from(&response.request_id);
        error.instance_id.clone_from(&response.instance_id);
        bytes = serde_json::to_vec(&error).expect("fixed response");
    }
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests;
