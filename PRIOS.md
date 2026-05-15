# Priorities

Ranked by impact and effort. Updated 2026-05-15.

## Tier 1: Critical

1. [ ] `delete_files` is a no-op (prints info, returns Ok, mutates nothing)
2. [ ] Replace 13 bare `.unwrap()` calls in CLI code with proper error propagation

## Tier 2: Low-Hanging Fruit

3. [ ] Enhance `blu status` with backend awareness (blob counts, sync state)
4. [ ] Remove dead config code (`prune_deleted`, `prune_dangling`, `KeyID`, `KeyType`)
5. [ ] Guard divide-by-zero in status when `total_chunks == 0`
6. [ ] Replace joke panic message in `encrypt_files.rs` with proper BluError

## Tier 3: Important Polish

7. [ ] Config validation (version compat, S3 fields, local path existence)
8. [ ] Feature-gate S3 and security-framework in Cargo.toml
9. [ ] Persist the search index (constant exists, serde not wired up)

## Tier 4: Bigger Lifts

10. [ ] Full delete cascade design (blob marking, live/dead ratio, repacking)
11. [ ] `blu doctor` diagnostics command
12. [ ] CLI test coverage (sync, encrypt, delete, tag)
