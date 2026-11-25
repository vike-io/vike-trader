//! **Flow step 2 — Studio asks for a build, THROUGH THE BACKTESTHUB.** The Studio's half of
//! `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`'s hand-off:
//!
//! ```text
//! Studio ──file + text──> backtesthub ──forwards──> builder ──writes──> <builds>/<folder>/<sha256 of the text>.so
//! Studio ──the file─────> backtesthub ──reads the file, its sha names the build
//! ```
//!
//! Since decision 0288's amendment of 2026-11-16 (the owner's option A) Studio dials the strategy builder
//! NOT AT ALL: the builder's key is a daemon key no person holds, and the backtesthub — which already
//! checks the person against the one access list, and which Studio already dials — forwards the Build
//! (`BuildStrategyFile`, Control scope). So a Build is a backtesthub dispatch like a Run, signed with
//! this PC's device key at the [`crate::StudioRunBackend`]'s address: the sibling
//! `strategy_files`'s `spawn_backtesthub_build`. This module holds what is refused before that dial.
//!
//! # Ordering is the contract, and it lives in the caller
//!
//! *"Studio's ordering is strictly sequential: await `Ok(sha)` from the build, then send Run."*
//! That is enforced by `StudioState`, not here: `rust_sha` is `None` until a build ANSWERS, and
//! `StudioState::run_blocked_reason` refuses Run/Sweep/Walk-Forward while it is — before any
//! request is built. The dispatch hands back a `Receiver` rather than a value: the wait is a
//! compile, so it cannot happen on the frame thread.

/// Why a Build of `source` as the file `file` is refused BEFORE anything is dialled, or `None`. The
/// builder judges the path itself (a plain relative `.rs` path); a missing key is the backtesthub's
/// refusal, which says how this PC's device gets on the server's list.
pub(crate) fn refusal(file: &str, source: &str) -> Option<String> {
    if file.trim().is_empty() {
        return Some(
            "name the strategy file before building it (for example `my/strategy.rs`) — the \
             builder builds a file, so a build with no path has none."
                .to_string(),
        );
    }
    if source.trim().is_empty() {
        return Some("there is nothing to build — the editor buffer is empty.".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build with no file path is refused before the dial: the builder builds a FILE.
    #[test]
    fn an_unnamed_strategy_file_is_refused_before_the_dial() {
        let err = refusal("   ", "fn main() {}").expect("no name");
        assert!(err.contains("name the strategy file"), "{err}");
    }

    /// An empty buffer is refused too — a build of nothing wastes a compile and answers with a
    /// sha for an artifact nobody meant. A named file with text is not refused here.
    #[test]
    fn an_empty_editor_buffer_is_refused_before_the_dial() {
        let err = refusal("my_strat.rs", "  \n ").expect("no source");
        assert!(err.contains("nothing to build"), "{err}");
        assert_eq!(refusal("my_strat.rs", "fn main() {}"), None);
    }

    /// The refusal reaches the caller through the real spawn, and nothing is dialled: the address
    /// is one nothing listens on, so a dial would answer a connect error instead.
    #[test]
    fn the_spawn_refuses_before_it_dials() {
        let err = crate::backend::backtesthub::strategy_files::spawn_backtesthub_build(
            "127.0.0.1:1".to_string(),
            None,
            "my_strat.rs".to_string(),
            String::new(),
            None,
        )
        .recv()
        .expect("the worker answered")
        .expect_err("an empty buffer");
        assert!(err.contains("nothing to build"), "{err}");
    }
}
