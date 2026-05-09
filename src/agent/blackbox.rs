//! Integration test for BlackBox::Agent variant.
//!
//! This module verifies that an agent-backed BlackBox can perform
//! v2 envelope encryption (wrap_dek/unwrap_dek) end-to-end.

#[cfg(test)]
mod test {
    use crate::age::BlackBox;
    use crate::agent::client::AgentClient;
    use crate::agent::daemon::run_daemon;
    use crate::agent::paths::AgentPaths;
    use crate::keys::kek::KekStore;
    use std::str::FromStr;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn agent_blackbox_v2_round_trip() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.keep();
        let paths = AgentPaths::from_base(&tmp_path).unwrap();
        let paths_for_daemon = paths.clone();

        let handle = std::thread::spawn(move || {
            run_daemon(&paths_for_daemon).unwrap();
        });

        for _ in 0..50 {
            if paths.socket_exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let client = AgentClient::with_paths(paths.clone());
        let secret = include_str!("../../test/blu_secrets/blu.key").trim();
        client.unlock_with_secret(secret).unwrap();

        // Create a KEK store so the agent can load a KEK
        let blu_dir = tmp_path.join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();
        let identity = age::x25519::Identity::from_str(secret).unwrap();
        let recipient = identity.to_public().to_string();
        let store = KekStore::new(&blu_dir);
        store.init(&[&recipient]).unwrap();

        // Load KEK via wrap_dek with kek_dir
        let kek_dir = blu_dir.to_str().unwrap();
        client.wrap_dek(Some(kek_dir)).unwrap();

        let bbox = BlackBox::from_agent(client);

        let plaintext = b"agent BlackBox v2 round-trip test";
        let ciphertext = bbox.encrypt_blob(plaintext).unwrap();
        let decrypted = bbox.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Use a fresh client for shutdown since the original was moved
        let shutdown_client = AgentClient::with_paths(paths);
        shutdown_client.shutdown().unwrap();
        handle.join().unwrap();
    }
}
