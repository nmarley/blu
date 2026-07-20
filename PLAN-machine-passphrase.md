# PLAN: Machine-supplied passphrases (greenfield)

Make every passphrase entry point in the CLI scriptable so the whole
pipeline (identity, init, backup, restore, doctor) can run headless.
Follows the industry-standard pattern (restic `RESTIC_PASSWORD`,
borg `BORG_PASSPHRASE`, gpg `--passphrase-fd`): the interactive TTY
prompt stays the default; environment variables are the documented
automation path.

Today `blu identity init --no-passphrase` still prompts (mnemonic
"25th word" via TTY-only rpassword, plus a typed `yes` confirmation),
and every agent unlock path prompts on a locked agent. Scripts and CI
cannot drive the CLI end to end; the in-repo smoke tests bypass the
CLI entirely for this reason.

## Decision

- `BLU_PASSPHRASE`: passphrase for the encrypted global identity
  file. Used by agent unlock paths and by identity file encryption
  at `identity init` / `identity recover` time.
- `BLU_MNEMONIC_PASSPHRASE`: optional BIP39 "25th word" for
  `identity init` / `identity recover`.
- `--yes` on `blu identity init`: skip the "type yes" mnemonic
  confirmation; prompts that have no env value take their default
  (no 25th word, no file encryption).
- No `--passphrase <value>` CLI argument, ever: command-line args
  leak via `ps` and shell history. Env vars are visible to same-user
  processes only; this tradeoff is documented, not hidden.
- Precedence (unlock paths): already-unlocked agent wins, then
  `BLU_PASSPHRASE`, then TTY prompt. `--no-passphrase` (existing)
  still overrides everything with an empty passphrase.
- Env set but wrong: fail with the agent's error, never silently
  fall through to an interactive prompt (scripts must not hang).
- Empty env value is a valid empty passphrase (equivalent to
  `--no-passphrase`).
- Resolver returns `zeroize::Zeroizing<String>`; passphrases are
  never logged.

## Stages

Stage 1: Passphrase resolver module
  1a: Add `src/cli/passphrase.rs`: `passphrase_from_env()` and
      `mnemonic_passphrase_from_env()` reading the two env vars
  1b: Unit tests for set/unset/empty handling (serialized env
      access; tests share the process environment)

Stage 2: Wire unlock paths through the resolver
  2a: `helpers.rs` `try_agent_keys`: env between empty-passphrase
      attempt and prompt
  2b: `agent_cmd.rs` `unlock_with_passphrase`: env before prompt
  2c: `init.rs` / `open.rs` global identity passphrase loads
  2d: Grep audit: every `prompt_passphrase` caller routes through
      the resolver first

Stage 3: Non-interactive identity init/recover
  3a: `--yes` flag on `identity init` skips the "type yes"
      confirmation
  3b: Mnemonic 25th word from `BLU_MNEMONIC_PASSPHRASE`; `--yes`
      with no env means no 25th word
  3c: `save_pq_seed_file` encryption passphrase from
      `BLU_PASSPHRASE` (skip the confirm re-prompt)
  3d: `identity recover` honors both env vars (mnemonic words
      already come from stdin)

Stage 4: Headless end-to-end smoke script
  4a: `scripts/e2e-passphrase-smoke.sh`: sandboxed XDG dirs,
      `BLU_PASSPHRASE` set, identity init `--yes`, then init,
      backup, restore, doctor, and a content diff
  4b: Assert the identity file is actually encrypted on disk
  4c: Run green locally (script ships mode 0644)

Stage 5: Docs
  5a: `README.md`: automation section (env vars, `--yes`, same-user
      visibility caveat, why no `--passphrase` arg)
  5b: `AGENTS.md`: note the env seam at the agent/identity paths
  5c: `CHANGELOG.md` Unreleased Added entry

Stage 6: Verify
  6a: `cargo fmt -- --check`, `cargo test`,
      `cargo clippy --all-targets`
  6b: `scripts/e2e-passphrase-smoke.sh` green

## Non-goals

- `--passphrase <value>` or `--passphrase-fd` / stdin variants
  (env covers the automation case; more knobs later if needed)
- Changes to the agent socket protocol or biometric unlock
- Passphrase-encrypted vault configs beyond the global identity
- Migrating anyone off interactive prompts (default UX unchanged)

## Suggested commit split

One atomic commit per stage (1 through 5). Stage 6 is verification
only, no commit unless docs/tests needed a fixup.
