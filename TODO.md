## TODO

### Release

- [x] Merge all src/bin/ files into a single binary? (not really usable as separate ones is it?)

- [ ] Draft initial intro / release post

- [ ] Set up GitHub Actions CI (along w/something for aarch64, maybe BuildJet?)

- [ ] Prep README w/CI/Build badges, header image, use cases, maybe a quicktime GIF/screen recording of the CLI in action?

- Encryption part has to be solid. Maybe send Filippo an email asking his thoughts?


### Functionality

- [ ] Separate std/fs implementation from API. Tools can use std::fs, try to avoid in lib (accept bytes instead of filename, etc.)

- [ ] Multiple backends to allow for redundant backups. E.g. I can have both a local and S3 backend. Or and s3 and a Azure blob storage. Any combination.

- [x] Config backends

- [ ] Deletes, e.g. full data deletes. Also managing "plain index" deletes vs "full deletes" (deletes the encrypted chunks from blob files, or at least marks them for deletion). Which leads to ...
- [ ] Blob defragmentation... e.g. when enough pieces of a blob file are marked for deletion, collect the remaining pieces and group them up all together in a new blob file. Should be fast, Just a straight copy TBH, and then the old blobs (the entire files) get marked for deletion and removed from the blob index. Might need a deletion staging area to ensure the blob index isn't bloated w/old stuff and also lets the "deletion backend sync" happen at a later time. This "deletion backend sync" will involve work on backends as well, basically it ensures that full blobfiles marked for deletion are removed from the storage backends. Obviously it should be after all other syncs (of new/fresh blobfiles) happen first, w/o errors, since those new pieces could contain valid chunks from old blob files.

- Ideas:
  - [ ] Work more on this Global Hash Table idea. Esp. ints to an array of multihashes, (and vice versa -- but a single multihash would map directly to an int. This would allow for expandability / different hashing algos, as well as keep indexes smaller. By making them multihashes, we know which type of hashing algo was used and can map multiple hashes from different algos w/o having to guess at which is which. Gonna hash it out (haha, no pun intended) a bit more.

- [ ] remove hard-coded hashing algo and make it configurable -- also consider when changed, make sure new hashing algo doesn't conflict w/old, e.g. if sha3 is used then sha512, files shouldn't be considered "different" just b/c the hashes are different. if the old version was hashed sha512, that same algo should be used for any comparisons.

- [ ] add a --verbose option to `list_files` which will show number of chunks a
  file has been broken into and the chunk size, maybe also whether it's been
  encrypted or not (but really this just depends on if a blob index exists ...)

- [ ] add to Hash type and allow for different multihashes?
  -- thinking on this, multihash is really just hash digest itself + type of hash algo

- [ ] upgrade multihash crate (a bit of work for v0.19, maybe an hour or 2)

- [ ] tokio for async

- [ ] Add and start to maintain a [changelog](https://keepachangelog.com/en/1.1.0/)
  - Yes, even now. For the changes below that are to-DONE, but I need/want to keep a record of it

-- STREAM INDEXING TO DISK, DO NOT KEEP IT ALL IN MEMORY ... or do?
  - memory map it?

- [ ] status command
  -- which does what? Describe this.
  -- Could display files which are in the PlainIndex but not encrypted
  -- Could display stats, e.g. # files, # bytes de-duplicated (saved), x tags being used, etc.

- [ ] add/edit/remove notes on files, larger bodies of text than tag. Should also be searchable.

- [ ] Seed Phrase generation / recovery for AGE keys + Recovery Kits (a la 1Password)
  See: <https://electrum.readthedocs.io/en/latest/seedphrase.html>

- [ ] multi-key encryption/recovery. How to handle this?

Other storage backends such as s3, etc. Current version only implements local disk!
- [ ] s3
- [ ] digital ocean one?
- [ ] Google Cloud?
- [ ] Azure?

- [ ] Consider if there should be a .bluignore, similar to .gitignore? Or within .blu, e.g. .blu/ignore?

- [ ] Consider licensing as Apache + MIT dual license or similar
