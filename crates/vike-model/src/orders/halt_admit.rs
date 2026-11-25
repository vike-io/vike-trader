//! [`HaltAdmit`] — what a HALT sentinel is allowed to let OUT, and [`HaltVerify`], the answer a venue's
//! row gives to where that question can honestly be answered. The per-venue table itself is
//! `crates/vike-venues/src/venue_halt_verify.rs`'s `halt_verify_support` (decision 0162). No Python twin; a
//! Rust-native operational safeguard.
//!
//! The HALT file sentinel stops orders that OPEN risk and admits orders that CLOSE it
//! (`crates/vike-boot/src/halt.rs`'s module doc is the authority on the sentinel itself).
//! Until this type existed, "admits orders that close it" was **one boolean the caller asserts** —
//! `request.reduce_only` — which the adapter cannot check, so a strategy bug that tags its entries
//! `reduce_only` puts orders on the wire through an engaged HALT file.
//!
//! ## The two modes
//!
//! * [`HaltAdmit::Admit`] — the DEFAULT, and byte-identical to the behaviour that shipped before
//!   this type: trust the flag. Every venue.
//! * [`HaltAdmit::Verify`] — check the venue's OWN position book before admitting. Only meaningful
//!   where such a book exists; everywhere else it degrades to `Admit`, and the venue table's
//!   `halt_verify_support` is the written per-venue row saying which, so the degrade is REPORTED at
//!   mount rather than discovered mid-incident.
//!
//! ⚠ **`Verify` is a strictly weaker guarantee than the word suggests.** It refuses only what a
//! POSITIVE venue report PROVES opens risk; every absence — never fetched, fetch failed, an answer
//! that could not be read in full or was for another account, poisoned lock, **and a book holding
//! no position in the symbol at all** — ADMITS. That is deliberate and load-bearing:
//! `docs/ops/kill-switches.md`'s first promise is "Halting cannot trap you", and a mode that
//! refused a genuine exit because its book happened to be empty — after a restart, or because the
//! venue's own answer omitted the position — would break it. `crates/vike-execution/src/halt.rs`'s
//! `PositionEvidence` carries the three-state evidence that makes that distinction representable,
//! and that module's doc is the SINGLE AUTHORITY for the sentence above — this page cites it rather
//! than restating it.
//!
//! ## ⚠ There is deliberately no third mode
//!
//! A `Refuse` mode (no exemption at all) is NOT offered. That was the behaviour until 2026-08-06
//! and it was removed because it TRAPS you: halting denied `reduce_only` too, so the panic button
//! was disarmed in exactly the situations that reach a halt on their own (a fold panic, the
//! dead-man's switch). Re-adding it as one TOML line — in a file nobody re-reads mid-incident —
//! would make a documented harm one edit away. The design also makes it unreachable by accident:
//! `Verify` degrades TOWARD `Admit` on every unknown, never toward refusal.

use serde::{Deserialize, Serialize};

/// The halt-admit POLICY: how much evidence a HALT sentinel demands before letting a submit out.
///
/// Lives in `vike-model` rather than beside the predicate because it is carried by
/// `vike_config::Policy` AND consumed by `vike_execution::halt` — this is the one crate below both, and
/// the same argument that moved `halt_admits_submit` itself down into vike-execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HaltAdmit {
    /// **The default.** Admit a submit that asserts `reduce_only`, whatever the position book says
    /// — including on venues that have one. Byte-identical to the pre-policy behaviour on all
    /// fourteen roster venues.
    #[default]
    Admit,
    /// Admit a `reduce_only` submit unless the venue's own position book PROVES it opens risk —
    /// which requires a POSITIVE report about that very instrument, never an absence of one.
    /// Degrades to [`Self::Admit`], reported at mount, on every venue whose adapter holds no book —
    /// see `crates/vike-venues/src/venue_halt_verify.rs`'s `halt_verify_support`.
    Verify,
}

impl HaltAdmit {
    /// The `policy.halt_admit` row's spelling, for log lines and `config show`-style dumps.
    pub const fn as_str(self) -> &'static str {
        match self {
            HaltAdmit::Admit => "admit",
            HaltAdmit::Verify => "verify",
        }
    }
}

