//! [`HaltAdmit`] — what a HALT sentinel is allowed to let OUT, and the per-venue table saying where
//! that question can honestly be answered. No Python twin; a Rust-native operational safeguard.
//!
//! The HALT file sentinel stops orders that OPEN risk and admits orders that CLOSE it
//! (`crates/vike-bridge-core/src/halt.rs`'s module doc is the authority on the sentinel itself).
//! Until this type existed, "admits orders that close it" was **one boolean the caller asserts** —
//! `request.reduce_only` — which the adapter cannot check, so a strategy bug that tags its entries
//! `reduce_only` puts orders on the wire through an engaged HALT file.
//!
//! ## The two modes
//!
//! * [`HaltAdmit::Admit`] — the DEFAULT, and byte-identical to the behaviour that shipped before
//!   this type: trust the flag. Every venue.
//! * [`HaltAdmit::Verify`] — check the venue's OWN position book before admitting. Only meaningful
//!   where such a book exists; everywhere else it degrades to `Admit`, and [`halt_verify_support`]
//!   is the written per-venue row saying which, so the degrade is REPORTED at mount rather than
//!   discovered mid-incident.
//!
//! ⚠ **`Verify` is a strictly weaker guarantee than the word suggests.** It refuses only what a
//! POSITIVE venue report PROVES opens risk; every absence — never fetched, fetch failed, an answer
//! that could not be read in full or was for another account, poisoned lock, **and a book holding
//! no position in the symbol at all** — ADMITS. That is deliberate and load-bearing:
//! `docs/ops/kill-switches.md`'s first promise is "Halting cannot trap you", and a mode that
//! refused a genuine exit because its book happened to be empty — after a restart, or because the
//! venue's own answer omitted the position — would break it. `crates/vike-exec/src/halt.rs`'s
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
/// `vike_config::Policy` AND consumed by `vike_exec::halt` — this is the one crate below both, and
/// the same argument that moved `halt_admits_submit` itself down into vike-exec.
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
    /// see [`halt_verify_support`].
    Verify,
}

impl HaltAdmit {
    /// The `policy.toml` spelling, for log lines and `config show`-style dumps.
    pub const fn as_str(self) -> &'static str {
        match self {
            HaltAdmit::Admit => "admit",
            HaltAdmit::Verify => "verify",
        }
    }
}

/// Whether [`HaltAdmit::Verify`] can do anything at a venue, and — when it cannot — the reason an
/// operator is told at mount.
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

impl HaltVerify {
    /// `true` when `Verify` genuinely tightens this venue.
    pub const fn is_real(self) -> bool {
        matches!(self, HaltVerify::Real)
    }
}

/// Can [`HaltAdmit::Verify`] verify anything at `venue`? One NAMED row per `crate::VENUES` entry,
/// even where the answer equals the fallback — the named row is the declaration that the venue was
/// CLASSIFIED, not forgotten (the per-venue capability-table contract in `crate::venues`' module
/// doc).
///
/// The reasons are not decoration: each is the exact sentence an operator sees when they set
/// `verify` and the venue cannot honour it.
///
/// ⚠ **Only ONE venue answers [`HaltVerify::Real`] today, and that is the honest answer, not a
/// stub.** Exactly two bridge adapters in this tree hold a position book of their own, and only one
/// of them holds it at a halt boundary:
///
/// * **cTrader** does — `crates/bridges/ctrader/src/conn.rs`'s `PositionMap`, seeded at connect and
///   maintained from every execution event, read by `crates/bridges/ctrader/src/positions.rs`'s
///   `opposing_available`.
/// * **Polymarket** does NOT, on the exec path: its only position source is a blocking geo-proxied
///   HTTP fetch on the settlement/recon side (`crates/bridges/polymarket/src/recon_client.rs`'s
///   `fetch_position_status_reports`), account-wide and known to lag settlement — so a `verify`
///   there would mean a synchronous proxied round trip inside `submit`, against an oracle whose own
///   module doc records it missing redemptions entirely.
///
/// ⚠ **This answers a question about the ADAPTER, not about a MOUNT.** [`HaltVerify::Real`] means
/// "this venue's live exec client holds a position book at its halt boundary" — it does not mean the
/// venue in front of you mounted that client. Absent credentials, or (on cTrader) a failed
/// handshake, put `vike_paper::PaperExecutionClient` there instead, and it holds no book either, so
/// `verify` verifies nothing that session. That is a mount-time fact this table cannot see;
/// `vike_mount::make_engine` reports it from the arm, where the outcome is known. A caller that
/// treats `Real` as "armed" is making the same mistake this whole type exists to prevent, one layer
/// up.
pub fn halt_verify_support(venue: &str) -> HaltVerify {
    /// The reason shared by every venue whose exec client is the venue-neutral command actor: that
    /// boundary owns a channel, an event lane and a sentinel path, and nothing that could say how
    /// big any position is (`crates/vike-bridge-core/src/exec_actor.rs`'s `halt_engaged`).
    const NO_BOOK_ON_THE_SHARED_ACTOR: &str =
        "its exec client is the shared ExecActor, whose submit boundary holds no position book";
    match venue {
        // ── the one venue that can ───────────────────────────────────────────────────────────
        "ctrader" => HaltVerify::Real,
        // ── the shared-ExecActor family ──────────────────────────────────────────────────────
        "binance" | "bybit" | "okx" | "deribit" | "oanda" | "ig" | "alpaca" | "aster" | "ibkr" => {
            HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR)
        }
        "fxcm" => HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR),
        "polymarket" => HaltVerify::Degrades(
            "its exec path holds no position book at all — the venue's only position source is a \
             blocking proxied HTTP fetch on the settlement side, which also misses redemptions",
        ),
        // ── bespoke halt copy, still no book ─────────────────────────────────────────────────
        "hyperliquid" => {
            HaltVerify::Degrades("its bespoke halt boundary (reject_halted) holds no position book")
        }
        // ── no halt boundary at all today ────────────────────────────────────────────────────
        "dukascopy" => HaltVerify::Degrades(
            "it has no halt boundary and no live mount arm; its position state lives in the Java \
             sidecar",
        ),
        // vike:new-venue:row // TODO(new-venue: {venue}): does this venue's halt boundary hold a position book? If it uses
        // vike:new-venue:row // the shared ExecActor, say so with NO_BOOK_ON_THE_SHARED_ACTOR; if it has a bespoke
        // vike:new-venue:row // boundary, write the reason in that venue's own terms (>30 chars — an operator must be
        // vike:new-venue:row // able to act on it, per `every_degrade_reason_is_an_argument`).
        // vike:new-venue:row "{venue}" => HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR),
        // A venue string this table has never heard of. Conservative in the only direction that
        // cannot trap anyone: no verification is claimed.
        _ => HaltVerify::Degrades("unknown venue — no position book is claimed for it"),
    }
}

