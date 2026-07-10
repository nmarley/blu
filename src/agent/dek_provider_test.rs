//! Integration test for agent-backed DekProvider.
//!
//! This module verifies that an agent-backed DekProvider can perform
//! v2 envelope encryption (wrap_dek/unwrap_dek) end-to-end.

#[cfg(test)]
mod test {
    use crate::agent::client::AgentClient;
    use crate::agent::daemon::run_daemon;
    use crate::agent::paths::AgentPaths;
    use crate::dek_provider::{decrypt_envelope, encrypt_envelope, DekProvider};
    use crate::keys::hybrid_kem::{public_key_from_seed, HybridSeed};
    use crate::keys::kek::KekStore;
    use crate::keys::pq::PqRecipient;
    use crate::v2format::FileType;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn agent_dek_provider_v2_round_trip() {
        let tmp = tempdir().unwrap();
        let tmp_path = tmp.keep();
        let paths = AgentPaths::from_base(&tmp_path);
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
        let seed = HybridSeed::new([7u8; 32]);
        client.unlock_with_pq_seed(seed.as_bytes()).unwrap();

        // Create a KEK store so the agent can load a KEK
        let blu_dir = tmp_path.join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();
        let recipient = PqRecipient::new(public_key_from_seed(&seed)).to_string();
        let store = KekStore::new(&blu_dir);
        let pq_recipient = crate::keys::pq::parse_pq_recipient(&recipient).unwrap();
        let recipients: Vec<&dyn age::Recipient> = vec![&pq_recipient as &dyn age::Recipient];
        store.init_with(&recipients, &[recipient]).unwrap();

        // Load KEK via wrap_dek with kek_dir
        let kek_dir = blu_dir.to_str().unwrap();
        client.wrap_dek(Some(kek_dir)).unwrap();

        let keys = DekProvider::Agent {
            client,
            kek_dir: Some(kek_dir.to_string()),
        };

        let plaintext = b"agent DekProvider v2 round-trip test";
        let ciphertext = encrypt_envelope(plaintext, FileType::Blob, &keys).unwrap();
        let decrypted = decrypt_envelope(&ciphertext, &keys).unwrap();
        assert_eq!(&decrypted, plaintext);

        // Use a fresh client for shutdown since the original was moved
        let shutdown_client = AgentClient::with_paths(paths);
        shutdown_client.shutdown().unwrap();
        handle.join().unwrap();
    }
}
