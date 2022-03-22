// use serde::{Deserialize, Serialize};

// #[derive(PartialEq, Serialize, Deserialize, Clone, Hash)]
#[derive(PartialEq, Clone, Hash)]
pub struct Block {
    hash: Vec<u8>,
}

type BlockVec<'a> = Vec<&'a Block>;
// impl Hash for BlockVec { }

// #[derive(PartialEq, Serialize, Deserialize, Clone)]
#[derive(PartialEq, Clone)]
pub struct File<'a> {
    blocks: BlockVec<'a>,
    ref_count: usize,
}
