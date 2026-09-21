//! `load_and_install_user_indicators` — the ONE call every composition root makes.
//!
//! ⚠ **Its own test binary, like `install.rs`, because the installed set is a `OnceLock`.** Cargo
//! runs each `tests/*.rs` as a separate process, so the install performed here cannot leak into
//! `install.rs`'s own once-per-process assertions (or vice versa), and neither file's result
//! depends on which ran first. Everything below shares ONE install inside a single `#[test]` for
//! the same reason — two `#[test]`s in one binary run concurrently, and a once-per-process call
//! cannot be exercised twice.

use std::path::{Path, PathBuf};

/// A throwaway directory, hand-rolled rather than pulled from `tempfile`: this crate has no
/// dev-dependency on it, and the shape mirrors `load.rs`'s own `Scratch` (which lives inside a
/// `#[cfg(test)]` module and is therefore unreachable from an integration test).
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-install-from-ud-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch");
        Self(p)
    }
    fn write(&self, rel: &str, src: &str) {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, src).unwrap();
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The four claims a composition root relies on, in the one order a `OnceLock` permits.
///
/// **Non-vacuous by construction, in four independent ways** — every assertion below fails if the
/// corresponding line of `load_and_install_user_indicators` is reverted:
///
/// 1. the set is measured EMPTY first, so "installed" cannot be a pre-existing state;
/// 2. the argument is the `user_data` directory and the files live one level down under
///    `indicators/` — drop the `INDICATORS_SUBDIR` join and the loader is handed the ROOT, where its
///    own flat-layout rule reports all three files as `InSubdirectory` and installs nothing. So the
///    message count is 3 rather than 2 and the installed set is empty: caught by the
///    `messages.len()` assertion and by the installed-set assertion, NOT by an absence of messages
///    (an earlier draft of this comment claimed the latter, and it was wrong);
/// 3. a deliberately broken file must produce exactly ONE message naming it — drop the diagnostic
///    loop and the vector is empty while the good indicator still installs, so only this assertion
///    catches it;
/// 4. the SECOND call must come back with the already-installed message rather than an empty vector
///    — drop the `Err` arm and a root would silently believe it owned a set it did not install.
#[test]
fn it_joins_indicators_reports_every_rejection_and_installs_exactly_once() {
    // ── the property being changed, measured BEFORE the change ──────────────────────────────────
    assert!(
        vike_script::installed_user_indicators().is_empty(),
        "nothing may be installed before this test installs it, or every assertion below is \
         measuring somebody else's set"
    );

    let s = Scratch::new("ok");
    // The GOOD one. Under `indicators/`, not at the top level: the caller passes `user_data`, and
    // the join is this function's job (claim 2).
    s.write("indicators/doubler.rhai", "fn on_bar(bar) { bar.close * 2.0 }");
    // The BAD one — `fn on_bar` is missing entirely, so `compile_indicator` rejects it (claim 3).
    s.write("indicators/broken.rhai", "fn nope() { 1 }");
    // ...and one file the FLAT layout does not read, which is a diagnostic rather than a shrug.
    s.write("indicators/nested/buried.rhai", "fn on_bar(bar) { 1.0 }");

    let messages = vike_script::load_and_install_user_indicators(s.path());

    // The GOOD one installed despite two rejected siblings: a half-edited file must not take the
    // rest of the directory down with it.
    assert_eq!(
        vike_script::installed_user_indicators(),
        vec!["doubler"],
        "the join must reach <user_data>/indicators/ and one bad file must not block a good one"
    );

    // Exactly the two rejections, each naming its own file so the author knows what to open.
    assert_eq!(messages.len(), 2, "one message per rejected file: {messages:?}");
    assert!(
        messages.iter().all(|m| m.starts_with("indicator not loaded — ")),
        "every message says what kind of thing it is, because a root prefixes it with its own \
         binary name and adds nothing else: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("broken.rhai")),
        "the compile failure names its file: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("buried.rhai")),
        "the ignored subdirectory names its file: {messages:?}"
    );

    // ── a SECOND call is REPORTED, not silently swallowed ───────────────────────────────────────
    // Two roots in one process would each believe they owned the set; one of them is wrong.
    let again = vike_script::load_and_install_user_indicators(s.path());
    assert_eq!(again.len(), 3, "the two rejections, plus the refused install: {again:?}");
    assert!(
        again.iter().any(|m| m.contains("already installed")),
        "the refusal must reach the caller: {again:?}"
    );
    assert_eq!(
        vike_script::installed_user_indicators(),
        vec!["doubler"],
        "the refused call must not have changed the set"
    );
}

// ⚠ There is deliberately NO second `#[test]` here for the "project with no `indicators/`
// directory" case, tempting as it is. Two `#[test]`s in one binary run CONCURRENTLY, and calling
// this function is what consumes the once-per-process `OnceLock` — a sibling test pointed at an
// empty directory would install an EMPTY set and make the test above fail (or pass) depending on
// which thread won. The property it would assert already has a home one level down, where nothing
// installs anything: `crates/vike-script/src/load.rs`'s
// `an_absent_directory_is_a_clean_empty_report_not_an_error`.
