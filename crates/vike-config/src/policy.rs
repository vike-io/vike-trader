//! [`Policy`] — the HARD CEILINGS. Code defaults and a file, and nothing else.
//!
//! Policy answers "what is this deployment never allowed to exceed", which makes it the one
//! settings type whose value must be *harder* to change than the rest. It is therefore the only
//! type here with FEWER layers than the others — no CLI — and that reduction is the
//! entire reason the type exists as its own struct rather than a few fields on [`crate::Config`].
//!
//! **The reduction is structural.** `Policy` does not implement [`crate::CliOverride`] and has no
//! `apply_cli` inherent method, so `policy.apply_cli(&cli)` does not fail a review — it fails to
//! compile. See [`crate::layers`] for why that is worth a sealed trait: a ceiling settable from
//! outside the settings database can be raised by a stale systemd unit or an inherited shell with
//! no diff, no review, and a run that looks completely normal. (No settings type has an
//! environment layer at all since decision 0111.)
//!
//! ## The fields, and where each one lives today
//!
//! Ported from the real settings in play, not invented. The per-field docs carry the provenance;
//! no count is written here, because a count stops matching the struct the moment a field is
//! added.
//!
//! ## ⚠ A ceiling on this struct MUST be read by something
//!
//! `Policy`'s guarantee — a limit here cannot be widened from the environment — is worth nothing
//! when nothing READS the limit: an operator who sets such a field gets validation, no complaint,
//! and no cap. `max_total_exposure` and `rate.max_utilization` were two such fields and are
//! TOMBSTONES on [`PolicyPatch`] (see those fields), and
//! `crates/vike-config/tests/policy_is_consumed.rs` is the standing gate: every field of this
//! struct must name a file that genuinely reads it, or carry a written admission that nothing does.
//! A consumer INSIDE this crate (a clamp in [`fn@crate::load`]) is self-consumption and does not
//! count.
//!
//! ⚠ The bounds on `market_slippage` are **reused** from `vike-model`
//! ([`vike_model::market_slippage`]), never redefined here. A second copy of a bound is exactly the
//! split-brain that module's own doc warns about; this crate imports
//! [`MIN_MARKET_SLIPPAGE`]/[`MAX_MARKET_SLIPPAGE`] so a ceiling cannot drift from the clamp that
//! enforces it downstream.

use serde::{Deserialize, Serialize};
use vike_model::HaltAdmit;
use vike_model::market_slippage::{MAX_MARKET_SLIPPAGE, MIN_MARKET_SLIPPAGE};

mod apply;
mod patch;

pub use patch::{PolicyPatch, RatePolicyPatch};

/// Default leverage ceiling: **1x, i.e. no leverage**.
///
/// A ceiling's default must be the conservative end, not the permissive one: a deployment with no
/// `policy` rows gets the safest reading, and raising it is a deliberate, journalled settings
/// write (`vike-cli config set`). Note this differs from `vike_model::ProfileRisk::max_leverage`,
/// whose `None` means "the buying-power gate stays off entirely" — a default of "unbounded" is not
/// a ceiling.
pub const DEFAULT_MAX_LEVERAGE: f64 = 1.0;

/// The dead-man timeout the mount-time warning RECOMMENDS for a 24/7 venue: 60 seconds of feed
/// silence. **A recommendation, never a default** — nothing in this workspace applies it silently.
///
/// ⚠ Not an armed default: the switch observes SILENCE, not the connection, so an armed default
/// halts every session-bounded venue at every close and any thin market in a quiet minute
/// ([`Policy::deadman_timeout_ms`] records why). For a
/// venue that never closes, sixty seconds is coarse enough that a WS pump's ordinary re-dial does
/// not trip it and short enough that a book of resting quotes is pulled before a one-minute bar
/// closes on a venue the daemon can no longer see. The warning `vike-tradehub`'s live mount emits
/// when the key is ABSENT (`crates/vike-tradehub/src/venue_arming/deadman.rs`'s `deadman_absent_warning`)
/// prints this value as the paste-ready line — `vike-cli config set
/// policy.deadman_timeout_ms <ms>` (`docs/ops/kill-switches.md` documents the key). An operator
/// writes it; the binary never assumes it.
pub const RECOMMENDED_DEADMAN_TIMEOUT_MS: u64 = 60_000;

/// The spelling that DISABLES the dead-man EXPLICITLY: `deadman_timeout_ms = 0`.
///
/// An absent key is off too — see [`Policy::deadman_timeout_ms`] — but an absent key is off with a
/// mount-time WARNING, because the daemon cannot tell "the operator decided against it" from "the
/// operator never heard of it". Writing zero is how the operator says the decision was made, and
/// it is the ONLY spelling that silences the warning. Nothing between this and
/// [`MIN_DEADMAN_TIMEOUT_MS`] is legal.
pub const DEADMAN_DISABLED_MS: u64 = 0;

