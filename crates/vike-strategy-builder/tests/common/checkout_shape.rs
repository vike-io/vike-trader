//! A scratch source tree with a checkout's shape, as the daemon accepts one (`render::source_tree_refusal`):
//! a `.git` and this checkout's cdylib templates, which every build reads from the tree it compiles
//! against (decision 0296), so a build of it reaches `cargo` (or the jail) rather than stopping at a
//! missing template.

use std::path::{Path, PathBuf};

/// Give `root` a checkout's shape: a `.git` folder and this checkout's
/// `crates/vike-rust-strategy/template/` files.
pub fn plant_checkout_shape(root: &Path) {
    std::fs::create_dir(root.join(".git")).expect("plant .git");
    let template = Path::new("crates/vike-rust-strategy/template");
    let from = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(template);
    let to = root.join(template);
    std::fs::create_dir_all(&to).expect("the template folder");
    for entry in std::fs::read_dir(&from).expect("this checkout's templates").flatten() {
        if entry.path().is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).expect("copy a template");
        }
    }
}
