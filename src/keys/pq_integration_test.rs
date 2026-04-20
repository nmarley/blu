//! Post-quantum integration tests.
//!
//! These tests exercise the full pipeline from BIP39 mnemonic through
//! PQ key derivation, KEK wrapping, DEK wrapping, and data encryption.
//! They also test interoperability with Go age v1.3.1.

#[cfg(test)]
mod test {
    use std::io::{Read, Write};

    use age::{Identity, Recipient};
    use rand::RngCore;

    use crate::keys::dek::{self, Dek};
    use crate::keys::hybrid_kem::{self, HybridSeed};
    use crate::keys::kek::KekStore;
    use crate::keys::mnemonic;
    use crate::keys::pq::{self, PqIdentity, PqRecipient};

    const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon art";

    #[test]
    fn full_pipeline_mnemonic_to_data() {
        // Step 1: BIP39 mnemonic -> seed
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "test-passphrase");

        // Step 2: Derive PQ identity and recipient
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();
        let pq_recipient = mnemonic::derive_pq_recipient(&seed).unwrap();

        // Verify they match
        assert_eq!(
            pq_identity.to_public().public_key().as_bytes(),
            pq_recipient.public_key().as_bytes()
        );

        // Step 3: Create KEK store, init with PQ recipient
        let tmp = tempfile::tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();

        let store = KekStore::new(&blu_dir);
        let recipient_str = pq_recipient.to_string();
        let kek = store
            .init_with(
                &[&pq_recipient as &dyn Recipient],
                std::slice::from_ref(&recipient_str),
            )
            .unwrap();

        // Step 4: Verify metadata
        let meta = store.load_metadata().unwrap();
        assert_eq!(meta.current_version, 0);
        assert_eq!(meta.versions[0].users, vec![recipient_str]);

        // Step 5: Unwrap KEK with PQ identity
        let (unwrapped_kek, version) = store
            .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
            .unwrap();
        assert_eq!(version, 0);
        assert_eq!(unwrapped_kek.as_bytes(), kek.as_bytes());

        // Step 6: DEK wrap/unwrap
        let (dek, wrapped_dek) = dek::generate_and_wrap(&kek).unwrap();
        let recovered_dek = Dek::unwrap(&kek, &wrapped_dek).unwrap();
        assert_eq!(recovered_dek.as_bytes(), dek.as_bytes());

