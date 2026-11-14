//! **Flow step 2 — Studio asks the builder service for a build.** The Studio's half of
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`'s hand-off:
//!
//! ```text
//! Studio ──file + text──> builder ──writes──> <builds>/<folder>/<sha256 of the text>.so
//! Studio ──the file─────────────────────────> backtest server ──reads the file, its sha names the build
//! ```
//!
//! This module owns the LEFT arrow. The right one is `crate::backend::remote`'s ordinary
//! `StrategySpecWire::File`: the backtesthub reads the file and hands its sandboxed child the build its text
//! hashes to (plan 2b; no hub loads an artifact by a sha it is sent). **Nothing here tells the
//! backtest server that a builder exists**, which is the
//! property the design is explicit about — a server that could ask for a build, or wait for an
//! artifact to appear, would have that knowledge.
//!
//! # Ordering is the contract, and it lives in the caller
//!
//! *"Studio's ordering is strictly sequential: await `Ok(sha)` from the builder, then send Run."*
//! That is enforced by `StudioState`, not here: `rust_sha` is `None` until a build ANSWERS, and
//! `StudioState::run_blocked_reason` refuses Run/Sweep/Walk-Forward while it is — before any
//! request is built. This module's only
//! contribution to the ordering is that it hands back a `Receiver` rather than a value: the wait
//! is a compile, so it cannot happen on the frame thread, and a `Receiver` is the shape every
//! other long call in this shell already takes.
//!
//! # Why a module beside `remote.rs` rather than inside it
//!
//! `remote.rs` is the client of ONE daemon (`vike-backend backtesthub`) speaking ONE protocol. The
//! builder is a THIRD service with its own domain separator, its own protocol version and its own
//! key (`vike_strategy_builder::builder`'s Decision 1 argues why it may not reuse either
//! sibling's). Folding a second protocol into that file would make "which daemon does this dial"
//! a question you answer by reading, and the address/key confusion that file's own tombstones
//! record is exactly what comes of that.

use std::sync::mpsc::Receiver;

use vike_hub_protocol::auth::HubKeys;
use vike_strategy_builder::builder::{BUILDER_KEY_NAME, set_key_advice};
use vike_strategy_builder::client;
use vike_studio_core::spawn_outcome;

/// The `toolchain_fp` this client sends, and why it is EMPTY.
///
/// `vike_strategy_builder::builder::Request::Build`'s own doc records that the field rides the
/// wire and is checked against nothing yet. The value that would eventually be right is the
/// BACKTEST SERVER's fingerprint — the host that will `dlopen` the artifact — and this process
/// runs on the operator's PC under a different toolchain, profile and target, so sending this
/// binary's own `vike_rust_strategy::fingerprint::FINGERPRINT` would be a confident wrong
/// answer the day the check lands. Empty is the honest one: this client does not know.
///
/// ⚠ When the check DOES land, the value belongs in the Run path's answer rather than here — the
/// Studio would have to learn it from the backtesthub it is about to run on, not from itself.
const UNKNOWN_TOOLCHAIN_FP: &str = "";

/// Ask the builder at `addr` to compile `source` as the strategy file `file` (its path under the
/// `strategies/` folder, `trend/momentum.rs`), off the frame thread.
///
/// The receiver yields `Ok(sha)` — the design's Flow step 3/4 answer, the sha that then travels
/// with the Run — or the failure as one rendered sentence. `Err` is a `String` rather than the
/// typed [`vike_strategy_builder::client::BuildRequestError`] for the reason `StudioState::study_last` gives for its own error
/// side: nothing in this shell branches on the variant, the pane renders one sentence, and every
/// variant's `Display` already names what to do about it. A `Compile` failure renders rustc's own
/// diagnostics VERBATIM, which is the whole point of carrying them across the wire.
///
/// A missing key is refused HERE rather than dialled and refused there: without one this client
/// would sign a mac with empty key bytes and earn `bad mac`, which reads like a wrong key rather
/// than no key.
pub fn spawn_build(
    addr: String,
    keys: Option<HubKeys>,
    file: String,
    source: String,
) -> Receiver<Result<String, String>> {
    spawn_outcome(move || build(&addr, keys.as_ref(), &file, &source))
}

