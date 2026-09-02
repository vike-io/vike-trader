//! `load_and_install_user_indicators` — the ONE call a binary makes at startup.
//!
//! ⚠ **Its own test binary, for the reason `install.rs` is:** the installed set is a `OnceLock`, so
//! the whole file shares ONE install inside a single `#[test]`. Two `#[test]`s in one binary run
//! concurrently and a once-per-process call cannot be exercised twice; a sibling file would be
//! a different process and so cannot see this one's install at all.
//!
//! # What each assertion would do WITHOUT the function under test
//!
//! Every one of them is a claim about a link in the chain a binary depends on, and each fails on
//! its own if that link is missing — which is why they are asserted separately rather than as one
//! "it worked" check:
//!
//! * the strategy call: if the helper LOADED but never INSTALLED, `my_triple()` resolves to nothing
//!   and the broker records no order (the exact "compiles, never trades" failure the load side
//!   exists to make visible);
//! * the `indicators/` join: the file is written to `<user_data>/indicators/`, so a helper that
//!   scanned the directory it was HANDED would load nothing and every assertion below it fails;
//! * the diagnostics: a broken sibling file must come back as a LINE, and the good one must still
//!   install — a helper that returned `Vec::new()` on rejection, or that gave up on the whole
//!   directory, fails one of those two halves.

use std::path::{Path, PathBuf};

use vike_model::{Bar, Broker, Strategy};
use vike_script::RhaiStrategy;

#[derive(Default)]
struct MockBroker {
    bars: Vec<Bar>,
    submits: Vec<(i32, f64)>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}

fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

/// A throwaway `<user_data>` directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-load-install-{tag}-{nanos}"));
        std::fs::create_dir_all(&p).expect("scratch");
        Self(p)
    }
    /// Write `<user_data>/indicators/<name>.rhai` — the layout the helper is claimed to know.
    fn indicator(&self, name: &str, src: &str) {
        let dir = self.0.join("indicators");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.rhai")), src).unwrap();
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

fn drive(strat: &mut RhaiStrategy<MockBroker>, broker: &mut MockBroker, closes: &[f64]) {
    for &c in closes {
        broker.bars.push(bar(c));
        let b = broker.bars.last().unwrap().clone();
        strat.on_bar(broker, &b);
    }
}

#[test]
fn one_call_makes_a_users_file_callable_and_reports_what_it_refused() {
    // ── before: nothing is installed ────────────────────────────────────────────────────────────
    // Measured first so the assertion after the call cannot be vacuous. ⚠ It is a bare read, NOT a
    // helper call on an absent directory: the helper INSTALLS (an empty set is still a set), and a
    // warm-up call would consume the once-per-process slot every assertion below depends on. The
    // absent-directory case itself is `crates/vike-script/src/load.rs`'s
    // `an_absent_directory_is_a_clean_empty_report_not_an_error` — inherited, not re-asserted here.
    assert!(vike_script::installed_user_indicators().is_empty());

    // ── the real load, from a directory holding one good file and two rejects ───────────────────
    let s = Scratch::new("mixed");
    s.indicator("my_triple", "fn on_bar(bar) { bar.close * 3.0 }");
    s.indicator("my_broken", "fn on_bar(bar) { this. }");
    // A name that would SHADOW a built-in: refused for its name, never compiled.
    s.indicator("sma", "fn on_bar(bar) { bar.close }");

    let lines = vike_script::load_and_install_user_indicators(s.path());

    // Both rejects are reported, each naming its own file so the author knows which to open.
    assert!(
        lines.iter().any(|l| l.contains("my_broken.rhai") && l.contains("did not compile")),
        "the compile failure must come back as a line: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("sma.rhai") && l.contains("built-in indicator")),
        "the shadowing name must come back as a line: {lines:?}"
    );
    // ...and NOTHING about the file that loaded cleanly.
    assert!(!lines.iter().any(|l| l.contains("my_triple")), "{lines:?}");

    // ── the good one is INSTALLED, not merely loaded ─────────────────────────────────────────────
    // ⚠ This is the assertion that separates this helper from `load_user_indicators`: a version
    // that returned the report and forgot to install would satisfy every diagnostic check above.
    assert_eq!(vike_script::installed_user_indicators(), vec!["my_triple"]);

    // ...and a PLAIN `compile` — the path `harness::run_backtest` and the Studio runners take,
    // neither of which threads an indicator set anywhere — resolves the name and trades on it.
    let mut strat =
        RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(my_triple()) }").unwrap();
    let mut broker = MockBroker::default();
    drive(&mut strat, &mut broker, &[5.0, 6.0]);
    assert_eq!(
        broker.submits,
        vec![(1, 15.0), (1, 18.0)],
        "the user's own indicator must be callable from a script that passed through no seam"
    );

    // ── a SECOND call comes back as a LINE, never a panic and never a silent no-op ───────────────
    // The helper's contract is that it cannot fail; the once-per-process refusal is data like any
    // other rejection, because its consequence is the same one the caller is about to print.
    let again = Scratch::new("second");
    again.indicator("too_late", "fn on_bar(bar) { 1.0 }");
    let lines = vike_script::load_and_install_user_indicators(again.path());
    assert!(
        lines.iter().any(|l| l.contains("already installed")),
        "a second install must be reported: {lines:?}"
    );
    assert_eq!(
        vike_script::installed_user_indicators(),
        vec!["my_triple"],
        "the refused call must not have changed the set"
    );
}