/// Whether [`HaltAdmit::Verify`] can do anything at a venue, and — when it cannot — the reason an
/// operator is told at mount: what a row of `crates/vike-venues/src/venue_halt_verify.rs`'s
/// `halt_verify_support` answers. ⚠ A fact about the ADAPTER, not about a MOUNT ([`halt_admit_arming`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltVerify {
    /// The venue's exec adapter holds a position book the halt boundary can read, so `Verify` is a
    /// real, tightening mode there.
    Real,
    /// The venue's exec adapter has no position book at that boundary, so `Verify` would answer
    /// `Unknown` to every question — i.e. exactly `Admit`. Carries the reason, which is LOGGED at
    /// mount so "I set verify" and "verify is doing anything here" cannot be confused.
    Degrades(&'static str),
}

/// The mode ACTUALLY in force where the venue's halt boundary has `support` (the venue's row,
/// `crates/vike-venues/src/venue_halt_verify.rs`'s `halt_verify_support`), plus the degrade reason
/// when it is not what was asked for. `None` in the second slot means the requested mode is what runs.
///
/// The caller REPORTS the reason (this crate logs nothing). It must do so at MOUNT, once — not at
/// the first halted submit: an operator who set `verify` believes it is armed, and the whole point
/// of `crates/vike-boot/src/halt.rs`'s `halt_path_arming_error` probe was that a switch
/// which is not armed looks exactly like one that is, right up until it is needed.
pub fn effective_halt_admit(
    requested: HaltAdmit,
    support: HaltVerify,
) -> (HaltAdmit, Option<&'static str>) {
    match (requested, support) {
        (HaltAdmit::Admit, _) => (HaltAdmit::Admit, None),
        (HaltAdmit::Verify, HaltVerify::Real) => (HaltAdmit::Verify, None),
        (HaltAdmit::Verify, HaltVerify::Degrades(why)) => (HaltAdmit::Admit, Some(why)),
    }
}

/// What a mount owes the operator about `halt_admit`, once the mount OUTCOME is known — the second
/// half of the report, and a separate question from [`effective_halt_admit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingHaltAdmit {
    /// Say NOTHING. The default mode adds no log line anywhere; that is what makes "byte-identical
    /// with no `policy` rows" true in the trace file and not only on the wire.
    Silent,
    /// `verify` is genuinely in force: this venue's LIVE exec client holds the position book its
    /// halt boundary reads.
    Armed,
    /// `verify` was requested, the venue's ADAPTER can honour it, and this mount landed on the paper
    /// client anyway — absent credentials, or a failed handshake. The paper client holds no position
    /// book, so nothing is verified this session, and the operator believes otherwise unless told.
    NotRunning,
}