        // Step 7: Data encrypt/decrypt
        let plaintext = b"most excellent post-quantum vault data";
        let ciphertext = dek.encrypt_data(plaintext).unwrap();
        let decrypted = recovered_dek.decrypt_data(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn pq_recipient_string_round_trip_through_kek_metadata() {
        // Verify that PQ recipient strings survive serialization
        // through kek.toml metadata.
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let recipient = mnemonic::derive_pq_recipient(&seed).unwrap();
        let recipient_str = recipient.to_string();

        // Parse it back
        let parsed = pq::parse_pq_recipient(&recipient_str).unwrap();
        assert_eq!(
            parsed.public_key().as_bytes(),
            recipient.public_key().as_bytes()
        );

        // Verify it starts with the right HRP
        assert!(recipient_str.starts_with("age1pq"));
    }

    #[test]
    fn pq_identity_bech32_deterministic_from_mnemonic() {
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");

        let id1 = mnemonic::derive_pq_identity(&seed).unwrap();
        let id2 = mnemonic::derive_pq_identity(&seed).unwrap();

        assert_eq!(id1.to_bech32(), id2.to_bech32());
        assert!(id1.to_bech32().starts_with("AGE-SECRET-KEY-PQ-"));
    }

    #[test]
    fn age_file_round_trip_with_pq_stanzas() {
        // Encrypt a full age file (not just a file key) using PQ,
        // then decrypt it. This exercises the age crate's
        // Encryptor/Decryptor with our custom Recipient/Identity.
        let mut seed_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed_bytes);
        let seed = HybridSeed::new(seed_bytes);

        let recipient = PqRecipient::new(hybrid_kem::public_key_from_seed(&seed));
        let identity = PqIdentity::new(seed);

        let plaintext = b"this data is protected by post-quantum cryptography \
                          using ML-KEM-768 and X25519 in a hybrid construction \
                          that follows the age spec v1.1.0 mlkem768x25519 type";

        // Encrypt
        let recipients: Vec<&dyn Recipient> = vec![&recipient];
        let encryptor = age::Encryptor::with_recipients(recipients.into_iter()).unwrap();
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(plaintext).unwrap();
        writer.finish().unwrap();

        // The encrypted output should be a valid age file
        assert!(encrypted.starts_with(b"age-encryption.org"));

        // Decrypt
        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn Identity))
            .unwrap();
        let mut decrypted = vec![];
        reader.read_to_end(&mut decrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn backward_compat_x25519_kek_with_pq_agent() {
        // An agent with both PQ and X25519 identities should be able
        // to unwrap a KEK that was wrapped with X25519 only (old format).
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");

        let x25519_identity = mnemonic::derive_x25519_identity(&seed).unwrap();
        let x25519_pubkey = mnemonic::public_key_from_identity(&x25519_identity);
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();

        // Create a KEK store with X25519 recipient (old format)
        let tmp = tempfile::tempdir().unwrap();
        let blu_dir = tmp.path().join(".blu");
        std::fs::create_dir_all(&blu_dir).unwrap();

        let store = KekStore::new(&blu_dir);
        let kek = store.init(&[&x25519_pubkey]).unwrap();

        // Unwrap with both PQ and X25519 identities (PQ first)
        let (unwrapped, version) = store
            .unwrap_current_kek_with(&[
                &pq_identity as &dyn Identity,
                &x25519_identity as &dyn Identity,
            ])
            .unwrap();
        assert_eq!(version, 0);
        assert_eq!(unwrapped.as_bytes(), kek.as_bytes());
    }

    #[test]
    fn go_age_decrypt_our_pq_output() {
        // Encrypt data with our PQ implementation, then decrypt with
        // Go age v1.3.1. This tests interoperability.
        //
        // If Go age is not available, skip this test.
        let age_bin = std::process::Command::new("age").arg("--version").output();
        if age_bin.is_err() {
            eprintln!("skipping Go age interop test (age not found)");
            return;
        }

        // Generate a Go-compatible PQ keypair
        let keygen = std::process::Command::new("age-keygen")
            .arg("--pq")
            .output()
            .unwrap();
        assert!(keygen.status.success(), "age-keygen --pq failed");

        let keygen_output = String::from_utf8(keygen.stdout).unwrap();
        // First line is "# created: ..."
        // Second line is "# public key: age1pq..."
        // Third line is "AGE-SECRET-KEY-PQ-..."
        let pubkey_line = keygen_output
            .lines()
            .find(|l| l.contains("public key:"))
            .unwrap();
        let pubkey = pubkey_line.split(": ").nth(1).unwrap().trim();
        let secret_line = keygen_output
            .lines()
            .find(|l| l.starts_with("AGE-SECRET-KEY-PQ-"))
            .unwrap()
            .trim();

        // Parse the Go-generated public key with our code
        let recipient = pq::parse_pq_recipient(pubkey).unwrap();

        // Encrypt with our code
        let plaintext = b"cross-implementation PQ test";
        let recipients: Vec<&dyn Recipient> = vec![&recipient];
        let encryptor = age::Encryptor::with_recipients(recipients.into_iter()).unwrap();
        let mut encrypted = vec![];
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        writer.write_all(plaintext).unwrap();
        writer.finish().unwrap();

        // Write encrypted data and the secret key to temp files
        let tmp = tempfile::tempdir().unwrap();
        let enc_path = tmp.path().join("encrypted.age");
        let key_path = tmp.path().join("key.txt");
        std::fs::write(&enc_path, &encrypted).unwrap();
        std::fs::write(&key_path, secret_line).unwrap();

        // Decrypt with Go age
        let decrypt = std::process::Command::new("age")
            .args(["--decrypt", "--identity"])
            .arg(&key_path)
            .arg(&enc_path)
            .output()
            .unwrap();

        assert!(
            decrypt.status.success(),
            "Go age decrypt failed: {}",
            String::from_utf8_lossy(&decrypt.stderr)
        );
        assert_eq!(decrypt.stdout, plaintext);
    }

    #[test]
    fn our_code_decrypts_go_age_pq_output() {
        // Encrypt data with Go age v1.3.1 PQ, then decrypt with our
        // PQ implementation. This tests the reverse direction.
        let age_bin = std::process::Command::new("age").arg("--version").output();
        if age_bin.is_err() {
            eprintln!("skipping Go age interop test (age not found)");
            return;
        }

        // Generate a PQ keypair with Go age
        let keygen = std::process::Command::new("age-keygen")
            .arg("--pq")
            .output()
            .unwrap();
        assert!(keygen.status.success());

        let keygen_output = String::from_utf8(keygen.stdout).unwrap();
        let pubkey_line = keygen_output
            .lines()
            .find(|l| l.contains("public key:"))
            .unwrap();
        let pubkey = pubkey_line.split(": ").nth(1).unwrap().trim();
        let secret_line = keygen_output
            .lines()
            .find(|l| l.starts_with("AGE-SECRET-KEY-PQ-"))
            .unwrap()
            .trim();

        // Write plaintext to a temp file, encrypt with Go age
        let tmp = tempfile::tempdir().unwrap();
        let pt_path = tmp.path().join("plaintext.txt");
        let enc_path = tmp.path().join("encrypted.age");
        let plaintext = b"go-to-rust PQ interop test";
        std::fs::write(&pt_path, plaintext).unwrap();

        let encrypt = std::process::Command::new("age")
            .args(["--encrypt", "--recipient", pubkey, "--output"])
            .arg(&enc_path)
            .arg(&pt_path)
            .output()
            .unwrap();
        assert!(
            encrypt.status.success(),
            "Go age encrypt failed: {}",
            String::from_utf8_lossy(&encrypt.stderr)
        );

        // Read the encrypted data and decrypt with our code
        let encrypted = std::fs::read(&enc_path).unwrap();
        let identity = pq::parse_pq_identity(secret_line).unwrap();

        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn Identity))
            .unwrap();
        let mut decrypted = vec![];
        reader.read_to_end(&mut decrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }
}
