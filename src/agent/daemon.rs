//! Agent daemon: listens on a Unix socket and manages lifecycle.
//!
//! The daemon is started by `blu __agent-daemon` (an internal
//! subcommand). It:
//!
//! 1. Cleans up any stale socket/PID files
//! 2. Binds a Unix domain socket at `~/.blu/agent.sock`
//! 3. Sets socket permissions to 0600 (owner-only)
//! 4. Writes its PID to `~/.blu/agent.pid`
//! 5. Accepts connections and dispatches JSON-RPC requests
//! 6. On shutdown: zeroizes secrets, removes socket and PID file

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::agent::config::AgentConfig;
use crate::agent::paths::AgentPaths;
use crate::agent::protocol::{self, Method};
use crate::agent::state::AgentState;
use crate::error::{BluError, Result};

/// Run the agent daemon. This function does not return under normal
/// operation; it runs until it receives a shutdown request or signal.
pub fn run_daemon(paths: &AgentPaths) -> Result<()> {
    // Refuse to run as root
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Err(BluError::Internal("refusing to run agent as root".into()));
    }

    // Clean up stale files from a previous run
    cleanup_stale(paths);

    // Bind the Unix socket
    let listener = UnixListener::bind(&paths.socket).map_err(|e| {
        BluError::Internal(format!(
            "failed to bind agent socket at {}: {}",
            paths.socket.display(),
            e
        ))
    })?;

    // Set socket permissions to 0600 (owner read/write only)
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;

    // Write PID file
    let pid = process::id();
    paths.write_pid(pid)?;

    info!(
        "agent started (pid {}), listening on {}",
        pid,
        paths.socket.display()
    );

    // Set up signal handling for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_signal = running.clone();

    ctrlc_handler(running_signal);

    // Load agent configuration (timeout profiles, preferences)
    let agent_config = match AgentConfig::load() {
        Ok(cfg) => {
            info!(
                "agent config: profile={}, idle={:?}, max={:?}",
                cfg.profile, cfg.timeout_idle, cfg.timeout_max
            );
            cfg
        }
        Err(e) => {
            warn!("failed to load agent config, using defaults: {}", e);
            AgentConfig::default()
        }
    };

    // Agent state (holds decrypted keys when unlocked)
    let mut state = AgentState::with_config(agent_config);

    // Accept loop
    while running.load(Ordering::SeqCst) {
        // Check timeouts on each poll iteration
        state.check_timeouts();

        listener
            .set_nonblocking(true)
            .map_err(|e| BluError::Internal(format!("set_nonblocking: {}", e)))?;

        match listener.accept() {
            Ok((mut stream, _addr)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|e| BluError::Internal(format!("stream config: {}", e)))?;

                match handle_connection(&mut stream, &mut state, &running) {
                    Ok(()) => {
                        // Record activity to reset the idle timer
                        state.touch();
                    }
                    Err(e) => {
                        warn!("error handling connection: {}", e);
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                warn!("accept error: {}", e);
            }
        }
    }

    info!("agent shutting down");
    state.lock();
    paths.cleanup();
    Ok(())
}

/// Handle a single client connection: read request, dispatch, write response.
fn handle_connection(
    stream: &mut std::os::unix::net::UnixStream,
    state: &mut AgentState,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Read 4-byte big-endian length prefix
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|e| BluError::Internal(format!("read length: {}", e)))?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    if msg_len > 64 * 1024 * 1024 {
        return Err(BluError::Internal("message too large".into()));
    }

    let mut payload = vec![0u8; msg_len];
    stream
        .read_exact(&mut payload)
        .map_err(|e| BluError::Internal(format!("read payload: {}", e)))?;

    let request: serde_json::Value = serde_json::from_slice(&payload)?;

    let method_str = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let response = match Method::from_str(method_str) {
        Some(Method::Status) => handle_status(state, &id),
        Some(Method::Unlock) => handle_unlock(state, &id, &params),
        Some(Method::Lock) => handle_lock(state, &id),
        Some(Method::Encrypt) => handle_encrypt(state, &id, &params),
        Some(Method::Decrypt) => handle_decrypt(state, &id, &params),
        Some(Method::WrapDek) => handle_wrap_dek(state, &id, &params),
        Some(Method::UnwrapDek) => handle_unwrap_dek(state, &id, &params),
        Some(Method::Shutdown) => {
            running.store(false, Ordering::SeqCst);
            protocol::success_response(&id, serde_json::json!({}))
        }
        None => protocol::error_response(
            &id,
            protocol::error_code::METHOD_NOT_FOUND,
            "method not found",
        ),
    };

    write_response(stream, &response)?;
    Ok(())
}

