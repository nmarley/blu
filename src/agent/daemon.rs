//! Agent daemon: listens on a Unix socket and manages lifecycle.
//!
//! The daemon is started by `blu __agent-daemon` (an internal
//! subcommand). It:
//!
//! 1. Cleans up any stale socket/PID files
//! 2. Binds a Unix domain socket at `~/.blu/agent.sock`
//! 3. Sets socket permissions to 0600 (owner-only)
//! 4. Writes its PID to `~/.blu/agent.pid`
//! 5. Accepts connections and processes JSON-RPC requests
//! 6. On shutdown: zeroizes secrets, removes socket and PID file

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::agent::paths::AgentPaths;
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

    // Handle SIGTERM and SIGINT for graceful shutdown
    ctrlc_handler(running_signal);

    // Set a short timeout on the listener so we can check the
    // shutdown flag periodically
    listener
        .set_nonblocking(false)
        .map_err(|e| BluError::Internal(format!("failed to configure socket: {}", e)))?;

    // Accept loop
    while running.load(Ordering::SeqCst) {
        // Use a timeout so we periodically check the shutdown flag.
        // std UnixListener does not have set_timeout, so we use
        // non-blocking mode with a sleep.
        listener
            .set_nonblocking(true)
            .map_err(|e| BluError::Internal(format!("set_nonblocking: {}", e)))?;

        match listener.accept() {
            Ok((mut stream, _addr)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|e| BluError::Internal(format!("stream config: {}", e)))?;

                match handle_connection(&mut stream, &running) {
                    Ok(()) => {}
                    Err(e) => {
                        warn!("error handling connection: {}", e);
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No pending connection; sleep briefly then retry
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                warn!("accept error: {}", e);
            }
        }
    }

    info!("agent shutting down");
    paths.cleanup();
    Ok(())
}

/// Handle a single client connection.
///
/// Reads one length-prefixed JSON message, dispatches it, writes the
/// response, and closes the connection. (Stage 1b will add the full
/// JSON-RPC dispatch; for now this is a minimal skeleton.)
fn handle_connection(
    stream: &mut std::os::unix::net::UnixStream,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    // Read 4-byte big-endian length prefix
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|e| BluError::Internal(format!("read length: {}", e)))?;
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check: reject messages larger than 64 MiB
    if msg_len > 64 * 1024 * 1024 {
        return Err(BluError::Internal("message too large".into()));
    }

    // Read the JSON payload
    let mut payload = vec![0u8; msg_len];
    stream
        .read_exact(&mut payload)
        .map_err(|e| BluError::Internal(format!("read payload: {}", e)))?;

    let request: serde_json::Value = serde_json::from_slice(&payload)?;

    // Minimal dispatch: only "status" and "shutdown" for stage 1a.
    // Full JSON-RPC dispatch is stage 1b.
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let response = match method {
        "status" => serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "unlocked": false,
                "public_key": null,
                "expires_at": null,
                "vaults": []
            },
            "id": id
        }),
        "shutdown" => {
            running.store(false, Ordering::SeqCst);
            serde_json::json!({
                "jsonrpc": "2.0",
                "result": {},
                "id": id
            })
        }
        _ => {
            serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32601,
                    "message": "method not found"
                },
                "id": id
            })
        }
    };

    write_response(stream, &response)?;
    Ok(())
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

/// Remove stale socket and PID files from a previous agent that did
/// not shut down cleanly.
fn cleanup_stale(paths: &AgentPaths) {
    if let Some(pid) = paths.read_pid() {
        if !process_alive(pid) {
            info!("removing stale agent files (pid {} not running)", pid);
            paths.cleanup();
        }
    } else if paths.socket_exists() {
        // PID file missing but socket exists; stale
        info!("removing stale agent socket (no PID file)");
        paths.cleanup();
    }
}

/// Check whether a process with the given PID is alive.
fn process_alive(pid: u32) -> bool {
    // signal 0 checks if the process exists without sending a signal
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Install a handler for SIGTERM/SIGINT that sets the running flag
/// to false.
fn ctrlc_handler(running: Arc<AtomicBool>) {
    // We use a simple signal-safe approach: set an atomic flag.
    // The accept loop checks this flag on each iteration.
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
    // Store the Arc in a static so the signal handler can access it.
    // This is safe because we only set it once before the accept loop.
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

    #[test]
    fn daemon_status_and_shutdown() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        let paths_clone = paths.clone();

        // Start the daemon in a background thread
        let handle = std::thread::spawn(move || {
            run_daemon(&paths_clone).unwrap();
        });

        // Wait for socket to appear
        for _ in 0..50 {
            if paths.socket_exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(paths.socket_exists(), "agent socket did not appear");

        // Send status request
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

        // Send unknown method
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

        // Send shutdown
        let resp = send_request(
            &paths.socket,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "shutdown",
                "params": {},
                "id": 3
            }),
        );
        assert!(resp["result"].is_object());

        // Daemon thread should exit
        handle.join().unwrap();

        // Socket and PID file should be cleaned up
        assert!(!paths.socket_exists());
        assert!(paths.read_pid().is_none());
    }
}
