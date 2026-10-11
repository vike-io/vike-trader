//! `arming` — "is this box armed for live?", answered from the SETTINGS ROWS.
//!
//! [`armed_for_live`] is what `vike-cli config check` asks to decide whether an UNREADABLE
//! credential store is the degrade `docs/decisions/0013-degrade-vs-refuse.md` records or the false
//! belief it refuses on: an unreadable store on a paper box costs nothing an operator believes in,
//! while on a box armed for live it means the process will place NO orders while every surface
//! reads live.
//!
//! ## ⚠ Never the credential store
//!
//! The question cannot be asked of the credential store, and the reason is worth stating once: for
//! a venue with no switch of its own (deribit, alpaca, aster, …) "am I armed for live" is
//! answerable only from the credential PREFIXES (`DERIBIT_LIVE_*` vs `DERIBIT_DEMO_*`) — inside the
//! very store that could not be read. The signal and the failure are THE SAME OBJECT, so the answer
//! has to come from a source that is independent of the store: the settings rows.
//!
//! [`TRADEHUB_LIVE_ARMING`] is the main one. `flags.tradehub_live` is the headless daemon's live
//! master gate, and it is **NODE-scoped**: it selects `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s
//! `live_mount` over the paper one, and that mount is `crates/vike-mount/src/node/build.rs`'s `build_node`,
//! which issues a `make_engine` call for EVERY venue in `WIRED_MARKETS` and takes each one live iff
//! its credentials resolve — switched and switchless alike. So it sees every venue, it lives outside
//! the credential store, and it is read by the daemon itself (`crate::CONSUMPTION`'s
//! `flags.tradehub_live` row names `crates/vike-tradehub/src/tradehub_cli.rs`) — which is what makes
//! it evidence about this box rather than a declaration nobody honours.
//!
//! ### ⚠ The run profile's `venue` was the obvious second source, and it is the WRONG one
//!
//! It is the source that suggests itself, because a profile plainly names a venue
//! (`venue = "bybit"`), and it UNDER-COUNTS by construction. `build_node` does not mount the
//! profile's venue — it mounts the whole `WIRED_MARKETS` table and returns `Node::live_venues`, "the
//! set of venues with a credential-gated LIVE exec client". The profile's venue selects which pair
//! the STRATEGY trades, not which venues authenticate.
//!
//! A profile naming `bybit` over a store holding `BYBIT_DEMO_*` only: a `DERIBIT_LIVE_*` pair
//! appended to that same store would be taken live by the same mount, on the same start, with no
//! profile edit. A check keyed on the profile's venue would report a deribit-live box as
//! bybit-only. So the profile is not consulted here at all, and no `--profile` flag was added to
//! the verb: a second input that answers a NARROWER question than the one already available is a
//! way to be wrong with more ceremony.
//!
//! ### ⚠ `flags.poly_exec` / `flags.poly_reconcile` are counted too
//!
//! The unread-settings sweep wired both — `crate::CONSUMPTION` names
//! `crates/vike-tradehub/src/tradehub_cli.rs` for each, where the resolved flags are folded into the
//! map the Polymarket mount reads — so a row arms a real exec mount and excluding it would be a
//! hole in exactly the direction this module exists to close: armed from a row, reported UNARMED,
//! degrading past an unreadable store.
//!
//! [`POLY_FILE_ARMING`] is the fold, and `the_polymarket_flags_are_file_evidence_now` gates it
//! (it was `the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing`, the name
//! `docs/decisions/0055` cites).
//!
//! ### ⚠ "I could not tell" must never be spelled "not armed"
//!
//! Every source is a RESOLVED setting, so a caller that could not resolve the settings tree has no
//! answer — and the failure mode being closed here is precisely a box reading UNARMED for want of a
//! signal. Answering that case `Unarmed` would rebuild the same hole one level up, so it is not an
//! available answer: [`LiveArmingVerdict`] has a third variant, [`LiveArmingVerdict::Undetermined`],
//! and [`LiveArmingVerdict::refuses`] is true for it.
//!
//! ### What this still does NOT see
//!
//! Nothing the GUI does: `vike-desktop` mounts no venue (#1610).
//!
//! ## The refusal this module used to hold (deleted 2026-10-10)
//!
//! Until 2026-10-10 this module also REFUSED startup when the credential store carried an arming
//! switch row: `CREDENTIAL_FILE_ARMING_REFUSED` listed the four `{VENUE}_MAINNET` switches decision
//! 0095 retired and `POLY_EXEC`/`POLY_RECONCILE`, and `refuse_credential_file_arming` ran over the
//! credential map at `vike-boot`'s credential step. Nothing reads such a row any more: no code reads
//! a `{VENUE}_MAINNET` name, and every root that mounts Polymarket overwrites both `POLY_*` names
//! with the resolved flag before a mount reads the map
//! (`crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `fold_flags_into_vars`). A row stored under
//! one of those names is an unknown credential name now, read by nothing and stopping nothing
//! (`docs/decisions/0117-there-are-no-migrations.md`). The same table's process-environment half
//! went with it: each of the six names is a `crate::REMOVED_ENV` row, so a process carrying one
//! refuses at startup before any question about arming is asked.