/// The smallest ARMED dead-man timeout the file accepts, exclusive of the disabling zero: one
/// second.
///
/// A sub-second dead-man is a false halt on any quiet second. The switch observes liveness from the
/// WHOLE core's ingest — any venue event, any market tick, on any symbol — and a live account
/// following one instrument goes hundreds of milliseconds between ticks routinely; a trip there
/// cancels every resting order on a real account and engages HALT over a gap that was never an
/// outage. Rejected by name, never clamped: a value that was silently raised to one second is a
/// timeout the operator believes they set and do not have.
pub const MIN_DEADMAN_TIMEOUT_MS: u64 = 1_000;

/// The largest dead-man timeout the file accepts: one day.
///
/// A dead-man that waits a day is not a dead-man — a feed that has been silent for a day has left a
/// book resting through an entire session, which is exactly what the switch exists to prevent. The
/// bound exists so that a unit typo (`60000000`, seconds mistaken for milliseconds, an extra zero)
/// is refused at load rather than shipping as a switch that will never fire. Rejected, never
/// clamped, for the same reason as the floor.
pub const MAX_DEADMAN_TIMEOUT_MS: u64 = 86_400_000;

/// **The LINK dead-man's grace window when `link_deadman_grace_ms` is ABSENT: two minutes.**
///
/// ⚠ Unlike [`RECOMMENDED_DEADMAN_TIMEOUT_MS`] this one IS applied — an absent
/// [`Policy::link_deadman_grace_ms`] arms the connection-state switch at this value on every venue
/// `vike_model::link_deadman_default` says defaults ON. That is the whole difference between the
/// two switches: the silence one cannot tell a closed market from a dead socket, so it may not have
/// an armed default; this one observes what the BRIDGE reports about the link and stays quiet
/// through a market that merely closed, so it may.
///
/// **Derived, not chosen — and derived off the lanes that actually EMIT a disconnect.** The grace
/// must outlast one full ORDINARY reconnect, or the switch fires on the reconnects it is supposed
/// to sit through. ⚠ Not `vike_bridge_core::pump_spec`'s backoff or the subscribe-ack watchdog:
/// those govern the kline/trade lanes, which disclose no `StreamStatus` at all, so nothing they do
/// can ever reach this switch. The emitting lanes are two:
///
/// * **The L2 depth driver** (`vike_bridge_core::depth`'s `run_depth_feed`), the DOM-lane emitter
///   for binance/aster/bybit/okx and the SLOWEST of the three. Its worst ordinary cycle is the
///   bounded dial (`CONNECT_10S`, shared with the pump and 10 s), plus a REST book seed
///   (`crates/bridges/binance/src/family/depth.rs`'s `DEPTH_SEED_TIMEOUT`, 10 s), plus the venue's
///   own `DEPTH_BACKOFF` (3 s on each of the four) — 23 s end to end.
/// * **The HFT tick pumps** (`crates/bridges/binance/src/family/depth.rs`'s `md_main` and its
///   bybit/okx twins), the same four venues' OTHER emitter and the one a `vike-tradehub` CEX mount
///   actually subscribes. Its worst ordinary cycle is STRICTLY INSIDE the depth driver's: the same
///   bounded dial and the same 10 s REST seed on binance/aster, plus a 1 s reconnect nap rather
///   than 3 s — 21 s end to end; bybit/okx WS-seed their books, so their cycle is the same bounded
///   dial plus the nap, 11 s. ⚠ The dial is BOUNDED (a bare `tungstenite::connect` would pin the
///   thread for the OS's own SYN ladder on a black-holed route — longer than this whole grace);
///   bounding it is what makes this bullet a derivation rather than an estimate.
/// * **The market pump** (`vike_bridge_core::market_pump`), polymarket's emitter, whose exponential
///   ladder starts at 500 ms and only reaches its 30 s cap after a run of CONSECUTIVE failures,
///   which is an outage rather than a reconnect.
///
/// Two minutes is ~5 depth cycles: an ordinary re-dial, a flapping one, and a venue that needs
/// several attempts are all silent, while a link that is genuinely gone still has the book pulled
/// inside two minutes.
pub const DEFAULT_LINK_DEADMAN_GRACE_MS: u64 = 120_000;

/// The spelling that DISABLES the link dead-man EXPLICITLY: `link_deadman_grace_ms = 0`.
///
/// ⚠ The opposite convention to [`DEADMAN_DISABLED_MS`]'s neighbour: for THIS key the absent state
/// is ON (at [`DEFAULT_LINK_DEADMAN_GRACE_MS`]) and zero is the only off, so there is no
/// "off by omission" to warn about and zero needs no warning either — an operator who wrote it
/// decided, and one who wrote nothing gets the armed default rather than silence.
pub const LINK_DEADMAN_DISABLED_MS: u64 = 0;

/// The smallest ARMED link-dead-man grace the file accepts, exclusive of the disabling zero: 30
/// seconds.
///
/// The floor is the measured worst ORDINARY reconnect — the 23 s derived on
/// [`DEFAULT_LINK_DEADMAN_GRACE_MS`] off the DEPTH driver, the SLOWEST of the lanes that emit a
/// disconnect on the crypto venues (their tick pumps, the lane a live `vike-tradehub` mount
/// subscribes, are 21 s and under; the derivation lists all three), rounded up. Below it the
/// switch cancels a book on the re-dial that follows any ordinary socket fault, which is the
/// false-trip class the whole M13
/// re-ruling exists to remove; a grace that short does not observe the connection, it observes the
/// venue's backoff. Rejected by name, never clamped: a value silently raised is a grace the
/// operator believes they set and do not have.
pub const MIN_LINK_DEADMAN_GRACE_MS: u64 = 30_000;