/// The mode ACTUALLY in force at `venue`, plus the degrade reason when it is not what was asked
/// for. `None` in the second slot means the requested mode is what runs.
///
/// The caller REPORTS the reason (this crate logs nothing). It must do so at MOUNT, once — not at
/// the first halted submit: an operator who set `verify` believes it is armed, and the whole point
/// of `crates/vike-bridge-core/src/halt.rs`'s `halt_path_arming_error` probe was that a switch
/// which is not armed looks exactly like one that is, right up until it is needed.
pub fn effective_halt_admit(
    requested: HaltAdmit,
    venue: &str,
) -> (HaltAdmit, Option<&'static str>) {
    match requested {
        HaltAdmit::Admit => (HaltAdmit::Admit, None),
        HaltAdmit::Verify => match halt_verify_support(venue) {
            HaltVerify::Real => (HaltAdmit::Verify, None),
            HaltVerify::Degrades(why) => (HaltAdmit::Admit, Some(why)),
        },
    }
}

/// What a mount owes the operator about `halt_admit`, once the mount OUTCOME is known — the second
/// half of the report, and a separate question from [`effective_halt_admit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltAdmitArming {
    /// Say NOTHING. The default mode adds no log line anywhere; that is what makes "byte-identical
    /// with no `policy.toml`" true in the trace file and not only on the wire.
    Silent,
    /// `verify` is genuinely in force: this venue's LIVE exec client holds the position book its
    /// halt boundary reads.
    Armed,
    /// `verify` was requested, the venue's ADAPTER can honour it, and this mount landed on the paper
    /// client anyway — absent credentials, or a failed handshake. The paper client holds no position
    /// book, so nothing is verified this session, and the operator believes otherwise unless told.
    MountedPaper,
}

