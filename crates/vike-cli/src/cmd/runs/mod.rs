//! The STORED-RUN half of the backtest plane: `vike-cli backtest ls | show | path | tag | diff |
//! gate`.
//!
//! **Artifact-only — no engine, no store, no socket** (spec §6/§7). These work on a laptop with
//! neither, on runs minted months ago, remotely, or by someone else. The only inputs are
//! `<project>/user_data/runs/` and the marks store beside it, and the only schema is the COMMON
//! manifest `vike_model::runs` owns.
//!
//! # ⚠ One of the six WRITES, and it writes nothing a run depends on
//!
//! `tag` is the exception to "reading half", and it is deliberately a small one: it appends to a
//! run's OPTIONAL sidecar (`vike_model::runs::META_FILE`) and to the marks store, and touches
//! neither the manifest nor the report. Nothing it writes can make a run unreadable, which is what
//! lets it sit in this family rather than beside the engine.
//!
//! # The three JUDGING verbs, as a family
//!
//! [`tag`] gives a run a stable second name, [`diff`] shows what moved between two runs on both
//! sides of the arrow — inputs beside outputs — and [`gate`] turns a comparison into an EXIT CODE a
//! CI step branches on. They are one family because they share one operand grammar
//! ([`selector`]) and one pair of documents (`manifest.json` and `report.json`); a verb of the three
//! that accepted a different form in the same position is the fragmentation §15.6 forbids.
//!
//! # `--json`, and what a FAILURE looks like under it
//!
//! The document is emitted on SUCCESS only. A failure is a sentence on stderr plus a rung of
//! [`crate::exit`], and stdout carries nothing at all — the shape every sibling in this crate has
//! (`crate::cmd::data`'s `list_json`, `crate::cmd::secrets`'s `list`, `crate::cmd::init`). It is
//! deliberately NOT an `{"ok": false}` document.
//!
//! [`scan::RunScan::problems`] go to **stderr** in both modes, for the same reason: one unreadable
//! run directory must not make a machine-readable listing unparseable.

use std::path::Path;

use vike_datahub_client::NodeKeys;

pub(crate) mod diff;
pub(crate) mod failif;
pub(crate) mod gate;
pub(crate) mod jsondoc;
pub(crate) mod ls;
pub(crate) mod path;
pub(crate) mod scan;
pub(crate) mod selector;
pub(crate) mod show;
pub(crate) mod tag;
pub(crate) mod where_expr;

/// Everything the reading verbs need, resolved by the dispatcher and handed down whole.
///
/// It is a STRUCT rather than four parameters because five sub-verbs take the same set and the
/// dispatcher resolves all of it in one place — the rule
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchets down: a `src/cmd/` file
/// may not resolve a project, read an environment variable or open the credential store for itself.
pub(crate) struct Ctx<'a> {
    /// `<project>/user_data/runs`, already `$VIKE_USER_DATA_DIR`-aware. `None` when there is no
    /// project above the working directory at all — an ordinary state, refused per verb with a
    /// sentence rather than assumed away.
    pub(crate) runs_root: Option<&'a Path>,
    /// `<project>/user_data/marks`, resolved beside [`Self::runs_root`] and never under it.
    ///
    /// ⚠ A SIBLING, because a `marks/` directory inside the runs root is — to [`scan::scan_runs`]
    /// and to `crates/vike-studio-core/src/listing.rs`'s `list_runs` alike — a run directory holding
    /// no manifest, i.e. a permanent "this run never finished writing" row in every listing.
    /// `vike_model::state_path::MARKS_SUBDIR` carries the argument.
    ///
    /// `None` for the same reason `runs_root` is: no project above the working directory. A mark
    /// selector then refuses by saying there is nowhere to keep marks, which is a different fact
    /// from a mark that was never set.
    pub(crate) marks_root: Option<&'a Path>,
    /// `config.backtest_addr`, the MIDDLE rung of the compute-daemon address ladder. Only
    /// [`strategies`](crate::cmd::strategies) dials anything; every other reading verb is
    /// artifact-only.
    pub(crate) configured_addr: Option<&'a str>,
    /// The vike-datahub node keys, when the credential store holds a pair. A key-less dial is legal
    /// for an `Observe`-scope verb and is what an unconfigured laptop does.
    pub(crate) keys: Option<&'a NodeKeys>,
}