/// The largest link-dead-man grace the file accepts: one hour.
///
/// A link the bridge has reported DOWN for an hour, with orders resting behind it, is precisely
/// what this switch exists to end — so the ceiling is far below [`MAX_DEADMAN_TIMEOUT_MS`]'s day.
/// The bound's job is the same as its sibling's: refuse the unit slip (`120000000`, seconds written
/// as milliseconds) at load rather than shipping a switch that will never fire. Rejected, never
/// clamped, for the same reason as the floor.
pub const MAX_LINK_DEADMAN_GRACE_MS: u64 = 3_600_000;

/// What the dead-man switch does when it trips — the FILE spelling of `vike_core::DeadManAction`.
///
/// ⚠ A `vike-config`-LOCAL enum, deliberately not the core type itself: `vike-config` sits below
/// `vike-core` in the layer order and must not depend on it. The mapping to the core type lives in
/// the crate that depends on BOTH — the composition root that constructs the switch
/// (`crates/vike-tradehub/src/venue_arming/deadman.rs`'s `deadman_config_from_policy`) — and it is an
/// exhaustive `match`, so a variant added on either side is a compile error there rather than a
/// spelling that silently loads as the other one.
///
/// Serde `snake_case`, so the file spellings are `"cancel_all_and_halt"` and `"cancel_all"`; an
/// unknown spelling fails at deserialization naming the legal two — the [`Policy::halt_admit`]
/// idiom, free because the value type is an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadManActionSetting {
    /// Cancel every resting order AND engage HALT — the in-process `Halted` trading state on every
    /// engine plus the cross-process HALT sentinel file, the SAME file a manual `touch HALT` writes
    /// and every venue's submit boundary checks. **The default**: an outage that pulled the book is
    /// an incident, and a strategy that re-quotes the moment data resumes — into a market it has
    /// not seen for a minute — is the wrong first thing to happen after one. An operator un-halts.
    #[default]
    CancelAllAndHalt,
    /// Cancel every resting order and leave the trading state alone, so the strategy may re-quote
    /// on its own once data resumes. The lighter action, for a maker whose whole job is to be
    /// re-quoting and whose operator has decided a re-quote after an outage is acceptable.
    CancelAll,
}

impl DeadManActionSetting {
    /// The file spelling, for messages that name the legal set.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeadManActionSetting::CancelAllAndHalt => "cancel_all_and_halt",
            DeadManActionSetting::CancelAll => "cancel_all",
        }
    }
}