fn handle_status(state: &AgentState, id: &serde_json::Value) -> serde_json::Value {
    let remaining = state.time_remaining().map(format_duration);
    let profile = state.profile().profile.to_string();

    protocol::success_response(
        id,
        serde_json::json!({
            "unlocked": state.is_unlocked(),
            "public_key": state.public_key(),
            "profile": profile,
            "timeout_remaining": remaining,
            "has_kek": state.has_kek(),
            "kek_version": state.kek_version(),
        }),
    )
}

/// Format a Duration as a human-readable string (e.g. "59m 42s").
fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}h {:02}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m {:02}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

fn handle_unlock(
    state: &mut AgentState,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> serde_json::Value {
    let identity_path = match params.get("identity_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                "missing identity_path",
            );
        }
    };

    let passphrase = match params.get("passphrase").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                "missing passphrase",
            );
        }
    };

    match state.unlock(identity_path, passphrase) {
        Ok(public_key) => protocol::success_response(
            id,
            serde_json::json!({
                "public_key": public_key,
            }),
        ),
        Err(BluError::WrongPassphrase) => protocol::error_response(
            id,
            protocol::error_code::WRONG_PASSPHRASE,
            "incorrect passphrase",
        ),
        Err(BluError::KeyFileNotFound { path }) => protocol::error_response(
            id,
            protocol::error_code::KEY_NOT_FOUND,
            &format!("key file not found: {}", path.display()),
        ),
        Err(e) => protocol::error_response(
            id,
            protocol::error_code::CRYPTO_ERROR,
            &format!("unlock failed: {}", e),
        ),
    }
}

fn handle_lock(state: &mut AgentState, id: &serde_json::Value) -> serde_json::Value {
    state.lock();
    protocol::success_response(id, serde_json::json!({}))
}

fn handle_encrypt(
    state: &AgentState,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> serde_json::Value {
    if !state.is_unlocked() {
        return protocol::error_response(id, protocol::error_code::AGENT_LOCKED, "agent is locked");
    }

    let data_b64 = match params.get("data").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                "missing data",
            );
        }
    };

    let data = match BASE64.decode(data_b64) {
        Ok(d) => d,
        Err(e) => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                &format!("invalid base64 data: {}", e),
            );
        }
    };

    match state.encrypt(&data) {
        Ok(ciphertext) => protocol::success_response(
            id,
            serde_json::json!({
                "ciphertext": BASE64.encode(&ciphertext),
            }),
        ),
        Err(e) => protocol::error_response(
            id,
            protocol::error_code::CRYPTO_ERROR,
            &format!("encryption failed: {}", e),
        ),
    }
}

fn handle_decrypt(
    state: &AgentState,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> serde_json::Value {
    if !state.is_unlocked() {
        return protocol::error_response(id, protocol::error_code::AGENT_LOCKED, "agent is locked");
    }

    let data_b64 = match params.get("data").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                "missing data",
            );
        }
    };

    let data = match BASE64.decode(data_b64) {
        Ok(d) => d,
        Err(e) => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                &format!("invalid base64 data: {}", e),
            );
        }
    };

    match state.decrypt(&data) {
        Ok(plaintext) => protocol::success_response(
            id,
            serde_json::json!({
                "plaintext": BASE64.encode(&plaintext),
            }),
        ),
        Err(e) => protocol::error_response(
            id,
            protocol::error_code::CRYPTO_ERROR,
            &format!("decryption failed: {}", e),
        ),
    }
}

