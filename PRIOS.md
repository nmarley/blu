# Priorities

Ranked by impact and effort. Updated 2026-05-15.

## Tier 1: Critical

1. [x] `delete_files` is a no-op (prints info, returns Ok, mutates nothing)
2. [x] Replace bare `.unwrap()` calls with proper error propagation
       (24 fixed: 13 CLI + 11 core lib)

## Tier 2: Low-Hanging Fruit

3. [ ] Enhance `blu status` with backend awareness (blob counts, sync state)
4. [x] Remove dead config code (`prune_deleted`, `prune_dangling`, `KeyID`, `KeyType`)
5. [x] Guard divide-by-zero in status when `total_chunks == 0`
6. [x] Replace joke panic message in `encrypt_files.rs` with proper BluError
       (added `BlockHashMismatch` variant to `BluError`)

## Tier 3: Important Polish

7. [ ] Config validation (version compat, S3 fields, local path existence)
8. [ ] Feature-gate S3 and security-framework in Cargo.toml
9. [ ] Persist the search index (constant exists, serde not wired up)

## Tier 4: Bigger Lifts

10. [ ] Backend blob garbage collection (act on `paths_to_delete` from delete cascade)
11. [ ] Blob defragmentation (repack blobs with dead chunks)
12. [ ] `blu doctor` diagnostics command
13. [ ] CLI test coverage (sync, encrypt, delete, tag)