/// [`spawn_build`]'s body — BLOCKING, and legitimately for minutes (the answer is on the far side
/// of a `cargo build`). Separated so the refusal logic is testable with no socket. The builder
/// judges the path itself (a plain relative `.rs` path).
fn build(addr: &str, keys: Option<&HubKeys>, file: &str, source: &str) -> Result<String, String> {
    let file = file.trim();
    if file.is_empty() {
        return Err("name the strategy file before building it (for example \
                    `my/strategy.rs`) — the builder builds a file, so a build with no path has \
                    none."
            .to_string());
    }
    if source.trim().is_empty() {
        return Err("there is nothing to build — the editor buffer is empty.".to_string());
    }
    let Some(keys) = keys else {
        return Err(format!(
            "no strategy-builder key: this machine's settings database holds no \
             `{BUILDER_KEY_NAME}` row, so this Studio cannot ask for a build. It is that service's \
             OWN key — the same value the builder holds — and neither a datahub nor a tradehub \
             hub key can stand in for it. {}",
            set_key_advice()
        ));
    };
    client::build_remote(addr, keys, file, source, UNKNOWN_TOOLCHAIN_FP).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key: refused before a socket is opened, and the message NAMES the row and the one command
    /// that sets it. The alternative — dialling with empty key bytes — earns `bad mac`, which reads
    /// like a wrong key rather than an absent one.
    #[test]
    fn a_missing_builder_key_is_refused_by_name_with_its_command_and_never_dialled() {
        // An address nothing listens on: reaching a dial at all would be the failure this test is
        // about, and it would show up as a connect error rather than this message.
        let err = build("127.0.0.1:1", None, "my_strat.rs", "fn main() {}").expect_err("no key");
        assert!(err.contains(BUILDER_KEY_NAME), "{err}");
        assert!(err.contains(&format!("vike-cli secrets set {BUILDER_KEY_NAME}")), "{err}");
        assert!(!err.to_lowercase().contains("connect"), "it must not have dialled: {err}");
    }

    /// A build with no file path is refused before the dial: the builder builds a FILE.
    #[test]
    fn an_unnamed_strategy_file_is_refused_before_the_dial() {
        let keys = HubKeys::new(Vec::new(), b"k".to_vec());
        let err = build("127.0.0.1:1", Some(&keys), "   ", "fn main() {}").expect_err("no name");
        assert!(err.contains("name the strategy file"), "{err}");
    }

    /// An empty buffer is refused too — a build of nothing wastes a compile and answers with a
    /// sha for an artifact nobody meant.
    #[test]
    fn an_empty_editor_buffer_is_refused_before_the_dial() {
        let keys = HubKeys::new(Vec::new(), b"k".to_vec());
        let err = build("127.0.0.1:1", Some(&keys), "my_strat.rs", "  \n ").expect_err("no source");
        assert!(err.contains("nothing to build"), "{err}");
    }

    /// The default address is the BUILDER's, not a sibling daemon's — `remote.rs`'s tombstone
    /// records what a client pointed at the wrong daemon's port cost (every ▶ Run came back a
    /// wrong-plane error, and nothing reported it because the field was editable), and that is a
    /// mistake this module is one constant away from repeating.
    #[test]
    fn the_default_address_is_the_builders_own_and_not_the_backtesthubs() {
        let builder = vike_strategy_builder::client::default_address();
        assert_ne!(builder, crate::backend::remote::DEFAULT_BACKTESTHUB_ADDRESS);
        assert!(builder.starts_with("127.0.0.1:"), "{builder}");
    }
}
