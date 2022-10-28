# blu - design

Two halves - plain + encrypted

```
      local client                        local + cloud (replicated)
/---------------------\     AGE      /--------------------\
| plain (unencrypted) |  <-------->  | encrypted          |
|     plain data      |  <-------->  |     encrypted data |
|     plain index     |  <-------->  |                    |
\---------------------/              \--------------------/
```

## Plain

### Plain Data
- Files sitting on filesystem. Could be duplicates taking up extra disk space (can warn on this).

### Plain Index
- List of added files by cryptographic hash (configurable, default sha512)
- List of filenames referenced (in case of duplicates upon add, b/c this system de-duplicates)
- Tags / notes / extra information for searching. _Search not yet implemented._

- Plain index _file_ is also encrypted and stored in the encryption folder next to encrypted data, for backup and restores.

## Encrypted

### Encrypted Data
- Grouping of chunkfiles in hidden dir, sitting as a sub-directory within .blu dir in the plain data location.

### Encrypted Index
- Encrypted chunks of data mapped into chunkfiles
- Index of plain chunks <-> encrypted chunks for mapping back to plain files for restores from only encrypted dir.

## Encryption Keys

__TODO: Finish this design__

All encryption is done via age keys. Currently only single-keypair asymmetric encryption is supported.