fn handle_wrap_dek(
    state: &mut AgentState,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> serde_json::Value {
    if !state.is_unlocked() {
        return protocol::error_response(id, protocol::error_code::AGENT_LOCKED, "agent is locked");
    }

    // If a kek_dir is provided and no KEK is loaded yet, load it
    if !state.has_kek() {
        if let Some(kek_dir) = params.get("kek_dir").and_then(|v| v.as_str()) {
            if let Err(e) = state.load_kek(kek_dir) {
                return protocol::error_response(
                    id,
                    protocol::error_code::CRYPTO_ERROR,
                    &format!("failed to load KEK: {}", e),
                );
            }
        } else {
            return protocol::error_response(
                id,
                protocol::error_code::KEK_NOT_LOADED,
                "no KEK loaded (provide kek_dir to load)",
            );
        }
    }

    match state.wrap_dek() {
        Ok((dek_bytes, wrapped_dek, kek_version)) => protocol::success_response(
            id,
            serde_json::json!({
                "dek": BASE64.encode(&dek_bytes),
                "wrapped_dek": BASE64.encode(&wrapped_dek),
                "kek_version": kek_version,
            }),
        ),
        Err(e) => protocol::error_response(
            id,
            protocol::error_code::CRYPTO_ERROR,
            &format!("wrap_dek failed: {}", e),
        ),
    }
}

fn handle_unwrap_dek(
    state: &mut AgentState,
    id: &serde_json::Value,
    params: &serde_json::Value,
) -> serde_json::Value {
    if !state.is_unlocked() {
        return protocol::error_response(id, protocol::error_code::AGENT_LOCKED, "agent is locked");
    }

    // If a kek_dir is provided and no KEK is loaded yet, load it
    if !state.has_kek() {
        if let Some(kek_dir) = params.get("kek_dir").and_then(|v| v.as_str()) {
            if let Err(e) = state.load_kek(kek_dir) {
                return protocol::error_response(
                    id,
                    protocol::error_code::CRYPTO_ERROR,
                    &format!("failed to load KEK: {}", e),
                );
            }
        } else {
            return protocol::error_response(
                id,
                protocol::error_code::KEK_NOT_LOADED,
                "no KEK loaded (provide kek_dir to load)",
            );
        }
    }

    let wrapped_b64 = match params.get("wrapped_dek").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                "missing wrapped_dek",
            );
        }
    };

    let wrapped = match BASE64.decode(wrapped_b64) {
        Ok(d) => d,
        Err(e) => {
            return protocol::error_response(
                id,
                protocol::error_code::INVALID_PARAMS,
                &format!("invalid base64 wrapped_dek: {}", e),
            );
        }
    };

    let kek_version = params
        .get("kek_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u16;

    match state.unwrap_dek(&wrapped, kek_version) {
        Ok(dek_bytes) => protocol::success_response(
            id,
            serde_json::json!({
                "dek": BASE64.encode(&dek_bytes),
            }),
        ),
        Err(e) => protocol::error_response(
            id,
            protocol::error_code::CRYPTO_ERROR,
            &format!("unwrap_dek failed: {}", e),
        ),
    }
}

/// Write a length-prefixed JSON response to the stream.
fn write_response(
    stream: &mut std::os::unix::net::UnixStream,
    response: &serde_json::Value,
) -> Result<()> {
    let body = serde_json::to_vec(response)?;
    let len = (body.len() as u32).to_be_bytes();
    stream
        .write_all(&len)
        .map_err(|e| BluError::Internal(format!("write length: {}", e)))?;
    stream
        .write_all(&body)
        .map_err(|e| BluError::Internal(format!("write body: {}", e)))?;
    stream
        .flush()
        .map_err(|e| BluError::Internal(format!("flush: {}", e)))?;
    Ok(())
}