use crate::flags::Flags;

/// How many venues one piece of arming evidence can speak for.
///
/// The distinction is the whole reason [`armed_for_live`] reports a scope at all, so it is a TYPE
/// and not a comment: a report that says "armed" without saying whether the evidence covers one
/// venue or the whole mount cannot tell an operator whether the absence of evidence means anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingScope {
    /// ONE venue: the Polymarket gates (`flags.poly_exec`, `flags.poly_reconcile`). Silence about a
    /// venue with no such row is silence, not a `no`.
    Venue,
    /// The whole NODE: every venue the mount would take live. Silence here IS a `no`, because the
    /// gate is a single resolved setting rather than a per-venue row that may not exist.
    Node,
}

/// One reason to believe this box intends to trade LIVE, in terms an operator would recognise.
///
/// Deliberately flat `&'static str`s: a caller assembling a refusal message wants one list to
/// iterate, not several shapes to match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveArming {
    /// What the operator SET, spelled the way they would find it again: the settings row.
    pub source: &'static str,
    /// What that turns on, in the operator's own terms — this is what a message leads with,
    /// because "the row is set" is not why they should care.
    pub arms: &'static str,
    /// Whether this evidence speaks for one venue or for the mount.
    pub scope: ArmingScope,
}

/// The ONE node-scoped arming source: the headless daemon's real-money master gate.
///
/// The flag resolves from its `flags.tradehub_live` settings row alone: decision 0111 retired
/// `VIKE_TRADEHUB_LIVE`, which every booted root refuses at startup naming the row.
pub const TRADEHUB_LIVE_ARMING: LiveArming = LiveArming {
    source: "flags.tradehub_live (the settings database)",
    arms: "the headless daemon's credential-gated TWELVE-VENUE live mount — every venue whose \
           credentials are in the store goes live, SWITCHLESS ones (deribit/alpaca/aster/…) \
           included",
    scope: ArmingScope::Node,
};

/// The two Polymarket gates, as settings rows.
///
/// ⚠ The unread-settings sweep WIRED both (`crate::CONSUMPTION` names
/// `crates/vike-tradehub/src/tradehub_cli.rs` for each), so a row arms a real Polymarket exec mount
/// and excluding it would be a hole in exactly the direction this module exists to close: a box
/// armed from a row, reported UNARMED, degrading past an unreadable store.
/// `the_polymarket_flags_are_file_evidence_now` is the gate.
const POLY_FILE_ARMING: &[PolyFileArming] = &[
    PolyFileArming {
        on: |f| f.poly_exec,
        source: "flags.poly_exec (the settings database)",
        arms: "Polymarket LIVE order placement — the venue has no testnet, so every order it \
               mounts is real money on Polygon mainnet",
    },
    PolyFileArming {
        on: |f| f.poly_reconcile,
        source: "flags.poly_reconcile (the settings database)",
        arms: "Polymarket reconcile — authenticated MAINNET account reads",
    },
];

