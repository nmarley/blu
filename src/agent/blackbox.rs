//! Integration test for BlackBox::Agent variant.
//!
//! This module previously contained AgentBlackBox and BlackBoxProxy,
//! which have been replaced by the BlackBox::Agent enum variant in
//! src/age.rs. The test remains here to verify the agent-backed
//! BlackBox works end-to-end.

#[cfg(test)]
mod test {
    use crate::age::BlackBox;
    use crate::agent::client::AgentClient;
    use crate::agent::daemon::run_daemon;
    use crate::agent::paths::AgentPaths;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn agent_blackbox_encrypt_decrypt() {
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
        client.unlock("test/blu_secrets/blu.key", "unused").unwrap();

        let bbox = BlackBox::from_agent(client);

        let plaintext = b"agent BlackBox round-trip test";
        let ciphertext = bbox.encrypt(plaintext).unwrap();
        let decrypted = bbox.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Use a fresh client for shutdown since the original was moved
        let shutdown_client = AgentClient::with_paths(paths);
        shutdown_client.shutdown().unwrap();
        handle.join().unwrap();
    }
}
