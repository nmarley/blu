use multihash::{Code, MultihashDigest};
use std::collections::HashMap;
use std::{fmt, fs, path::Path};
use walkdir::WalkDir;

pub mod age;
pub mod clap;
pub mod config;
pub mod magic;

use magic::Wizard;

// also: consider an internal webserver which serves up the UI for blu
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // TODO: handle cmd-line args w/clap
    // let mut args: Args = Args::parse();
    // if args.num_crawlers < 1 || args.num_crawlers > 999 {
    //     args.num_crawlers = 96; // how to get default here?
    // }
    // dbg!(&args);

    // let key = read-key-from-.blu/metadata.json;
    // decrypt somehow?

    // let conn = db.connection();
    let dir = "."; // TODO: use Args
    let cfg = config::read_config(dir);
    dbg!(&cfg);

    // TODO: _iff_ we wanted to chdir before indexing, **HERE** is where to do
    // it
    let _map_files = index(dir)?;
    // TODO: ... and HERE is where to change back

    // dbg!(&map_files);

    Ok(())
}

// TODO: rename this struct ...
// FileMeta? Archive?
#[derive(PartialEq)]
pub struct Entry {
    // paths: Vec<std::path::Path>,
    paths: Vec<String>,
    filetype: String,

    hash: Vec<u8>,
    size: u64,
    enc: Option<Encrypted>,

    tags: Vec<String>,     // TODO: proper tagging, or... ?
    notes: Option<String>, // free-form text
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("paths", &self.paths)
            .field("filetype", &self.filetype)
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("enc", &self.enc)
            .field("tags", &self.tags)
            .field("notes", &self.notes)
            .finish()
    }
}

#[derive(PartialEq)]
pub struct Encrypted {
    hash: Vec<u8>,
    size: u64,
    keys: Vec<config::KeyID>,
}

impl fmt::Debug for Encrypted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encrypted")
            .field("hash", &hex::encode(&self.hash))
            .field("size", &self.size)
            .field("keys", &self.keys)
            .finish()
    }
}

// walk the dir and hash all regular files
// ignore block/char specials
//
// TODO: accept an SQLite3 connection for metadata writes?
fn index<P: AsRef<Path>>(
    base_dir: P,
) -> Result<HashMap<Vec<u8>, Entry>, Box<dyn std::error::Error>> {
    let mut count = 0usize;

    // TODO: only build a new hashmap if we don't get metadata from the DB already
    let mut map_files = HashMap::new();

    // chdir into base before walking
    //
    // otherwise we get paths like "./test/file.txt" if we set the base dir to
    // "./test"

    // let current_dir = env::current_dir()?;
    // env::set_current_dir(&base_dir)?;

    let wiz = Wizard::new();

    for entry in WalkDir::new(base_dir).into_iter().filter_map(|e| e.ok()) {
        // for initial debugging
        if count == 5 {
            break;
        }

        // TODO: allow symlinks?
        if !entry.file_type().is_file() {
            continue;
        }
        count += 1;

        let metadata = fs::metadata(entry.path())?;
        let size = metadata.len();
        println!("{:?}: {:?} bytes", entry.path(), size);

        // TODO: streaming reads here? as some files could be GB in size...
        let filedata = fs::read(entry.path()).unwrap();
        let filetype = wiz
            .get_filetype(&filedata, size)
            .unwrap_or_else(|_| "other".into());
        // dbg!(&filetype);
        let mh = Code::Sha2_512.digest(&filedata);

        // e2 is a reference to the entry in the hashmap ...
        let e2 = map_files.entry(mh.to_bytes()).or_insert(Entry {
            filetype,
            paths: vec![],
            size,
            hash: mh.to_bytes(),
            enc: None,
            tags: vec![],
            notes: None,
        });
        // ... so when it gets modified here, it is updated in the hashmap
        // TODO: fix this, serialize correctly
        e2.paths.push(entry.path().display().to_string());

        // dbg!(&e2);
        // println!("========================================================================");
    }

    // now go back to previous state
    // env::set_current_dir(current_dir)?;

    Ok(map_files)
}

#[cfg(test)]
mod test {
    const TEST_DIR_T0: &str = "test/t0/";
    // const TEST_DIR_T1: &str = "test/t1/";
    // const TEST_DIR_T2: &str = "test/t2/";

    #[test]
    fn index() {
        let map_files = super::index(TEST_DIR_T0).unwrap();
        // dbg!(&map_files);
        let art1_hash = hex::decode("1340dd4ce38ee6f793c6b294ec89093c37643e51d1f14afe31066313462f1940054cdc498e9e5cbbce02b836f6b80e9995ffa82af9a8a38845abb41ffb5d233187a6").unwrap();
        let entry = map_files.get(&art1_hash).unwrap();

        assert_eq!(
            super::Entry {
                paths: vec![
                    "test/t0/art1_dup_en.txt".to_string(),
                    "test/t0/article1_en.txt".to_string()
                ],
                filetype: "ASCII text".to_string(),
                size: 171,
                hash: art1_hash,
                enc: None,
                tags: vec![],
                notes: None,
            },
            *entry
        );
    }

    #[test]
    fn encrypt_decrypt() {
        let bbox = crate::age::BlackBox::new(&vec![crate::config::test::TEST_AGE_SECRET_KEY]);
        let data: [u8; 5] = [0x64, 0xff, 0xcd, 0xbf, 0xbb];

        let encrypted = bbox.encrypt(&data).unwrap();
        // dbg!(&encrypted);

        let decrypted = bbox.decrypt(&encrypted).unwrap();
        // dbg!(&decrypted);
        assert_eq!(decrypted, &data[..]);
    }
}
