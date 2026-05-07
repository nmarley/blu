# Remove Vault-Local Identity Copy

Single-password UX: the vault-local `.blu/identity.age` is a redundant
copy of the global `~/.blu/identity.age`. Removing it eliminates the
second passphrase prompt during `blu init` and aligns with 1Password's
model where identity lives in one place.

## Stage 1: Remove `identity_file` from `EncryptionConfig`

1a. `src/config.rs:49-52` -- Remove `identity_file: PathBuf` from `EncryptionConfig`
1b. `src/config.rs:55-57` -- Remove `default_identity_file()` function
1c. `src/config.rs:64` -- Remove `identity_file` from the `Default` impl
1d. `src/config.rs:194-196` -- Remove `Config::identity_path()` method
1e. `src/config.rs:207-211` -- Change `Config::load_blackbox()` to take
    an explicit path instead of calling `identity_path()`
1f. `src/config.rs:10` -- Remove `IDENTITY_FILENAME` import if unused

## Stage 2: Remove vault-local identity write from `blu init`

2a. `src/cli/init.rs:22-32` -- Remove `identity` and `passphrase` from
    `InitVaultParams`; only public keys are needed. Pass identity
    separately for the initial index write.
2b. `src/cli/init.rs:46-52` -- Remove `save_identity()` call from
    `init_vault()`
2c. `src/cli/init.rs:59` -- Remove `identity_file` from config construction
2d. `src/cli/init.rs:103-105` -- Refactor: pass identity (or BlackBox)
    into `init_vault()` separately for the empty index write, not as
    part of `InitVaultParams`
2e. `src/cli/init.rs:108,117` -- Remove `identity_path` from
    `InitVaultResult`
2f. `src/cli/init.rs:198` -- Remove `resolve_passphrase()` call (the
    second passphrase prompt)
2g. `src/cli/init.rs:204` -- Remove `passphrase` from `InitVaultParams`
    construction
2h. `src/cli/init.rs:210,219-220` -- Remove "Saving private key" and
    key-file backup messages
2i. `src/cli/init.rs:262-283` -- Remove `resolve_passphrase()` function

## Stage 3: Update agent to resolve `~/.blu/identity.age` itself

3a. `src/agent/state.rs:210` -- Change `unlock()`: remove
    `identity_path` parameter, resolve `~/.blu/identity.age` via
    `dirs::home_dir()`
3b. `src/agent/daemon.rs:225-234` -- `handle_unlock()`: stop requiring
    `identity_path` from params; let `state.unlock()` find the file.
    Still require `passphrase`.
3c. `src/agent/client.rs:137-152` -- `AgentClient::unlock()`: remove
    `identity_path` parameter, only send `passphrase`

## Stage 4: Update callers that send identity path to agent

4a. `src/cli/helpers.rs:122-136` -- `try_agent_blackbox()`: remove
    `cfg.identity_path()` and identity_str. Call `client.unlock("")`
    then `client.unlock(&pass)`.
4b. `src/cli/helpers.rs:141-149` -- `load_blackbox_via_agent()`: remove
    identity path, just send passphrase
4c. `src/cli/helpers.rs:153-154` -- `load_blackbox_inprocess()`: load
    identity from global `~/.blu/identity.age` instead of
    `cfg.load_blackbox()`
4d. `src/cli/agent_cmd.rs:77-107` -- `unlock_with_passphrase()`: remove
    vault config dependency. No longer requires being inside a blu repo;
    just sends passphrase to agent.

## Stage 5: Update tests

5a. `src/cli/init.rs:396-424` -- Remove
    `init_vault_writes_identity_with_passphrase` test
5b. `src/cli/init.rs:330-331` -- Remove `identity_path.exists()` assert
5c. `src/cli/init.rs:484-497` -- Update backward-compat test: remove
    `identity_file` from TOML
5d. `src/cli/init.rs:499-513` -- Update round-trip test: remove
    `identity_file` from `EncryptionConfig`
5e. `src/cli/init.rs:515-531` -- Same for the no-pq round-trip test
5f. Update all `InitVaultParams` in tests to match new struct shape
5g. `src/agent/state.rs` -- Agent state tests: switch from
    `state.unlock(TEST_KEY_PATH, ...)` to `state.unlock_with_secret()`
    (read test key, extract secret string). No optional-path hack in
    production API.
5h. `src/agent/daemon.rs` -- Daemon tests: update unlock params to only
    send `passphrase`; use `unlock_with_secret` RPC for tests that need
    a custom key file.

## Stage 6: Clean up dead code and exports

6a. `src/keys/mod.rs:35` -- `IDENTITY_FILENAME`: keep if still used by
    `identity_cmd.rs`, otherwise remove
6b. `src/keys/mod.rs:1-5` -- Update module doc comment (no longer
    "stored in `.blu/` directory")
6c. `src/cli.rs:36` -- Remove `InitVaultResult` from exports if
    `identity_path` is gone
6d. `src/cli/init.rs:14` -- Remove `IDENTITY_FILENAME` import if unused

## Stage 7: Update design docs

7a. `ENVELOPE_ENCRYPTION_DESIGN.md:611-612` -- Remove vault-local
    identity reference
7b. `PLAN.md:137` -- Remove `identity_path` from unlock RPC description
7c. `README.md:104,134` -- Update vault-local identity references

## Design decisions

**Agent test strategy**: Agent tests currently call
`state.unlock(TEST_KEY_PATH, ...)`. After removing the path parameter,
tests will use `state.unlock_with_secret()` instead (read the test key
file, extract the secret string). This keeps the production API clean
with no optional-path parameter leaking test concerns.

**`--no-passphrase` on `InitArgs`**: Stays. It controls whether the
global identity passphrase is prompted during `blu init`. Semantics
become cleaner since there is now only one passphrase in the flow.

**`--no-passphrase` on global `Args`**: Stays. Used by the in-process
fallback in `helpers.rs`. Now unambiguously means "do not prompt for
the (only) identity passphrase."
