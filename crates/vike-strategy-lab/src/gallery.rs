//! Every strategy file that opens with a licence header of its own (a rewrite of a published script: the strategy
//! gallery's rules, decision 0269 item 4, plan 2b unit C3; the set-B rows imported since, `looks_into_the_future/`
//! among them), one module each by `#[path]`: CI compiles every one here, and the strategy lab runs them from here.
//! The list is WRITTEN AT BUILD TIME from the files' folder (`build.rs`, from
//! `crates/vike-strategy-lab/build/gallery_list.rs`'s `gallery_files`): a strategy file is added, removed or edited
//! where it is, never through this file.

use vike_model::params::Params;
use vike_tactics::Rule;

/// A gallery-born strategy file: its path under `strategies/` and its `rule` entry.
#[derive(Debug, Clone, Copy)]
pub struct GalleryFileEntry {
    pub path: &'static str,
    pub rule: fn(&Params) -> Box<dyn Rule + Send>,
}

// The `#[path]` module of every gallery-born file, then `FILES`: every one's path and `rule`, in path order.
include!(concat!(env!("OUT_DIR"), "/gallery_files.rs"));