/// Does this mount owe the operator a line about `halt_admit`, and which one?
///
/// ⚠ **This is a fact about the MOUNT and cannot be answered by the venue table's
/// `halt_verify_support` ([`HaltVerify`]), which is a fact about the ADAPTER.** `vike_tradehub_core::make_engine` resolves the mode at the TOP of the function —
/// before credentials, before cTrader's synchronous handshake — and either of those can put
/// `vike_paper::ExecutionClientPaper` at the venue instead. Announcing "verify is ARMED" from up
/// there was a claim the mount could not keep: under `verify` a paper mount is not merely
/// un-tightened, it is the exact shape of the original defect — the operator believes a check is
/// running that is not. So the paper case is [`ArmingHaltAdmit::NotRunning`] (a `warn`) rather than silence.
///
/// A pure function rather than three `tracing` calls inline, so the DECISION is unit-testable: a
/// log-line assertion needs a subscriber harness, and the thing worth pinning here is which of the
/// three answers a (mode, outcome) pair produces.
pub const fn halt_admit_arming(effective: HaltAdmit, live: bool) -> ArmingHaltAdmit {
    match (effective, live) {
        (HaltAdmit::Admit, _) => ArmingHaltAdmit::Silent,
        (HaltAdmit::Verify, true) => ArmingHaltAdmit::Armed,
        (HaltAdmit::Verify, false) => ArmingHaltAdmit::NotRunning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A degrade reason, as a venue row carries one.
    const WHY: &str = "its exec client holds no position book at its submit boundary";

    /// The default IS today's behaviour. Stated as its own test because every byte-identical claim
    /// downstream rests on it.
    #[test]
    fn the_default_is_admit() {
        assert_eq!(HaltAdmit::default(), HaltAdmit::Admit);
        assert_eq!(HaltAdmit::default().as_str(), "admit");
    }

    /// `Admit` is never degraded and never annotated — asking for the default must not produce a
    /// mount-time log line on any venue, or the byte-identical claim is false in the log. (Every
    /// roster venue's row is pinned by `crates/vike-venues/src/venue_halt_verify.rs`'s
    /// `the_halt_verify_matrix_is_pinned_verbatim`.)
    #[test]
    fn admit_is_in_force_everywhere_with_nothing_to_report() {
        for support in [HaltVerify::Real, HaltVerify::Degrades(WHY)] {
            assert_eq!(effective_halt_admit(HaltAdmit::Admit, support), (HaltAdmit::Admit, None));
        }
    }

    /// `Verify` survives exactly where a book exists, and degrades WITH THE ROW'S REASON everywhere else.
    #[test]
    fn verify_survives_only_where_a_book_exists() {
        assert_eq!(
            effective_halt_admit(HaltAdmit::Verify, HaltVerify::Real),
            (HaltAdmit::Verify, None),
            "a venue with a book must not be reported as degraded"
        );
        assert_eq!(
            effective_halt_admit(HaltAdmit::Verify, HaltVerify::Degrades(WHY)),
            (HaltAdmit::Admit, Some(WHY)),
            "a venue without a book must degrade, saying the row's own reason"
        );
    }

    /// The row spelling round-trips — the `policy.halt_admit` row holds `"verify"`, and
    /// `config show` renders it back as the same word.
    #[test]
    fn the_toml_spelling_round_trips() {
        for (mode, word) in [(HaltAdmit::Admit, "admit"), (HaltAdmit::Verify, "verify")] {
            let json = serde_json::to_string(&mode).expect("serialize");
            assert_eq!(json, format!("\"{word}\""));
            let back: HaltAdmit = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, mode);
            assert_eq!(mode.as_str(), word);
        }
        // A capitalized or unknown spelling is REFUSED rather than silently defaulted — a policy
        // file that says `Verify` must fail loudly, not load as `admit`.
        assert!(serde_json::from_str::<HaltAdmit>("\"Verify\"").is_err());
        assert!(serde_json::from_str::<HaltAdmit>("\"refuse\"").is_err());
    }

    /// The mount-outcome half, all four inputs. ⚠ **`(Verify, live = false)` is the row that
    /// matters**: a venue whose adapter CAN verify but that mounted the paper client verifies
    /// nothing, and the operator must be told — the first version of this reporting announced
    /// `verify is ARMED` from the top of `make_engine`, before credentials and before cTrader's
    /// blocking handshake, so it said that about venues that had mounted PAPER. Collapse this arm
    /// into `Armed` (or into `Silent`) and this test is what goes red. ⚠ **MEASURED** (the CI box,
    /// 2026-08-08): with that arm returning `Armed`, this module then ran `8 tests: 6 passed, 2
    /// failed` — this one and [`adapter_support_is_not_mount_arming`].
    #[test]
    fn the_mount_outcome_decides_what_the_operator_is_told() {
        assert_eq!(halt_admit_arming(HaltAdmit::Admit, true), ArmingHaltAdmit::Silent);
        assert_eq!(
            halt_admit_arming(HaltAdmit::Admit, false),
            ArmingHaltAdmit::Silent,
            "the DEFAULT mode must add no line on ANY mount outcome"
        );
        assert_eq!(halt_admit_arming(HaltAdmit::Verify, true), ArmingHaltAdmit::Armed);
        assert_eq!(
            halt_admit_arming(HaltAdmit::Verify, false),
            ArmingHaltAdmit::NotRunning,
            "a paper mount holds no position book, so `verify` armed NOTHING — saying it did is \
             the defect this whole knob exists to avoid, one layer up"
        );
    }

    /// …and the ADAPTER table cannot answer that question, which is why there are two functions.
    /// cTrader's row is `Real` whatever the mount did with it.
    #[test]
    fn adapter_support_is_not_mount_arming() {
        assert_eq!(
            effective_halt_admit(HaltAdmit::Verify, HaltVerify::Real),
            (HaltAdmit::Verify, None)
        );
        assert_eq!(
            halt_admit_arming(HaltAdmit::Verify, false),
            ArmingHaltAdmit::NotRunning,
            "`Real` + a paper mount is a real, reportable state — treating `Real` as `armed` is \
             the confusion this pair separates"
        );
    }
}
