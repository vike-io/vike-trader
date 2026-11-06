//! `POLICY_VERDICTS`: the policy half of the consumption tables — one row per `policy.*` key,
//! saying whether the ceiling is ENFORCED by code outside this crate or is accepted and enforced by
//! NOTHING.
//!
//! ⚠ **This is the ONE place the unenforced-ceiling fact is written, and production code reads it.**
//! The fact used to live only in `crates/vike-config/tests/policy_is_consumed.rs`, as a test-local
//! `Consumed::No` row, so `vike-cli config show` — which cannot read a test file — answered `READ:
//! yes` for `policy.max_leverage`, a ceiling nothing enforced, and `config set` answered a generic
//! "no verdict". The gate now derives its `Consumed::No` rows from [`POLICY_VERDICTS`], so the
//! disclosure and the gate cannot disagree. ⚠ **No row is `Unenforced` today** —
//! `policy.max_leverage`, the last, is enforced since it narrows the run profile's leverage — and
//! the machinery stays: the next ceiling that is accepted before it is wired is admitted HERE, and
//! `config show` / `config set` print the admission (`UNENFORCED_POLICY_VERDICT`).
//!
//! What stays in the gate is the EVIDENCE for an [`PolicyVerdict::Enforced`] row: the file and the
//! needle that prove the read (`crates/vike-config/tests/policy_is_consumed.rs`'s
//! `POLICY_CONSUMERS`). Evidence is test-side data on purpose; the VERDICT is shipped data, because
//! the disclosure surfaces ship.
//!
//! Kept in `setting_keys()`'s own order (the `Policy` field order); the gate compares the key sets,
//! so a missing row fails loudly and a mis-ordered one fails nothing.

/// What enforces one `policy.*` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// Enforced by code outside this crate. WHICH code is the gate's evidence, not data here:
    /// `crates/vike-config/tests/policy_is_consumed.rs`'s `POLICY_CONSUMERS` names the file and the
    /// read, and opens the file to look.
    Enforced,
    /// Accepted, validated, serialized and displayed — and enforced by NOTHING. `why` is the
    /// argument the gate holds to a length and to the absence of a "todo": it must say what
    /// enforces the concept instead, because "nothing does" is the answer this table exists to
    /// make impossible to ship silently.
    Unenforced {
        /// What enforces the concept instead, and why this key is not carried.
        why: &'static str,
    },
}

/// The one-line verdict `config show` and `config set` print for an [`PolicyVerdict::Unenforced`]
/// key, ABOVE its `why` paragraph.
///
/// Worded for a CEILING: `Reader::verdict`'s "neither spelling" talks about an environment
/// variable, and a `Policy` key has no environment layer to speak of (it is sealed against one).
pub const UNENFORCED_POLICY_VERDICT: &str = "NOTHING ENFORCES THIS CEILING: the row is accepted and \
     validated like any other policy key, but no code acts on it, so setting it limits nothing.";

/// One row per `policy.*` key: the full dotted key and what enforces it.
pub const POLICY_VERDICTS: &[(&str, PolicyVerdict)] = &[
    // The leverage CEILING. It NARROWS the run profile's `[risk] max_leverage` (the leverage
    // authority: `vike_model::ProfileRisk::max_leverage` -> `RiskLimits::im_requirement`, which the
    // buying-power lane in `RiskGate::check_inner` evaluates, and the leverage each perp bridge posts
    // to its account) once, at the top of `vike_tradehub_core::make_engine_for_account`. Absent is NO
    // ceiling (`Policy::max_leverage` is an `Option`), so a deployment with no row behaves exactly as
    // it did when the key was enforced by nothing.
    ("policy.max_leverage", PolicyVerdict::Enforced),
    // Phase 5's real wiring: the three order surfaces each read this and cap an order with it.
    ("policy.max_notional_per_order", PolicyVerdict::Enforced),
    // The ACCOUNT-aggregate exposure ceiling, folded onto `vike_model::RiskLimits` at the end of
    // `make_engine_for_account`.
    ("policy.max_account_exposure", PolicyVerdict::Enforced),
    // The ceiling on the equity FIGURE the sizing and admission lanes may see.
    ("policy.max_sizing_equity", PolicyVerdict::Enforced),
    // The band a venue with no native market order prices its emulated market orders at.
    ("policy.market_slippage", PolicyVerdict::Enforced),
    // What a kill switch lets out; read by the one venue that holds a position book at its halt
    // boundary.
    ("policy.halt_admit", PolicyVerdict::Enforced),
    // The dead-man switch and its action, and the LINK dead-man's grace — all constructed by
    // `vike-tradehub`'s live mount.
    ("policy.deadman_timeout_ms", PolicyVerdict::Enforced),
    ("policy.deadman_action", PolicyVerdict::Enforced),
    ("policy.link_deadman_grace_ms", PolicyVerdict::Enforced),
    // The per-mount loss budget and its mode — folded into `vike_core::CoreConfig::mount_budget_default`
    // by `vike-tradehub`'s live mount, and judged by the core's per-mount budget latch.
    ("policy.max_mount_loss", PolicyVerdict::Enforced),
    ("policy.mount_loss_action", PolicyVerdict::Enforced),
];