/// Remove stale socket and PID files from a previous agent.
fn cleanup_stale(paths: &AgentPaths) {
    if let Some(pid) = paths.read_pid() {
        if !process_alive(pid) {
            info!("removing stale agent files (pid {} not running)", pid);
            paths.cleanup();
        }
    } else if paths.socket_exists() {
        info!("removing stale agent socket (no PID file)");
        paths.cleanup();
    }
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn ctrlc_handler(running: Arc<AtomicBool>) {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
    }
    RUNNING_FLAG
        .lock()
        .map(|mut guard| {
            *guard = Some(running);
        })
        .ok();
}

static RUNNING_FLAG: std::sync::Mutex<Option<Arc<AtomicBool>>> = std::sync::Mutex::new(None);

extern "C" fn signal_handler(_sig: libc::c_int) {
    if let Ok(guard) = RUNNING_FLAG.lock() {
        if let Some(ref flag) = *guard {
            flag.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::agent::paths::AgentPaths;
    use std::os::unix::net::UnixStream;
    use std::str::FromStr;
    use tempfile::tempdir;

    /// Helper: send a JSON-RPC request to the agent and read the response.
    fn send_request(
        socket_path: &std::path::Path,
        request: &serde_json::Value,
    ) -> serde_json::Value {
        let mut stream = UnixStream::connect(socket_path).unwrap();
        let body = serde_json::to_vec(request).unwrap();
        let len = (body.len() as u32).to_be_bytes();
        stream.write_all(&len).unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();

        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).unwrap();
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        let mut resp_buf = vec![0u8; resp_len];
        stream.read_exact(&mut resp_buf).unwrap();
        serde_json::from_slice(&resp_buf).unwrap()
    }

    /// Start a daemon in a background thread and wait for the socket.
    fn start_test_daemon(paths: &AgentPaths) -> std::thread::JoinHandle<()> {
        let paths_clone = paths.clone();
        let handle = std::thread::spawn(move || {
            run_daemon(&paths_clone).unwrap();
        });

        for _ in 0..50 {
            if paths.socket_exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(paths.socket_exists(), "agent socket did not appear");

        handle
    }

    #[test]
    fn daemon_status_and_shutdown() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let handle = start_test_daemon(&paths);

        // Status: should be locked
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "status",
                "params": {},
                "id": 1
            }),
        );
        assert_eq!(resp["result"]["unlocked"], false);
        assert_eq!(resp["result"]["public_key"], serde_json::Value::Null);
        assert!(resp["result"]["profile"].is_string());
        // timeout_remaining should be null when locked
        assert_eq!(resp["result"]["timeout_remaining"], serde_json::Value::Null);

        // Unknown method
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "nonexistent",
                "params": {},
                "id": 2
            }),
        );
        assert_eq!(resp["error"]["code"], -32601);

        // Shutdown
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 3
            }),
        );
        handle.join().unwrap();

        assert!(!paths.socket_exists());
    }

    #[test]
    fn daemon_unlock_lock_cycle() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let handle = start_test_daemon(&paths);

        // Encrypt while locked should fail
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "encrypt",
                "params": { "data": BASE64.encode(b"hello") },
                "id": 10
            }),
        );
        assert_eq!(resp["error"]["code"], protocol::error_code::AGENT_LOCKED);

        // Unlock with the test key (plaintext, no passphrase needed)
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unlock",
                "params": {
                    "identity_path": "test/blu_secrets/blu.key",
                    "passphrase": "unused"
                },
                "id": 11
            }),
        );
        assert!(resp["result"]["public_key"].is_string());
        let pubkey = resp["result"]["public_key"].as_str().unwrap();
        assert!(pubkey.starts_with("age1"));

        // Status should now show unlocked with timeout info
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "status",
                "params": {},
                "id": 12
            }),
        );
        assert_eq!(resp["result"]["unlocked"], true);
        assert_eq!(resp["result"]["public_key"], pubkey);
        assert!(resp["result"]["timeout_remaining"].is_string());

        // Lock
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "lock",
                "params": {},
                "id": 13
            }),
        );
        assert!(resp["result"].is_object());

        // Status should show locked again
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "status",
                "params": {},
                "id": 14
            }),
        );
        assert_eq!(resp["result"]["unlocked"], false);

        // Shutdown
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 99
            }),
        );
        handle.join().unwrap();
    }

    #[test]
    fn daemon_encrypt_decrypt_round_trip() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let handle = start_test_daemon(&paths);

        // Unlock
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unlock",
                "params": {
                    "identity_path": "test/blu_secrets/blu.key",
                    "passphrase": "unused"
                },
                "id": 1
            }),
        );

        // Encrypt
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "encrypt",
                "params": { "data": BASE64.encode(plaintext) },
                "id": 2
            }),
        );
        let ciphertext_b64 = resp["result"]["ciphertext"].as_str().unwrap();
        let ciphertext = BASE64.decode(ciphertext_b64).unwrap();
        assert_ne!(&ciphertext, plaintext);

        // Decrypt
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "decrypt",
                "params": { "data": ciphertext_b64 },
                "id": 3
            }),
        );
        let decrypted_b64 = resp["result"]["plaintext"].as_str().unwrap();
        let decrypted = BASE64.decode(decrypted_b64).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Shutdown
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 99
            }),
        );
        handle.join().unwrap();
    }

    #[test]
    fn daemon_wrap_unwrap_dek_round_trip() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let handle = start_test_daemon(&paths);

        // Unlock
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unlock",
                "params": {
                    "identity_path": "test/blu_secrets/blu.key",
                    "passphrase": "unused"
                },
                "id": 1
            }),
        );

        // Set up a KekStore in the temp dir so the agent can load it
        let blu_dir = tmp.path().join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();
        let identity_str = include_str!("../../test/blu_secrets/blu.key").trim();
        let identity = age::x25519::Identity::from_str(identity_str).unwrap();
        let recipient = identity.to_public().to_string();
        let store = crate::keys::kek::KekStore::new(&blu_dir);
        store.init(&[&recipient]).unwrap();

        // wrap_dek while no KEK is loaded and no kek_dir provided should fail
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "wrap_dek",
                "params": {},
                "id": 2
            }),
        );
        assert_eq!(resp["error"]["code"], protocol::error_code::KEK_NOT_LOADED);

        // wrap_dek with kek_dir should succeed and load the KEK
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "wrap_dek",
                "params": {
                    "kek_dir": blu_dir.to_str().unwrap()
                },
                "id": 3
            }),
        );
        assert!(resp["result"]["dek"].is_string());
        assert!(resp["result"]["wrapped_dek"].is_string());
        assert_eq!(resp["result"]["kek_version"], 0);

        let dek_b64 = resp["result"]["dek"].as_str().unwrap();
        let wrapped_b64 = resp["result"]["wrapped_dek"].as_str().unwrap();

        // unwrap_dek should return the same DEK
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unwrap_dek",
                "params": {
                    "wrapped_dek": wrapped_b64,
                    "kek_version": 0
                },
                "id": 4
            }),
        );
        assert_eq!(resp["result"]["dek"].as_str().unwrap(), dek_b64);

        // Status should show has_kek=true
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "status",
                "params": {},
                "id": 5
            }),
        );
        assert_eq!(resp["result"]["has_kek"], true);
        assert_eq!(resp["result"]["kek_version"], 0);

        // Shutdown
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 99
            }),
        );
        handle.join().unwrap();
    }

    #[test]
    fn daemon_unlock_bad_passphrase() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let handle = start_test_daemon(&paths);

        // Try to unlock with a nonexistent key file
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unlock",
                "params": {
                    "identity_path": "/nonexistent/path/identity.age",
                    "passphrase": "test"
                },
                "id": 1
            }),
        );
        assert_eq!(resp["error"]["code"], protocol::error_code::KEY_NOT_FOUND);

        // Missing params
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "unlock",
                "params": {},
                "id": 2
            }),
        );
        assert_eq!(resp["error"]["code"], protocol::error_code::INVALID_PARAMS);

        // Shutdown
        send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 99
            }),
        );
        handle.join().unwrap();
    }
}
