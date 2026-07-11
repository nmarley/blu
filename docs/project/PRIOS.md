# Priorities

Ranked by impact for beta readiness. Updated 2026-07-09.

## Completed

1. [x] `delete_files` is a no-op (prints info, returns Ok, mutates nothing)
2. [x] Replace bare `.unwrap()` calls with proper error propagation
       (24 fixed: 13 CLI + 11 core lib)
3. [x] Enhance `blu status` with vault summary
4. [x] Remove dead config code
5. [x] Guard divide-by-zero in status when `total_chunks == 0`
6. [x] Replace joke panic message in `encrypt_files.rs` with proper BluError
7. [x] Complete delete cascade (mutate BlobIndex, remove dead blobs
       from backends, full end-to-end)
8. [x] Implement blob defragmentation (repack blobs with dead chunks)
9. [x] Set up GitHub Actions CI (cargo build + test + clippy + fmt)
10. [x] `.bluignore` support
11. [x] `blu doctor` diagnostics (structural + blob presence)
12. [x] README + changelog rewrite for 0.5.0 dogfood
13. [x] End-to-end vault pipeline smoke tests

## Open: broader CLI test coverage

14. [ ] encrypt command tests
15. [ ] delete command tests
16. [ ] backup command tests
17. [ ] restore command tests
18. [ ] list-files command tests
19. [ ] status command tests
20. [ ] search command tests
21. [ ] backend command tests
22. [ ] tag command tests

## Open: polish

23. [ ] Config validation (version compat, S3 fields, local path
       existence)
24. [ ] Feature-gate S3 in Cargo.toml (security-framework is already
       macOS-only)
25. [ ] Persist the search index (constant exists, serde not wired up)
26. [ ] Doctor: backend `list` + orphan detection / repair