/// Hard ceilings for this machine. Set by an org/admin as `policy.*` rows in the settings database
/// (`vike-cli config set policy.<key> <value>`; decision 0086 — there is no `policy.toml`);
/// overridable by **nothing** below that.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Maximum leverage any strategy on this machine may request, `>= 1.0` (`10.0` = 10x).
    ///
    /// Was `[risk] max_leverage` in a run-profile TOML (`vike_model::ProfileRisk::max_leverage`,
    /// which converts it to `RiskLimits::im_requirement` as `1.0 / max_leverage`). Defaults to
    /// [`DEFAULT_MAX_LEVERAGE`].
    pub max_leverage: f64,

    /// Maximum notional of any single order, in quote currency. `None` = uncapped.
    ///
    /// Was **`VIKE_MAX_ORDER_NOTIONAL`** and **`VIKE_TRADEHUB_MAX_ORDER_NOTIONAL`** (the headless
    /// daemon's copy of the same idea). ⚠ This field is the clearest case the whole taxonomy exists
    /// for: an order-size ceiling that any exported variable can raise is not a ceiling. This is
    /// the only way to set it.
    ///
    /// ⚠ **A run profile's `[risk]` table carries a ceiling of this exact NAME that judges a
    /// DIFFERENT ACT, and the two are not a mirror.** This one guards the EDGE surfaces a human
    /// types at (the desktop's order preview, the daemon's control socket, an advisory CLI line);
    /// `vike_mount::MountPolicy::from` deliberately does not carry it, so it never reaches
    /// `vike_model::RiskLimits` and judges no strategy-emitted order. The profile's twin is the one
    /// the pre-trade gate evaluates, on every order from every origin. Nothing compares the two
    /// numbers at load, at mount or at submit. [`crate::ceilings::PRE_TRADE_CEILINGS`] is the
    /// authority for both rows and for what each refuses, and
    /// `crates/vike-config/tests/ceilings_are_distinct.rs` is what keeps them from being read as
    /// one.
    pub max_notional_per_order: Option<f64>,

    /// **The ACCOUNT-aggregate open-notional ceiling**, in quote currency. `None` — the default,
    /// and every deployment that writes no line — = uncapped, byte-identically to before this
    /// field existed.
    ///
    /// Enforced by `vike_exec::RiskGate::check_inner`'s `over-account-exposure` lane, one order at
    /// a time, on the projected world: this account's gross open notional across every symbol, with
    /// the order's own symbol re-valued as it will be AFTER the order. `vike_model::RiskLimits`'s
    /// `max_account_exposure` is the field it arrives at, [`crate::Policy`] → `vike_mount::
    /// MountPolicy` is the projection that carries it, and
    /// `crates/vike-config/tests/policy_is_consumed.rs` is the gate that keeps that chain real.
    ///
    /// # ⚠ Why THIS file rather than a run profile's `[risk]` table
    ///
    /// Because the two homes mean different things, and this ceiling is only true in one of them.
    ///
    /// * **It is a property of the BOX and of the wallet its credentials open, not of a strategy
    ///   run.** One account is one book at the venue; every engine this process mounts on it draws
    ///   on the same collateral, and profiles come and go over the top of that. `[risk]`'s
    ///   `max_total_exposure` reaches `vike_model::RiskLimits` through `vike_model::ProfileRisk`,
    ///   which is folded onto EACH engine's limits identically — so a profile knob would be N
    ///   copies of one number rather than one aggregate, which is the exact N×-weaker defect the
    ///   per-symbol cap already has and this axis exists to close. Putting the aggregate one layer
    ///   up in the same file would reproduce the bug with a longer name.
    /// * **A ceiling must be harder to change than the thing it bounds.** This is the settings type
    ///   with no env layer and no CLI layer (see this module's doc: both traits are sealed, so an
    ///   override is a compile error). A run profile is picked by a command-line argument, so the
    ///   same reasoning that took the notional ceiling out of the environment applies to
    ///   a number an operator can swap by pointing at another file.
    /// * **A `[risk]` table has no way to say which account it means.** One account's OWN figure
    ///   is its `account` row's `max_exposure` column, folded beside this one (below).
    ///
    /// # ⚠ It is ONE ceiling per account, not one ceiling for the box
    ///
    /// A scalar, applied to EVERY account this box mounts, each measured over its OWN book —
    /// because `vike_mount::make_engine_for_account` builds one engine per `(venue, AccountLabel)`
    /// (`vike_model::accounts::account_keys::AccountLabel`) and each carries its own copy. So
    /// two accounts of one venue get this budget EACH, which is correct while two labels really
    /// are two wallets: neither backs the other.
    ///
    /// ⚠ **Where they are NOT two wallets, this ceiling MULTIPLIES — a declared residual, detected
    /// and reported rather than refused.** [`crate::venue_accounts`]' shared-BOOK rule is the case:
    /// an agent key signing for a master whose own key is also configured, or one credential set
    /// pasted under two labels, is TWO engines over ONE venue ledger, and that module's own doc
    /// records that the mount REPORTS it and starts both. Each engine then applies the whole
    /// ceiling to its own half of that ledger, so the real book may hold a multiple of the number
    /// written here — the N×-looser defect this axis exists to close, one level up. The mount's
    /// shared-book warning names this key and its value when it is armed
    /// (`vike_mount::shared_book_ceiling_note`), so the multiplication is met at startup rather
    /// than after a fill, and
    /// `docs/decisions/0042-the-account-exposure-ceiling-is-a-policy-key.md` carries it as a
    /// residual. Refusing instead would contradict `docs/decisions/0013-degrade-vs-refuse.md` and
    /// the shared-book rule's own verdict; the operator may have meant the pair.
    ///
    /// One account's OWN figure is the `account` table's `max_exposure` column (decision 0119),
    /// and it can only NARROW this one: the mount folds this box-wide figure and then the
    /// account's, both through `vike_model::RiskLimits::narrow_account_exposure` — a `min` — so no
    /// row can raise an account above the number written here.
    ///
    /// Must be finite and `> 0` (`check_positive`); zero would deny every opening order, which is
    /// what the HALT sentinel is for, and is refused by name rather than accepted as a kill switch
    /// spelled sideways.
    pub max_account_exposure: Option<f64>,

    /// **The largest EQUITY FIGURE this deployment's sizing and admission lanes may see**, in
    /// quote currency. `None` — the default, and every deployment that writes no line — is
    /// uncapped and byte-identical to before this field existed.
    ///
    /// It is not a limit on what you may hold; [`Self::max_account_exposure`] is that. It is a
    /// limit on the NUMBER the decisions are computed from — `min(resolved equity, this)` — so a
    /// percent-of-equity sizer multiplies against a figure the operator wrote rather than one the
    /// venue reported, and the pre-trade margin lane admits against the same capped figure.
    ///
    /// # Why an equity ceiling exists at all
    ///
    /// Because on a live venue `Account::balance` is the venue's attested wallet for the WHOLE
    /// account the credentials open, and every reconcile pass adopts it
    /// (`crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports`). Under
    /// `vike_exec::BalanceMode::Authoritative` resolved equity is `balance + unrealized`, so a
    /// THIRD PARTY depositing to or withdrawing from a shared account moves the number this
    /// daemon sizes and admits against, with no file changed and nothing this process did.
    ///
    /// The alternative — DISPUTING the venue's figure, holding a cash divergence for an operator —
    /// was built, reviewed and abandoned;
    /// `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md` carries
    /// why, and the four-family competitor survey behind it (six platforms overwrite the balance
    /// unconditionally; the two that solved this problem solved it with a CAP —
    /// Hummingbot's `balance limit` applying `min(available, limit)`, and Freqtrade's
    /// `available_capital`, documented for *"running multiple bots on the same exchange
    /// account"*). This key is vike's version of that cap.
    ///
    /// # ⚠ THE ASYMMETRY — it applies to the CONSERVATIVE consumers ONLY
    ///
    /// A lower equity figure is **conservative** for sizing (you buy less) and for admission (you
    /// are refused sooner), and **DESTRUCTIVE** for the margin-call watchdog: a capped figure makes
    /// a healthy account look under-margined, and that sweep LIQUIDATES. So the ceiling reaches
    /// `vike_exec::ExecutionEngine::sizing_equity` — the one resolver every acting consumer goes
    /// through — and reaches `vike_exec::ExecutionEngine::resolved_equity`, which the margin-call
    /// sweep and every report surface read, NOT AT ALL. That method's doc is the authority for
    /// which side each consumer is on, and it names them.
    ///
    /// # ⚠ It is ONE ceiling per ENGINE, like [`Self::max_account_exposure`]
    ///
    /// A scalar, folded onto each engine's `vike_model::RiskLimits` at the mount, so every
    /// `(venue, AccountLabel)` this box mounts gets this figure measured over its OWN book. Two
    /// accounts of one venue get this budget EACH, which is correct while two labels really are
    /// two wallets — and MULTIPLIES where they are one, exactly as the exposure ceiling beside it
    /// does and for the same reason; `crate::venue_accounts`' shared-BOOK rule is the case, and it
    /// is REPORTED at startup rather than refused.
    ///
    /// Must be finite and `> 0` (`check_positive`). Zero would make every percent-of-equity sizer
    /// return zero units and every margin lane refuse, which is a kill switch spelled sideways —
    /// the HALT sentinel is the one that exists, and this key is refused at `0` by name rather
    /// than accepted as a second one.
    pub max_sizing_equity: Option<f64>,

    /// Aggression band for an EMULATED market order, as a fraction (`0.002` = 0.2 %). `None` — the
    /// default — means each venue keeps its own historical literal, byte-identically.
    ///
    /// Only venues with **no native market order** consult this; hyperliquid is the only one on the
    /// roster (`vike_model::market_slippage`'s module doc has the survey). Such an adapter prices a
    /// market intent as an `Ioc` limit at `mid * (1 ± band)`, and a tripped stop-MARKET at
    /// `trigger * (1 ± band)`. The band is therefore **the worst price the order is allowed to
    /// reach** — not a prediction and not a fee. Hyperliquid's compiled-in value was `0.05`, i.e.
    /// 5 % on every market order and every protective stop exit, which never binds on a deep BTC
    /// book and binds completely on a thin alt.
    ///
    /// ⚠ Policy-class precisely because the failure is SILENT: a band that is too wide produces a
    /// filled order at a bad price with no rejection and no alert. Accepted values lie in
    /// [`MIN_MARKET_SLIPPAGE`]`..=`[`MAX_MARKET_SLIPPAGE`], and the maximum is deliberately the
    /// widest band already in the code — so this key can only ever TIGHTEN a venue, never widen one.
    pub market_slippage: Option<f64>,

    /// How much evidence the HALT file sentinel demands before letting a submit out:
    /// `"admit"` (the default) or `"verify"`. See [`vike_model::HaltAdmit`], which owns both the
    /// modes and the per-venue table of where `verify` is real.
    ///
    /// **Policy-class, and it belongs here rather than anywhere an environment variable could
    /// reach, for the same reason as every other field on this struct**: it governs what a KILL
    /// SWITCH lets through. A knob a stale systemd `Environment=` line or an inherited shell export
    /// could relax is not a safety setting — and this one has the extra property that its two
    /// values differ ONLY under an engaged halt, i.e. only during an incident, which is the worst
    /// possible moment to discover that the environment overrode your file.
    ///
    /// ⚠ **`"verify"` is weaker than the word suggests, and only a LIVE cTrader mount implements
    /// it.** It refuses only what a POSITIVE report from the venue's own position book PROVES opens
    /// risk; every absence (never fetched, fetch failed, socket dropped since, an answer that could
    /// not be read in full or was for another account, or simply no position reported in that
    /// symbol) still ADMITS, because *halting cannot trap you* outranks *halting is airtight*. Every
    /// other venue degrades to `"admit"` and says so once, at mount, naming the reason — and a
    /// cTrader venue that fell back to PAPER says THAT at mount too, because the paper client holds
    /// no book either. `crates/vike-exec/src/halt.rs`'s module doc is the single authority for that
    /// sentence.
    ///
    /// ⚠ There is deliberately no third value. A `"refuse"` mode (no exemption at all) disarms the
    /// panic button in exactly the situations that halt on their own. An unknown spelling is
    /// rejected at load by name, with the legal set in the message — serde's own variant error,
    /// which is the whole of what an operator needs here (unlike the tombstones below, nobody ever
    /// had a working `refuse`).
    pub halt_admit: HaltAdmit,

    /// **The dead-man switch** — how long the live core's ingest may be SILENT before it cancels
    /// every resting order and (by default) engages HALT. Milliseconds. **`None` — the key ABSENT
    /// from the file — is OFF, with ONE warning at mount**; `Some(0)` ([`DEADMAN_DISABLED_MS`]) is
    /// OFF with no warning, the operator having decided; `Some(n)` arms it at `n`. Nothing arms it
    /// silently: there is no compiled-in timeout, and [`RECOMMENDED_DEADMAN_TIMEOUT_MS`] is the
    /// number the warning suggests, not one the binary applies.
    ///
    /// The mechanism is `crates/vike-core/src/runtime/deadman.rs` — implemented and unit-tested
    /// (`crates/vike-core/src/runtime/tests/deadman.rs`). It is armed as
    /// `vike_core::CoreConfig::deadman` by ONE composition root:
    /// `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s `live_mount_with`, through
    /// `deadman_config_from_policy` (which folds both `None` and `Some(0)` to the core's `None`);
    /// the absent-key warning is `deadman_absent_warning` beside it, and fires for `None` alone.
    /// `crates/vike-config/tests/policy_is_consumed.rs` names that call site and opens the file to
    /// check.
    ///
    /// # Why there is no armed default
    ///
    /// The safety case for one is true: a live daemon whose feed died with orders resting has no
    /// operator in front of it, and a switch an operator must remember to turn on is the switch
    /// that is off during the outage. What rules it out is what the switch actually OBSERVES. It
    /// counts ingest — `CoreThread::dispatch` records every venue event, market tick, closed bar,
    /// quote, trade and book update, on any venue and any symbol — and deliberately NOTHING else:
    /// not a control command, not the periodic waker, and not `Ingest::StreamStatus`, the one
    /// message that says a socket is alive. A transport heartbeat never reaches the core at all
    /// (oanda's `HEARTBEAT` frame closes a transport gap and emits no quote —
    /// `crates/bridges/oanda/src/market_feed.rs`'s `fold_pricing_line`). So the switch cannot tell
    /// a dead socket from a quiet market or a closed one, and an ARMED DEFAULT buys three halts
    /// nobody asked for:
    ///
    /// * **The session-close halt, deterministic.** On every session-bounded venue — FX (oanda, ig,
    ///   ctrader, fxcm) over the weekend, equities (alpaca, ibkr) every evening — the switch trips
    ///   about a minute after the last tick, at EVERY close: every order resting over the close is
    ///   cancelled (a GTC deliberately left over the weekend included), `Halted` is set on every
    ///   engine and the HALT sentinel is written to disk. The switch's own latch re-arms when data
    ///   resumes (`crates/vike-core/src/runtime/deadman.rs`'s `DeadMan::check`), but `Halted` and
    ///   the sentinel do NOT clear — the daemon opens the next session halted, refusing every submit
    ///   until an operator deletes the file.
    /// * **The thin-market halt, probabilistic.** A live mount following ONE illiquid instrument
    ///   goes sixty seconds without a tick in a quiet minute and trips a REAL halt on a live
    ///   account, over a gap that was never an outage.
    /// * **The cap that cannot be raised past the close.** [`MAX_DEADMAN_TIMEOUT_MS`] is one day,
    ///   and the weekend FX close is ~48 h — so on an FX venue an armed default leaves only `0` or
    ///   a switch that trips every Friday. A default that every FX operator must turn off before
    ///   the first weekend is not a safety default; it is a trap with a doc comment.
    ///
    /// **The correct dead-man observes the CONNECTION state, not silence**:
    /// [`Self::link_deadman_grace_ms`], ON by default, tripping on a socket the bridge
    /// reports dead and staying quiet through a market that merely closed.
    /// `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the record.
    /// THIS key is the OPTIONAL EXTRA for a 24/7 mount (a crypto perp venue that never closes
    /// and always ticks) that also wants a silence detector — off unless an operator writes it,
    /// with the mount-time warning so that "off" is a decision rather than an oversight. Do not
    /// re-derive an armed default from the safety argument alone; the argument is right and
    /// the mechanism is the wrong one for it.
    ///
    /// **Policy-class**, still: nothing in the environment can arm it OR disarm it. Both directions
    /// matter here — a stale `Environment=` line that armed a silence-detector on an FX box would
    /// halt it every Friday with no diff to review, exactly as one that disarmed a switch an
    /// operator relied on would leave a book resting through the next outage.
    ///
    /// ⚠ **The `paper_mount` arm does NOT arm it, deliberately — and that is a NARROWER claim than
    /// "paper does not".** Only `live_mount_with` constructs the config, so the daemon's paper
    /// rehearsal arm (the live gate OFF) passes the core its minimal default and never reads this
    /// field: a rehearsal that halted itself over a quiet minute would be surprising in a way
    /// nobody asked for, and a rehearsal's resting orders cost nothing to leave resting. But
    /// `live_mount_with` is entered by the LIVE GATE, not by any venue actually arming, and it
    /// constructs the switch BEFORE `build_node` decides per venue — so a live-gate run whose every
    /// exec still lands on the paper exchange (no active `account` row, which mounts ALL
    /// PAPER; every account at tier `paper`; a `data_only = true` mount, the seam built precisely for rehearsing on
    /// a real feed) IS armed when the key is written, and a trip there cancels paper orders, sets
    /// `Halted` and writes the process's REAL HALT sentinel — which outlives the rehearsal and is
    /// the file the next genuinely-live mount's submit boundary refuses on. Whether the arming
    /// should instead key on exec actually being armed (`vike_mount::armed_live_venues` is computed
    /// before the config, so the seam is reachable) is an owner decision neither ruling made; until
    /// it is, this paragraph states the shape as it is, and the operator page says the same.
    ///
    /// ⚠ **Journal replay: arming this changes NOTHING, and the derivation is short enough to write
    /// down so it is not re-derived.** `vike-core` writes the replay-refusing waker record when
    /// `journal_waker_records = submit_ack_timeout.is_some() || deadman.is_some()` — the
    /// `config_journal_waker_records` binding in `crates/vike-core/src/runtime/assemble.rs`, read into
    /// `CoreThread::journal_waker_records`; `crates/vike-core/src/replay.rs`'s module doc holds
    /// the WHY of the refusal, not the expression — and
    /// `live_mount_with` ALREADY sets `submit_ack_timeout: Some(Duration::from_secs(30))` — a
    /// run profile's `[guards]` can only ever replace that with another `Some` — so a live mount
    /// already forfeits journal replay before this key is consulted. The dead-man adds no second
    /// forfeit; there was nothing left to forfeit.
    ///
    /// Accepted values: absent, `0`, or [`MIN_DEADMAN_TIMEOUT_MS`]`..=`[`MAX_DEADMAN_TIMEOUT_MS`].
    /// Anything in `1..1000` (a false halt on any quiet second) or above a day (a dead-man that
    /// never fires) is REJECTED by name with the file named, never clamped — each bound argues
    /// itself on its constant.
    pub deadman_timeout_ms: Option<u64>,

    /// What the dead-man does when it trips: `"cancel_all_and_halt"` (the default) or
    /// `"cancel_all"`. See [`DeadManActionSetting`] for the two, and for why the type is this
    /// crate's own rather than `vike_core::DeadManAction`. Inert while
    /// [`Self::deadman_timeout_ms`] is `None` or `Some(0)` — the switch is then not constructed,
    /// so there is nothing for an action to act on — and validated by serde alone (an unknown
    /// spelling fails at load naming the legal two).
    pub deadman_action: DeadManActionSetting,

    /// **The LINK dead-man switch** — how long a venue's market-data link may be reported DOWN by
    /// its own bridge before the daemon cancels that venue's resting orders and (by default)
    /// engages HALT. Milliseconds. **ABSENT — the ordinary state — is ON at
    /// [`DEFAULT_LINK_DEADMAN_GRACE_MS`]**; `Some(0)` ([`LINK_DEADMAN_DISABLED_MS`]) is OFF, the
    /// operator having decided, with no warning; `Some(n)` arms it at `n`.
    ///
    /// # It is not the sibling above wearing a new number
    ///
    /// [`Self::deadman_timeout_ms`] observes SILENCE across the whole core's ingest, which is why
    /// it may not have an armed default: a market that CLOSES is silence too, so it halted every
    /// session-bounded venue at every close. That doc carries the three halts an armed default
    /// bought, and they are not restated here. **This key observes the CONNECTION**: the per-`(venue, symbol)` `FeedStatus` the
    /// bridges disclose — `Disconnected` opens the grace, `Live` for the same key closes it, and
    /// `vike_model::FeedStatus::Stale` NEVER counts, because "no fresh price exists" is exactly
    /// what a closed market looks like and is the signal this switch must ignore.
    /// `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the record.
    ///
    /// The two are INDEPENDENT: either, both or neither may be on, they are separate
    /// `vike_core::CoreConfig` fields, and this one shares only [`Self::deadman_action`] — what a
    /// trip DOES is the same question for both, and a second action key would be a second answer.
    ///
    /// # Why an armed default is safe here and was not there
    ///
    /// Because a per-venue table decides where it arms. `vike_model::link_deadman_default` carries
    /// one NAMED row per roster venue, read from the emitter it cites, and a venue whose market has
    /// SESSIONS — every FX and equities venue on the roster — is OFF with the reason until somebody
    /// OBSERVES a close being disclosed as `Stale` rather than as a disconnect. So the default
    /// cannot reproduce the close-halts-the-daemon defect: the venues that would suffer it are not
    /// armed. Seven roster venues are also DECLARED RESIDUALS in that table — their bridges emit no
    /// disconnect at all, so nothing could ever reach this switch for them.
    ///
    /// # Scope of a trip
    ///
    /// The VENUE whose link died, not the core: `vike_exec::OrderIntent::MassCancel { venue:
    /// Some(v), symbol: None }` through the one order-write path, so a bybit socket death leaves a
    /// binance book resting. HALT is the exception and is deliberately process-wide — under
    /// [`DeadManActionSetting::CancelAllAndHalt`] the trip sets `Halted` on EVERY engine and writes
    /// the one HALT sentinel file, because that file is process-wide by construction and a
    /// half-halted daemon is a state nobody asked for.
    ///
    /// **Policy-class**, like every field here: nothing in the environment can arm it OR disarm
    /// it. Both directions matter — a stale `Environment=` line that disarmed it would leave a
    /// book resting behind a dead socket with no diff to review.
    ///
    /// ⚠ **The `paper_mount` arm does not construct it**, the same ruling and the same reason as
    /// its sibling; `crates/vike-tradehub/src/venue_arming/deadman.rs`'s `link_deadman_config_from_policy`
    /// is the only fold, called from `live_mount_with` alone.
    ///
    /// Accepted values: absent, `0`, or [`MIN_LINK_DEADMAN_GRACE_MS`]`..=`
    /// [`MAX_LINK_DEADMAN_GRACE_MS`]. Anything in `1..30000` (below one ordinary reconnect, so a
    /// re-dial trips it) or above an hour (a link dead-man that never fires) is REJECTED by name
    /// with the file named, never clamped — each bound argues itself on its constant.
    pub link_deadman_grace_ms: Option<u64>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            max_leverage: DEFAULT_MAX_LEVERAGE,
            max_notional_per_order: None,
            // Uncapped, like its per-order sibling above: a deployment with no `policy` rows gets
            // exactly the gate it had before this axis existed. ⚠ This is the ONE ceiling on this
            // struct whose conservative end is NOT its default, and the reason is upgrade safety
            // rather than principle — any positive number would refuse orders on a running live
            // account from a row the operator never wrote.
            max_account_exposure: None,
            // Uncapped for the same upgrade-safety reason as the ceiling above it: any positive
            // number would shrink the equity a running deployment sizes and admits against, from a
            // row nobody wrote. `Policy::max_sizing_equity` argues the axis.
            max_sizing_equity: None,
            market_slippage: None,
            halt_admit: HaltAdmit::Admit,
            // ⚠ `None`, not a number: `Policy::deadman_timeout_ms`'s doc records why. Like every
            // other scalar here: a deployment with no `policy` rows arms nothing.
            deadman_timeout_ms: None,
            deadman_action: DeadManActionSetting::default(),
            // ⚠ `None`, and — unlike every other scalar on this struct — `None` here means ON at
            // `DEFAULT_LINK_DEADMAN_GRACE_MS`. The armed end is the safe end for THIS switch
            // because a per-venue table (`vike_model::link_deadman_default`) keeps every
            // session-bounded venue out of it; the field's own doc argues the asymmetry.
            link_deadman_grace_ms: None,
        }
    }
}

