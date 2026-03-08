//! JSON-RPC 2.0 method names, error codes, and response builders
//! for the agent protocol.

/// All methods the agent supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Status,
    Unlock,
    Lock,
    Encrypt,
    Decrypt,
    WrapDek,
    UnwrapDek,
    Shutdown,
}

impl Method {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "status" => Some(Self::Status),
            "unlock" => Some(Self::Unlock),
            "lock" => Some(Self::Lock),
            "encrypt" => Some(Self::Encrypt),
            "decrypt" => Some(Self::Decrypt),
            "wrap_dek" => Some(Self::WrapDek),
            "unwrap_dek" => Some(Self::UnwrapDek),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }
}

/// JSON-RPC 2.0 error codes used by the agent.
pub mod error_code {
    /// Standard JSON-RPC: method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Standard JSON-RPC: invalid params.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Agent is locked (not unlocked yet).
    pub const AGENT_LOCKED: i64 = -32000;
    /// Passphrase is incorrect.
    pub const WRONG_PASSPHRASE: i64 = -32001;
    /// Identity file not found.
    pub const KEY_NOT_FOUND: i64 = -32002;
    /// Encryption or decryption failed.
    pub const CRYPTO_ERROR: i64 = -32003;
    /// KEK not loaded (no vault KEK available).
    pub const KEK_NOT_LOADED: i64 = -32004;
}

/// Build a JSON-RPC 2.0 success response.
pub fn success_response(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "result": result,
        "id": id
    })
}

/// Build a JSON-RPC 2.0 error response.
pub fn error_response(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": {
            "code": code,
            "message": message
        },
        "id": id
    })
}