/// One [`POLY_FILE_ARMING`] row.
///
/// A named struct rather than a `(fn(Flags) -> bool, &str, &str)` tuple: clippy's `type_complexity`
/// refuses that spelling under this workspace's `-D warnings` merge gate. The fields read better than
/// positions anyway — `on` is a FLAG READER, and a tuple hides which of the two `&str`s is which.
struct PolyFileArming {
    /// Reads the RESOLVED flag out of [`Flags`] — the settings row alone, since decision 0095
    /// retired the flag's environment layer (a set variable refuses startup instead).
    on: fn(Flags) -> bool,
    /// What [`LiveArming::source`] says when this row armed the box: the settings row that did.
    source: &'static str,
    /// What [`LiveArming::arms`] says: what the gate turns on.
    arms: &'static str,
}

/// Whether this box intends to trade LIVE — the three answers, one of which is "I could not tell".
///
/// A plain `Vec<LiveArming>` was the obvious return type and it is the wrong one: an empty vector
/// reads as "not armed" whether the sources all answered NO or they could not be consulted, and
/// conflating those two is the exact defect this module exists to repair. So the ignorance case is
/// a VARIANT, and [`LiveArmingVerdict::refuses`] treats it like an arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveArmingVerdict {
    /// Every source answered, and none of them says live. The ONLY answer that permits the ADR 0013
    /// degrade — a paper box, which is the overwhelmingly common case and must keep starting.
    Unarmed,
    /// At least one source says live. Never empty: [`TRADEHUB_LIVE_ARMING`] first, then the
    /// Polymarket rows in table order.
    Armed(Vec<LiveArming>),
    /// The settings tree did not load, so no source could be consulted — and "unarmed" is not
    /// something anyone here knows.
    Undetermined,
}

impl LiveArmingVerdict {
    /// Whether a caller deciding a DISPOSITION must take the strict branch: armed, or unknowable.
    ///
    /// The fail-safe direction, and the whole point of the [`LiveArmingVerdict::Undetermined`]
    /// variant. Only [`LiveArmingVerdict::Unarmed`] — every source consulted, every source silent —
    /// earns the degrade.
    #[must_use]
    pub fn refuses(&self) -> bool {
        !matches!(self, LiveArmingVerdict::Unarmed)
    }

    /// The evidence, for a message that has to NAME what armed the box. Empty for the two variants
    /// that have none, which a caller renders as its own sentence rather than a list.
    #[must_use]
    pub fn evidence(&self) -> &[LiveArming] {
        match self {
            LiveArmingVerdict::Armed(v) => v,
            LiveArmingVerdict::Unarmed | LiveArmingVerdict::Undetermined => &[],
        }
    }
}

/// Does this box intend to trade LIVE? Asked of the RESOLVED settings rows, NEVER of the credential
/// store — see the module doc for the argument.
///
/// 1. [`TRADEHUB_LIVE_ARMING`], when `flags.tradehub_live` is on — node-scoped, so it is the source
///    that sees a switchless venue at all.
/// 2. The two Polymarket gates (`flags.poly_exec`, `flags.poly_reconcile`), one venue each.
///
/// ⚠ `flags` is `None` ONLY when the settings tree did not LOAD, which is a real state for the
/// caller that has it: `vike-cli config check` reports on a directory whose settings rows do not
/// load. It does NOT mean "no flags rows" — a store with no `flags` row resolves to
/// `Flags::default()`, every field false, and that is a genuine `Some`. Passing `Flags::default()`
/// for "I do not know" is the one call-site mistake that reopens the hole, which is why the
/// parameter is an `Option` and the unknown answer is a variant rather than an empty list.
#[must_use]
pub fn armed_for_live(flags: Option<Flags>) -> LiveArmingVerdict {
    let Some(f) = flags else {
        return LiveArmingVerdict::Undetermined;
    };
    let mut out: Vec<LiveArming> = Vec::new();
    if f.tradehub_live {
        out.push(TRADEHUB_LIVE_ARMING);
    }
    for row in POLY_FILE_ARMING {
        if (row.on)(f) {
            out.push(LiveArming { source: row.source, arms: row.arms, scope: ArmingScope::Venue });
        }
    }
    if out.is_empty() { LiveArmingVerdict::Unarmed } else { LiveArmingVerdict::Armed(out) }
}

#[path = "arming_tests.rs"]
#[cfg(test)]
mod arming_tests;