/// The number of fields [`Policy`]'s `Serialize` emits. Named so the impl below and its own gate
/// cannot disagree about the count.
const POLICY_SERIALIZED_FIELDS: usize = 9;

/// ⚠ **Hand-written rather than derived**, and it serializes exactly as `#[derive(Serialize)]`
/// would — same names, same order, no `skip_serializing_if` anywhere (an `Option::None` reaches the
/// format's own null, which is what makes `policy.max_notional_per_order` a real leaf in JSON and
/// an ABSENT key in TOML; `crates/vike-config/tests/provenance.rs` depends on both). Every field is
/// a scalar leaf: `Policy` carries no map, so no key space escapes `deny_unknown_fields`.
impl Serialize for Policy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Policy", POLICY_SERIALIZED_FIELDS)?;
        s.serialize_field("max_leverage", &self.max_leverage)?;
        s.serialize_field("max_notional_per_order", &self.max_notional_per_order)?;
        s.serialize_field("max_account_exposure", &self.max_account_exposure)?;
        s.serialize_field("max_sizing_equity", &self.max_sizing_equity)?;
        s.serialize_field("market_slippage", &self.market_slippage)?;
        s.serialize_field("halt_admit", &self.halt_admit)?;
        s.serialize_field("deadman_timeout_ms", &self.deadman_timeout_ms)?;
        s.serialize_field("deadman_action", &self.deadman_action)?;
        s.serialize_field("link_deadman_grace_ms", &self.link_deadman_grace_ms)?;
        s.end()
    }
}

impl Policy {
    /// The LINK dead-man grace ACTUALLY in force: `None` = off, `Some(ms)` = armed at `ms`.
    ///
    /// The whole of the absent-is-armed rule, in one place so no composition root re-derives it:
    /// an absent [`Self::link_deadman_grace_ms`] resolves to [`DEFAULT_LINK_DEADMAN_GRACE_MS`], an
    /// explicit [`LINK_DEADMAN_DISABLED_MS`] resolves to `None`, and every other value passes
    /// through (the file edge has already refused everything outside the bounds). ⚠ It resolves
    /// only the GRACE — whether a given venue arms is [`vike_model::link_deadman_default`]'s
    /// question, and combining the two is the composition root's job.
    #[must_use]
    pub const fn link_deadman_grace_ms_effective(&self) -> Option<u64> {
        match self.link_deadman_grace_ms {
            None => Some(DEFAULT_LINK_DEADMAN_GRACE_MS),
            Some(LINK_DEADMAN_DISABLED_MS) => None,
            Some(ms) => Some(ms),
        }
    }
}

#[cfg(test)]
use std::path::Path;

#[path = "policy_tests.rs"]
#[cfg(test)]
mod policy_tests;