/// Does this mount owe the operator a line about `halt_admit`, and which one?
///
/// ⚠ **This is a fact about the MOUNT and cannot be answered by [`halt_verify_support`], which is a
/// fact about the ADAPTER.** `vike_mount::make_engine` resolves the mode at the TOP of the function —
/// before credentials, before cTrader's synchronous handshake — and either of those can put
/// `vike_paper::PaperExecutionClient` at the venue instead. Announcing "verify is ARMED" from up
/// there was a claim the mount could not keep: under `verify` a paper mount is not merely
/// un-tightened, it is the exact shape of the original defect — the operator believes a check is
/// running that is not. So the paper case is [`Self::MountedPaper`] (a `warn`) rather than silence.
///
/// A pure function rather than three `tracing` calls inline, so the DECISION is unit-testable: a
/// log-line assertion needs a subscriber harness, and the thing worth pinning here is which of the
/// three answers a (mode, outcome) pair produces.
pub const fn halt_admit_arming(effective: HaltAdmit, live: bool) -> HaltAdmitArming {
    match (effective, live) {
        (HaltAdmit::Admit, _) => HaltAdmitArming::Silent,
        (HaltAdmit::Verify, true) => HaltAdmitArming::Armed,
        (HaltAdmit::Verify, false) => HaltAdmitArming::MountedPaper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VENUES;

    /// The default IS today's behaviour. Stated as its own test because every byte-identical claim
    /// downstream rests on it.
    #[test]
    fn the_default_is_admit() {
        assert_eq!(HaltAdmit::default(), HaltAdmit::Admit);
        assert_eq!(HaltAdmit::default().as_str(), "admit");
    }

    /// The per-venue completeness gate, exactly as `crate::venues`' contract requires: every roster
    /// venue has a NAMED row, so adding a bridge crate fails here until its halt-verify support is
    /// classified.
    #[test]
    fn every_roster_venue_has_a_named_halt_verify_row() {
        // The fallback arm's reason, which no roster venue may ride.
        let fallback = halt_verify_support("no-such-venue-anywhere");
        for venue in VENUES {
            let row = halt_verify_support(venue);
            assert_ne!(
                row, fallback,
                "{venue} rides the unknown-venue fallback — add a NAMED row saying whether it \
                 holds a position book at its halt boundary, even if the answer is the same"
            );
        }
    }

    /// A degrade must SAY something. An empty or one-word reason is how a silent degrade gets
    /// reintroduced wearing a `Some(_)`.
    #[test]
    fn every_degrade_reason_is_an_argument() {
        for venue in VENUES {
            if let HaltVerify::Degrades(why) = halt_verify_support(venue) {
                assert!(
                    why.len() > 30 && why.contains(' '),
                    "{venue}'s degrade reason is not something an operator can act on: {why:?}"
                );
            }
        }
    }

    /// `Admit` is never degraded and never annotated — asking for the default must not produce a
    /// mount-time log line on any venue, or the byte-identical claim is false in the log.
    #[test]
    fn admit_is_in_force_everywhere_with_nothing_to_report() {
        for venue in VENUES {
            assert_eq!(effective_halt_admit(HaltAdmit::Admit, venue), (HaltAdmit::Admit, None));
        }
        assert_eq!(effective_halt_admit(HaltAdmit::Admit, "not-a-venue"), (HaltAdmit::Admit, None));
    }

    /// `Verify` survives exactly where a book exists, and degrades WITH A REASON everywhere else.
    #[test]
    fn verify_survives_only_where_a_book_exists() {
        let (mode, why) = effective_halt_admit(HaltAdmit::Verify, "ctrader");
        assert_eq!(mode, HaltAdmit::Verify);
        assert_eq!(why, None, "the one real venue must not be reported as degraded");

        for venue in VENUES.iter().filter(|v| **v != "ctrader") {
            let (mode, why) = effective_halt_admit(HaltAdmit::Verify, venue);
            assert_eq!(mode, HaltAdmit::Admit, "{venue} must degrade, not silently claim verify");
            assert!(why.is_some(), "{venue} degraded with no reason to report");
        }
    }

    /// The file spelling round-trips — `policy.toml` holds `halt_admit = "verify"`, and
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
    /// 2026-08-08): with that arm returning `Armed`, this module runs `8 tests: 6 passed, 2 failed`
    /// — this one and [`adapter_support_is_not_mount_arming`].
    #[test]
    fn the_mount_outcome_decides_what_the_operator_is_told() {
        assert_eq!(halt_admit_arming(HaltAdmit::Admit, true), HaltAdmitArming::Silent);
        assert_eq!(
            halt_admit_arming(HaltAdmit::Admit, false),
            HaltAdmitArming::Silent,
            "the DEFAULT mode must add no line on ANY mount outcome"
        );
        assert_eq!(halt_admit_arming(HaltAdmit::Verify, true), HaltAdmitArming::Armed);
        assert_eq!(
            halt_admit_arming(HaltAdmit::Verify, false),
            HaltAdmitArming::MountedPaper,
            "a paper mount holds no position book, so `verify` armed NOTHING — saying it did is \
             the defect this whole knob exists to avoid, one layer up"
        );
    }

    /// …and the ADAPTER table cannot answer that question, which is why there are two functions.
    /// `halt_verify_support("ctrader")` is `Real` whatever the mount did with it.
    #[test]
    fn adapter_support_is_not_mount_arming() {
        assert!(halt_verify_support("ctrader").is_real());
        assert_eq!(effective_halt_admit(HaltAdmit::Verify, "ctrader"), (HaltAdmit::Verify, None));
        assert_eq!(
            halt_admit_arming(HaltAdmit::Verify, false),
            HaltAdmitArming::MountedPaper,
            "`Real` + a paper mount is a real, reportable state — treating `Real` as `armed` is \
             the confusion this pair separates"
        );
    }
}
