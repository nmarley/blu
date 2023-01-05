mod add;
mod init;
mod restore;

pub use add::add;
pub use init::init;
pub use restore::restore;

// pub fn add() {
//     println!("this runs add");
// }
// pub fn init() {
//     println!("this runs init");
// }
// pub fn restore() {
//     println!("this runs restore");
// }

use crate::age::BlackBox;
use crate::config::Config;

pub fn list_tags(cfg: &Config, bbox: &BlackBox) {
    // open the tagger index + list the tags. Could ostensibly sort them?
    let tag_index = cfg.load_tag_index(bbox).unwrap().unwrap();
    for tag in tag_index.list_all_tags() {
        println!("{}", tag);
    }
}
