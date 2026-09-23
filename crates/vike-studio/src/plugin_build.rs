//! **Flow step 2 — Studio asks the builder service for a build.** The Studio's half of
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`'s hand-off:
//!
//! ```text
//! Studio ──source──> builder ──writes──> user_data/plugins/<name>-<sha>.so
//! Studio ──sha──────────────────────────> backtest server ──reads──> the same file
//! ```
//!
//! This module owns the LEFT arrow. The right one already exists and is untouched:
//! `crate::remote` sends the sha inside an ordinary `WireSpec::Plugin`, and the compute daemon
//! reads the file. **Nothing here tells the backtest server that a builder exists**, which is the
//! property the design is explicit about — a server that could ask for a build, or wait for an
//! artifact to appear, would have that knowledge.
//!
//! # Ordering is the contract, and it lives in the caller
//!
//! *"Studio's ordering is strictly sequential: await `Ok(sha)` from the builder, then send Run."*
//! That is enforced by `StudioState`, not here: `plugin_sha` is `None` until a build ANSWERS, and
//! `StudioState::run_blocked_reason` refuses Run/Sweep/Walk-Forward while it is — before any
//! request is built, so an empty sha never reaches `to_wire_spec`. This module's only
//! contribution to the ordering is that it hands back a `Receiver` rather than a value: the wait
//! is a compile, so it cannot happen on the frame thread, and a `Receiver` is the shape every
//! other long call in this shell already takes.
//!
//! # Why a module beside `remote.rs` rather than inside it
//!
//! `remote.rs` is the client of ONE daemon (`vike-backend backtest`) speaking ONE protocol. The
//! builder is a THIRD service with its own domain separator, its own protocol version and its own
//! key (`vike_strategy_builder::builder`'s Decision 1 argues why it may not reuse either
//! sibling's). Folding a second protocol into that file would make "which daemon does this dial"
//! a question you answer by reading, and the address/key confusion that file's own tombstones
//! record is exactly what comes of that.

use std::sync::mpsc::Receiver;

use vike_node_proto::auth::NodeKeys;
use vike_strategy_builder::builder::BUILDER_KEY_ENV;
use vike_strategy_builder::client;
use vike_studio_core::spawn_outcome;

/// The `toolchain_fp` this client sends, and why it is EMPTY.
///
/// `vike_strategy_builder::builder::Request::Build`'s own doc records that the field rides the
/// wire and is checked against nothing yet. The value that would eventually be right is the
/// BACKTEST SERVER's fingerprint — the host that will `dlopen` the artifact — and this process
/// runs on the operator's PC under a different toolchain, profile and target, so sending this
/// binary's own `vike_strategy_plugin::fingerprint::FINGERPRINT` would be a confident wrong
/// answer the day the check lands. Empty is the honest one: this client does not know.
///
/// ⚠ When the check DOES land, the value belongs in the Run path's answer rather than here — the
/// Studio would have to learn it from the compute daemon it is about to run on, not from itself.
const UNKNOWN_TOOLCHAIN_FP: &str = "";

/// Ask the builder at `addr` to compile `source` as `name`, off the frame thread.
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
    keys: Option<NodeKeys>,
    name: String,
    source: String,
) -> Receiver<Result<String, String>> {
    spawn_outcome(move || build(&addr, keys.as_ref(), &name, &source))
}

/// [`spawn_build`]'s body — BLOCKING, and legitimately for minutes (the answer is on the far side
/// of a `cargo build`). Separated so the refusal logic is testable with no socket.
fn build(addr: &str, keys: Option<&NodeKeys>, name: &str, source: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name the plugin before building it — the artifact is filed as \
                    `<name>-<sha>.so`, so an unnamed build has no file name."
            .to_string());
    }
    if source.trim().is_empty() {
        return Err("there is nothing to build — the editor buffer is empty.".to_string());
    }
    let Some(keys) = keys else {
        return Err(format!(
            "no strategy-builder key is configured, so this Studio cannot ask for a build. Set \
             {BUILDER_KEY_ENV} in this process's environment; it is that service's OWN key and \
             neither a datahub nor a tradehub node key can stand in for it."
        ));
    };
    client::build_remote(addr, keys, name, source, UNKNOWN_TOOLCHAIN_FP).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No key: refused before a socket is opened, and the message NAMES the variable. The
    /// alternative — dialling with empty key bytes — earns `bad mac`, which reads like a wrong key
    /// rather than an absent one.
    #[test]
    fn a_missing_builder_key_is_refused_by_name_and_never_dialled() {
        // An address nothing listens on: reaching a dial at all would be the failure this test is
        // about, and it would show up as a connect error rather than this message.
        let err = build("127.0.0.1:1", None, "my_strat", "fn main() {}").expect_err("no key");
        assert!(err.contains(BUILDER_KEY_ENV), "{err}");
        assert!(!err.to_lowercase().contains("connect"), "it must not have dialled: {err}");
    }

    /// An unnamed plugin is refused before the dial, because the artifact's FILE NAME is built
    /// from the name — `<name>-<sha>.so`.
    #[test]
    fn an_unnamed_plugin_is_refused_before_the_dial() {
        let keys = NodeKeys::new(Vec::new(), b"k".to_vec());
        let err = build("127.0.0.1:1", Some(&keys), "   ", "fn main() {}").expect_err("no name");
        assert!(err.contains("name the plugin"), "{err}");
    }

    /// An empty buffer is refused too — a build of nothing wastes a compile and answers with a
    /// sha for an artifact nobody meant.
    #[test]
    fn an_empty_editor_buffer_is_refused_before_the_dial() {
        let keys = NodeKeys::new(Vec::new(), b"k".to_vec());
        let err = build("127.0.0.1:1", Some(&keys), "my_strat", "  \n ").expect_err("no source");
        assert!(err.contains("nothing to build"), "{err}");
    }

    /// The default address is the BUILDER's, not a sibling daemon's — `remote.rs`'s tombstone
    /// records what a client pointed at the wrong daemon's port cost (every ▶ Run came back a
    /// wrong-plane error, and nothing reported it because the field was editable), and that is a
    /// mistake this module is one constant away from repeating.
    #[test]
    fn the_default_address_is_the_builders_own_and_not_the_compute_daemons() {
        let builder = vike_strategy_builder::client::default_addr();
        assert_ne!(builder, crate::remote::DEFAULT_COMPUTE_ADDR);
        assert!(builder.starts_with("127.0.0.1:"), "{builder}");
    }
}
