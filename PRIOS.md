# Priorities

Ranked by impact for beta readiness. Updated 2026-05-15.

## Completed

1. [x] `delete_files` is a no-op (prints info, returns Ok, mutates nothing)
2. [x] Replace bare `.unwrap()` calls with proper error propagation
       (24 fixed: 13 CLI + 11 core lib)
3. [x] Enhance `blu status` with vault summary
4. [x] Remove dead config code
5. [x] Guard divide-by-zero in status when `total_chunks == 0`
6. [x] Replace joke panic message in `encrypt_files.rs` with proper BluError

## Tier 1: Finish the Data Pipeline

7. [ ] Complete delete cascade (mutate BlobIndex, remove dead blobs
       from backends, full end-to-end)
8. [ ] Implement blob defragmentation (repack blobs with dead chunks)

## Tier 2: Test Coverage and CI

9. [ ] Set up GitHub Actions CI (cargo build + cargo test on push)
10. [ ] encrypt command tests
11. [ ] delete command tests
12. [ ] sync command tests
13. [ ] restore command tests
14. [ ] list-files command tests
15. [ ] status command tests
16. [ ] search command tests
17. [ ] backend command tests
18. [ ] tag command tests

## Tier 3: Beta Readiness

19. [ ] `blu doctor` diagnostics command

## Tier 4: Polish

20. [ ] Config validation (version compat, S3 fields, local path
       existence)
21. [ ] Feature-gate S3 and security-framework in Cargo.toml
22. [ ] Persist the search index (constant exists, serde not wired up)
