//! [`Policy`] — the HARD CEILINGS. Code defaults and a file, and nothing else.
//!
//! Policy answers "what is this deployment never allowed to exceed", which makes it the one
//! settings type whose value must be *harder* to change than the rest. It is therefore the only
//! type here with FEWER layers than the others — no env, no CLI — and that reduction is the
//! entire reason the type exists as its own struct rather than a few fields on [`crate::Config`].
//!
//! **The reduction is structural.** `Policy` implements neither [`crate::EnvOverride`] nor
//! [`crate::CliOverride`] and has no `from_env`/`apply_env`/`apply_cli` inherent method, so
//! `policy.apply_env(&env)` does not fail a review — it fails to compile. See
//! [`crate::layers`] for why that is worth a sealed trait: an env-settable ceiling can be raised
//! by a stale systemd unit or an inherited shell with no diff, no review, and a run that looks
//! completely normal.
//!
//! ## The fields, and where each one lives today
//!
//! Ported from the real settings in play, not invented — each is either a risk limit that today
//! arrives from a run-profile TOML's `[risk]` table (`vike_exec::ProfileRisk`) or — worse, and
//! precisely what Phase 5 removes — from an environment variable, or a compiled-in constant. The
//! per-field docs carry the provenance; no count is written here, because the one this paragraph
//! used to carry stopped matching the struct the moment a field was added.
//!
//! ## ⚠ A ceiling on this struct MUST be read by something
//!
//! `Policy`'s guarantee — a limit here cannot be widened from the environment — is worth nothing
//! when nothing READS the limit. `max_total_exposure` was such a field: declared here, validated by
//! [`Policy::apply`], accepted by [`PolicyPatch`]'s `deny_unknown_fields`, and consumed by no code
//! path in the workspace — while `crates/vike-mount/src/policy.rs`'s module doc asserted that it
//! "ARE consumed, but at the ORDER surfaces" alongside `max_notional_per_order`. Only the sibling
//! ever was. An operator who set it got validation, no complaint, and no cap.
//!
//! It is now a TOMBSTONE on [`PolicyPatch`] (see that field), and
//! `crates/vike-config/tests/policy_is_consumed.rs` is the standing gate: every field of this
//! struct must name a file that genuinely reads it, or carry a written admission that nothing does.
//!
//! **`rate.max_utilization` was the second one, and it is a tombstone too.** It clamped
//! [`crate::Preferences::rate_utilization`], which was itself read by nothing — a ceiling bounding a
//! dead value, with a warning line that announced the clamp of a number no code consumed. It
//! survived the first gate only because its row claimed a consumer INSIDE this crate (the clamp in
//! [`crate::load`]), which the newer `Config`/`Preferences`/`Flags` gate already rejects as
//! self-consumption; `policy_is_consumed.rs` now enforces the same rule, so the loophole is closed
//! rather than just this one field removed.
//!
//! ⚠ The bounds on `market_slippage` are **reused** from `vike-model`
//! ([`vike_model::market_slippage`]), never redefined here. A second copy of a bound is exactly the
//! split-brain that module's own doc warns about; this crate imports
//! [`MIN_MARKET_SLIPPAGE`]/[`MAX_MARKET_SLIPPAGE`] so a ceiling cannot drift from the clamp that
//! enforces it downstream. (`MIN_UTILIZATION`/`MAX_UTILIZATION` were imported for the same reason
//! and are gone with the field — [`vike_model::rate_limits`] still owns them, and is now the only
//! place that does.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use vike_model::account_keys::AccountLabel;
use vike_model::market_slippage::{MAX_MARKET_SLIPPAGE, MIN_MARKET_SLIPPAGE};
use vike_model::{HaltAdmit, VENUES};

use crate::error::ConfigError;
use crate::venue_mode::{VenueMode, VenuePolicy, did_you_mean, legal_modes, roster_id};

/// Default leverage ceiling: **1x, i.e. no leverage**.
///
/// A ceiling's default must be the conservative end, not the permissive one: a deployment that
/// ships no `policy.toml` gets the safest reading, and raising it is a deliberate, reviewable file
/// edit. Note this differs from `vike_exec::ProfileRisk::max_leverage`, whose `None` means "the
/// buying-power gate stays off entirely" — a default of "unbounded" is not a ceiling.
pub const DEFAULT_MAX_LEVERAGE: f64 = 1.0;

/// The dead-man timeout the mount-time warning RECOMMENDS for a 24/7 venue: 60 seconds of feed
/// silence. **A recommendation, never a default** — nothing in this workspace applies it silently.
///
/// ⚠ This constant was `DEFAULT_DEADMAN_TIMEOUT_MS` for one morning, and its doc argued that the
/// dead-man was "the one ceiling on [`Policy`] whose default is the ARMED end rather than the
/// absent one" — a safety default, because a switch an operator must remember to turn on is the
/// switch that is off during the outage. The ruling was reversed the same day, and
/// [`Policy::deadman_timeout_ms`] records why; the short form is that the switch observes SILENCE,
/// not the connection, so an armed default halts every session-bounded venue at every close and
/// any thin market in a quiet minute. What survives of the number is the recommendation: for a
/// venue that never closes, sixty seconds is coarse enough that a WS pump's ordinary re-dial does
/// not trip it and short enough that a book of resting quotes is pulled before a one-minute bar
/// closes on a venue the daemon can no longer see. The warning `vike-tradehub`'s live mount emits
/// when the key is ABSENT (`crates/vike-tradehub/src/tradehub_cli.rs`'s `deadman_absent_warning`)
/// prints this value as the paste-ready line, and `settings/policy.example.toml` shows it
/// commented out. An operator writes it; the binary never assumes it.
pub const RECOMMENDED_DEADMAN_TIMEOUT_MS: u64 = 60_000;

/// The spelling that DISABLES the dead-man EXPLICITLY: `deadman_timeout_ms = 0`.
///
/// An absent key is off too — see [`Policy::deadman_timeout_ms`] — but an absent key is off with a
/// mount-time WARNING, because the daemon cannot tell "the operator decided against it" from "the
/// operator never heard of it". Writing zero is how the operator says the decision was made, and
/// it is the ONLY spelling that silences the warning. Nothing between this and
/// [`MIN_DEADMAN_TIMEOUT_MS`] is legal. (This doc used to say "an explicit zero rather than an
/// absent key, because the key's default is armed"; the default is no longer armed, and zero's job
/// became the silence rather than the off.)
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

/// What the dead-man switch does when it trips — the FILE spelling of `vike_core::DeadManAction`.
///
/// ⚠ A `vike-config`-LOCAL enum, deliberately not the core type itself: `vike-config` sits below
/// `vike-core` in the layer order and must not depend on it. The mapping to the core type lives in
/// the crate that depends on BOTH — the composition root that constructs the switch
/// (`crates/vike-tradehub/src/tradehub_cli.rs`'s `deadman_config_from_policy`) — and it is an
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

/// Hard ceilings for this machine. Set by an org/admin in `<project>/settings/policy.toml`; overridable
/// by **nothing** below that.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Maximum leverage any strategy on this machine may request, `>= 1.0` (`10.0` = 10x).
    ///
    /// Was `[risk] max_leverage` in a run-profile TOML (`vike_exec::ProfileRisk::max_leverage`,
    /// which converts it to `RiskLimits::im_requirement` as `1.0 / max_leverage`). Defaults to
    /// [`DEFAULT_MAX_LEVERAGE`].
    pub max_leverage: f64,

    /// Maximum notional of any single order, in quote currency. `None` = uncapped.
    ///
    /// Was **`VIKE_MAX_ORDER_NOTIONAL`** (read by `vike-app`'s `main.rs` and by `vike-cli`'s
    /// `cmd/verbs.rs`) and **`VIKE_TRADEHUB_MAX_ORDER_NOTIONAL`** (the headless daemon's copy of
    /// the same idea). ⚠ This field is the clearest case the whole taxonomy exists for: an order
    /// -size ceiling that any exported variable can raise is not a ceiling. Phase 5 removes those
    /// env reads and makes this the only way to set it.
    pub max_notional_per_order: Option<f64>,

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
    /// ⚠ There is deliberately no third value. A `"refuse"` mode (no exemption at all) WAS the
    /// behaviour until 2026-08-06 and was removed because it disarmed the panic button in exactly
    /// the situations that halt on their own. An unknown spelling is rejected at load by name, with
    /// the legal set in the message — serde's own variant error, which is the whole of what an
    /// operator needs here (unlike the tombstones below, nobody ever had a working `refuse`).
    pub halt_admit: HaltAdmit,

    /// **The dead-man switch** — how long the live core's ingest may be SILENT before it cancels
    /// every resting order and (by default) engages HALT. Milliseconds. **`None` — the key ABSENT
    /// from the file — is OFF, with ONE warning at mount**; `Some(0)` ([`DEADMAN_DISABLED_MS`]) is
    /// OFF with no warning, the operator having decided; `Some(n)` arms it at `n`. Nothing arms it
    /// silently: there is no compiled-in timeout, and [`RECOMMENDED_DEADMAN_TIMEOUT_MS`] is the
    /// number the warning suggests, not one the binary applies.
    ///
    /// The mechanism is `crates/vike-core/src/runtime/deadman.rs` — implemented, unit-tested
    /// (`crates/vike-core/src/runtime/deadman_tests.rs`) and, until this key, constructed by no
    /// shipped binary. It is armed as `vike_core::CoreConfig::deadman` by ONE composition root:
    /// `crates/vike-tradehub/src/tradehub_cli.rs`'s `live_mount_with`, through
    /// `deadman_config_from_policy` (which folds both `None` and `Some(0)` to the core's `None`);
    /// the absent-key warning is `deadman_absent_warning` beside it, and fires for `None` alone.
    /// `crates/vike-config/tests/policy_is_consumed.rs` names that call site and opens the file to
    /// check.
    ///
    /// # The ruling, and its reversal the same day
    ///
    /// ⚠ **This key shipped one morning as `u64` with a compiled-in default of 60 s — ARMED unless
    /// an operator wrote `0`** — and its doc argued the safety case: a live daemon whose feed died
    /// with orders resting has no operator in front of it, and a switch an operator must remember
    /// to turn on is the switch that is off during the outage. That argument is still true, and the
    /// ruling was still reversed, because review found what the switch actually OBSERVES. It counts
    /// ingest — `CoreThread::dispatch` records every venue event, market tick, closed bar, quote,
    /// trade and book update, on any venue and any symbol — and deliberately NOTHING else: not a
    /// control command, not the periodic waker, and not `Ingest::StreamStatus`, the one message
    /// that says a socket is alive. A transport heartbeat never reaches the core at all (oanda's
    /// `HEARTBEAT` frame closes a transport gap and emits no quote —
    /// `crates/bridges/oanda/src/market_feed.rs`'s `fold_pricing_line`). So the switch cannot tell a
    /// dead socket from a quiet market or a closed one, and an ARMED DEFAULT bought three halts
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
    /// * **The cap that could not be raised past the close.** [`MAX_DEADMAN_TIMEOUT_MS`] is one day,
    ///   and the weekend FX close is ~48 h — so on an FX venue the only legal settings under the
    ///   morning's ruling were `0` or a switch that trips every Friday, and the doc had to tell the
    ///   operator so. A default that every FX operator must turn off before the first weekend is
    ///   not a safety default; it is a trap with a doc comment.
    ///
    /// **The correct dead-man observes the CONNECTION state, not silence** — that is the coming
    /// default, the separate M13 item, and it is not this key: it will trip on a socket the bridge
    /// reports dead and stay quiet through a market that merely closed. Until it lands, THIS switch
    /// is an OPT-IN tool for a 24/7 mount (a crypto perp venue that never closes and always ticks),
    /// off unless an operator writes the key, and the mount-time warning exists so that "off"
    /// is a decision rather than an oversight. Do not re-derive the morning's ruling from the
    /// safety argument alone; the argument was right and the mechanism was the wrong one for it.
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
    /// exec still lands on the paper exchange (no `[venues]` table, which mounts ALL PAPER; every
    /// venue capped `paper`; a `data_only = true` mount, the seam built precisely for rehearsing on
    /// a real feed) IS armed when the key is written, and a trip there cancels paper orders, sets
    /// `Halted` and writes the process's REAL HALT sentinel — which outlives the rehearsal and is
    /// the file the next genuinely-live mount's submit boundary refuses on. Whether the arming
    /// should instead key on exec actually being armed (`vike_run::armed_live_venues` is computed
    /// before the config, so the seam is reachable) is an owner decision neither ruling made; until
    /// it is, this paragraph states the shape as it is, and the operator page says the same.
    ///
    /// ⚠ **Journal replay: arming this changes NOTHING, and the derivation is short enough to write
    /// down so it is not re-derived.** `vike-core` writes the replay-refusing waker record when
    /// `journal_waker_records = submit_ack_timeout.is_some() || deadman.is_some()` — the
    /// `config_journal_waker_records` binding in `crates/vike-core/src/runtime/mod.rs`, read into
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
    /// itself on its constant. The bounds survived the reversal unchanged: they bound what an
    /// operator WRITES, and the reversal changed only what happens when they write nothing.
    pub deadman_timeout_ms: Option<u64>,

    /// What the dead-man does when it trips: `"cancel_all_and_halt"` (the default) or
    /// `"cancel_all"`. See [`DeadManActionSetting`] for the two, and for why the type is this
    /// crate's own rather than `vike_core::DeadManAction`. Inert while
    /// [`Self::deadman_timeout_ms`] is `None` or `Some(0)` — the switch is then not constructed,
    /// so there is nothing for an action to act on — and validated by serde alone (an unknown
    /// spelling fails at load naming the legal two).
    pub deadman_action: DeadManActionSetting,

    // vike:new-venue:note a new venue needs a COMMENTED line in `settings/policy.example.toml`
    // (`# venues.{venue} = "paper"`) — the scaffold scans `.rs` files, every `Cargo.toml` and the
    // justfile, so it cannot reach that template. The DEFAULT below needs no edit: it derives from
    // `vike_model::VENUES`, so the row appears the moment the roster does. Nothing goes silent —
    // `cargo test -p vike-cli --test settings_examples` names the key and the file.
    /// **Per-venue arming ceilings** — `paper` / `demo` / `live`, one entry per roster venue,
    /// defaulting to `paper` everywhere. Written as `[venues]` in `policy.toml`:
    ///
    /// ```toml
    /// [venues]
    /// bybit   = "live"
    /// binance = "demo"
    /// ```
    ///
    /// ⚠ **A CEILING, NOT A SELECTOR.** The effective tier is
    /// `min(this, whatever the existing mechanisms decide)` — [`VenueMode::cap`] is the one
    /// spelling of that fold. A `live` line does not put a venue live; it declines to STOP it going
    /// live. Setting a venue to `paper` is the only thing that changes an outcome, and it can only
    /// ever change it downward. That asymmetry is what makes the switch safe to introduce into a
    /// deployment already trading real money.
    ///
    /// Policy-class for the same reason as every field above, plus one specific to this key: it is
    /// the only place a deployment can state which venues it MEANS to trade on. The gate today is
    /// credential PRESENCE, so a `DERIBIT_LIVE_*` pair appended to `secrets.env` arms deribit on
    /// the next start with no file changed and no review — the escalation [`crate::arming`] closes
    /// for the four SWITCHED venues and structurally cannot close for the rest. A ceiling reachable
    /// from the environment would reopen it one shell export at a time.
    ///
    /// ⚠ **This is now ENFORCED.** Stage 2 landed the type; stage 3 folds it in at
    /// `crates/vike-mount/src/lib.rs`'s `make_engine_with_legs`, ABOVE the credential read — a
    /// `paper` venue returns the paper client without loading a credential, fetching an instrument
    /// grid or opening a socket. `crates/vike-config/tests/policy_is_consumed.rs`'s `venues` row
    /// names that fold and opens the file to check it; it was the written `Consumed::No` admission
    /// until the fold landed, which is what made the promotion unmissable.
    /// [`crate::venue_mode`] carries the whole argument, including why a feature-absent venue
    /// (`ibkr`, `polymarket`, `fxcm`) still gets a row.
    ///
    /// ⚠ **The first start after that fold, on a box with credentials and no `[venues]` table,
    /// drops EVERY venue to paper.** That is the correct ceiling and a silent one, so it is not
    /// silent: `vike_mount::venue_arming_migration` warns once, names this file and this key, lists
    /// the venues whose credentials WOULD have armed, and prints the block that restores them. It
    /// warns rather than refusing because a venue connection is a CAPABILITY
    /// (`docs/decisions/0013-degrade-vs-refuse.md`), and a daemon that will not start cannot flatten
    /// a position either. [`VenuePolicy::is_declared`] is the fact it self-silences on.
    pub venues: VenuePolicy,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            max_leverage: DEFAULT_MAX_LEVERAGE,
            max_notional_per_order: None,
            market_slippage: None,
            halt_admit: HaltAdmit::Admit,
            // ⚠ `None`, not a number. This line read `DEFAULT_DEADMAN_TIMEOUT_MS` (60 s, armed)
            // for one morning; `Policy::deadman_timeout_ms`'s doc records the reversal. Like every
            // other scalar here: a deployment with no `policy.toml` arms nothing.
            deadman_timeout_ms: None,
            deadman_action: DeadManActionSetting::default(),
            venues: VenuePolicy::default(),
        }
    }
}

/// The number of fields [`Policy`]'s `Serialize` emits — seven real ones plus the `accounts`
/// PROJECTION. Named so the impl below and its own gate cannot disagree about the count.
const POLICY_SERIALIZED_FIELDS: usize = 8;

/// ⚠ **Hand-written rather than derived, and the projection field is why.**
///
/// The real fields serialize exactly as `#[derive(Serialize)]` emitted them — same names, same
/// order, no `skip_serializing_if` anywhere (an `Option::None` reaches the format's own null, which
/// is what makes `policy.max_notional_per_order` a real leaf in JSON and an ABSENT key in TOML;
/// `crates/vike-config/tests/provenance.rs` depends on both).
///
/// The last, `accounts`, is a **projection of [`VenuePolicy`]'s per-account ceilings, not a second
/// copy of them**. It exists because those ceilings had no disclosure surface at all:
/// [`VenuePolicy`] is `#[serde(transparent)]` over its flat `{venue: mode}` map, so it cannot carry
/// a second serialized field, and a table nothing serializes gets no
/// `crates/vike-config/src/provenance.rs` row, no `vike-cli config show` cell and no
/// `settings/policy.example.toml` line — which is the declared-but-undisclosed shape this crate
/// exists to refuse. Deriving here and holding the map on `Policy` instead would put the same fact
/// in two places, which is the defect one level up.
///
/// The shape emitted is the FILE's own shape (`{venue: {LABEL: mode}}`), deliberately: `describe`
/// compares the resolved value against the raw file value to decide whether a row was `adjusted`,
/// and a rendering that did not match the file's would report every account table as adjusted.
impl Serialize for Policy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Policy", POLICY_SERIALIZED_FIELDS)?;
        s.serialize_field("max_leverage", &self.max_leverage)?;
        s.serialize_field("max_notional_per_order", &self.max_notional_per_order)?;
        s.serialize_field("market_slippage", &self.market_slippage)?;
        s.serialize_field("halt_admit", &self.halt_admit)?;
        s.serialize_field("deadman_timeout_ms", &self.deadman_timeout_ms)?;
        s.serialize_field("deadman_action", &self.deadman_action)?;
        s.serialize_field("venues", &self.venues)?;
        s.serialize_field("accounts", &self.venues.accounts_by_venue())?;
        s.end()
    }
}

/// The FILE shape of [`Policy`]: every field optional, so a layer patches only what it names.
///
/// Deserialization goes through this rather than through `Policy` itself, because the loader is
/// LAYERED: `#[serde(default)]` on the effective struct would reset a key the home file set back
/// to the code default whenever a later file omitted it. An all-`Option` patch expresses
/// "unmentioned = inherit", which is what a precedence chain means.
///
/// `deny_unknown_fields` is the other half of the contract: a mistyped key is rejected by NAME.
/// A typo'd setting that silently does nothing is worse than an error — the operator sets a
/// ceiling, sees no complaint, and does not have it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    /// See [`Policy::max_leverage`].
    pub max_leverage: Option<f64>,
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Policy`] field.
    ///
    /// `max_total_exposure` was a `Policy` ceiling that nothing in the workspace ever read (see
    /// this module's doc). Deleting it outright would let `deny_unknown_fields` reject the key
    /// with serde's generic "unknown field" — technically safe, since the load fails and nobody
    /// trades on a limit they do not have, but it tells an operator only that the key is wrong,
    /// not that the protection they wanted lives somewhere else and is already mandatory there.
    ///
    /// So the key is still PARSED, purely so [`Policy::apply`] can refuse it by name and point at
    /// the exposure cap that IS enforced: `max_total_exposure` in a **run profile's `[risk]`
    /// table** (`vike_exec::ProfileRisk` → `vike_exec::RiskLimits`), which `RiskGate::check_inner`
    /// evaluates on every order and which `vike_mount::require_live_risk_budget` already refuses
    /// to mount a live venue without.
    ///
    /// ⚠ The redirect must NOT be read as an equivalence, and the refusal message says so: that
    /// run-profile cap is scoped to **ONE symbol at ONE venue**, not the account — see
    /// `vike_exec::RiskLimits::max_total_exposure`'s doc and the scope pin
    /// `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
    /// `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`. **No account-aggregate ceiling
    /// exists in this workspace today.** Sending an operator who wanted a book-wide limit to a
    /// per-symbol key while calling it "aggregate" would re-create this very field's defect one
    /// file over: a number that looks like the protection asked for and is N× weaker.
    ///
    /// Same argument as [`crate::refuse_removed_env`]: a setting that silently does nothing is the
    /// failure mode worth engineering against, and the fix belongs in the error message.
    pub max_total_exposure: Option<f64>,
    /// See [`Policy::max_notional_per_order`].
    pub max_notional_per_order: Option<f64>,
    /// See [`Policy::market_slippage`].
    pub market_slippage: Option<f64>,
    /// See [`Policy::halt_admit`]. An unknown spelling fails HERE, at deserialization, naming the
    /// legal variants — so `halt_admit = "refuse"` is a load error rather than a mode an operator
    /// believes they configured.
    pub halt_admit: Option<HaltAdmit>,
    /// See [`Policy::deadman_timeout_ms`]. A `u64` rather than a float: the value is a count of
    /// milliseconds, and a fractional millisecond in a dead-man timeout is a typo, not a setting.
    /// Bounds are checked in [`Policy::apply`], where the file can be named.
    pub deadman_timeout_ms: Option<u64>,
    /// See [`Policy::deadman_action`]. An unknown spelling fails HERE, at deserialization, naming
    /// the legal variants — the [`PolicyPatch::halt_admit`] idiom.
    pub deadman_action: Option<DeadManActionSetting>,
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Policy`] field.
    ///
    /// `[rate] max_utilization` was a ceiling on `Preferences::rate_utilization`, and that
    /// preference was read by NOTHING: the target utilization every pacer actually spends comes
    /// from `vike_binance::family::klines::KlineSpec::utilization`, which every construction site
    /// fills with the compiled-in [`vike_model::rate_limits::DEFAULT_UTILIZATION`]. So the ceiling
    /// bounded a value no code consumed — the `max_total_exposure` shape one field over, with the
    /// extra twist that the clamp emitted a warning naming both halves and neither did anything.
    ///
    /// The concept did not disappear; it has a better home. [`vike_model::rate_limits`] owns the
    /// number, its bounds and its per-VENUE overrides ([`vike_model::RateLimitConfig`]) — which is
    /// the shape a pacing knob has to have, since one global float cannot say "spend 40 % on
    /// binance and 20 % on aster". The settings pair was a venue-blind duplicate of it, sitting in
    /// a crate no pacer can reach. It comes back when the value can genuinely arrive at
    /// `KlineSpec::utilization` — which needs the fetchers' uniform 4-arg shape to carry it and a
    /// settings-loading binary somewhere in `vike-backfill`, neither of which exists today.
    ///
    /// Same argument as [`crate::refuse_removed_env`] and as
    /// [`PolicyPatch::max_total_exposure`]: a setting that silently does nothing is the failure
    /// mode worth engineering against, and the fix belongs in the error message.
    pub rate: Option<RatePolicyPatch>,
    /// See [`Policy::venues`]. The `[venues]` table, as a raw `venue -> mode` map.
    ///
    /// ⚠ **`deny_unknown_fields` does NOT reach inside this map, and cannot.** It governs the
    /// FIELD names of this struct — `venues` is one field — while serde treats a map's KEYS as
    /// free-form data by construction. So the two halves of validation are split, deliberately and
    /// visibly:
    ///
    /// * the **MODE** is refused by serde HERE, at deserialization, with the legal spellings in the
    ///   message — the [`Policy::halt_admit`] idiom, and free because the value type is an enum;
    /// * the **VENUE** is refused by [`Policy::apply`], by hand, against
    ///   [`vike_model::VENUES`] — nothing else can, and a ceiling written against a venue that does
    ///   not exist applies to nothing while looking exactly like a ceiling that applies to
    ///   something.
    ///
    /// A `BTreeMap` rather than a `HashMap` so the refusal names the alphabetically-first offender
    /// deterministically: a load error that changes text between runs is one an operator cannot
    /// grep a log for.
    pub venues: Option<BTreeMap<String, VenueMode>>,
    /// **Per-ACCOUNT arming ceilings** — the `[accounts]` table, `venue -> LABEL -> mode`:
    ///
    /// ```toml
    /// [venues]
    /// hyperliquid = "live"      # the venue ceiling — caps every account below it
    ///
    /// [accounts.hyperliquid]
    /// ALT  = "live"             # a SECOND hyperliquid account, armed
    /// TEST = "demo"
    /// ```
    ///
    /// The label is the one from the credential key's
    /// [`vike_model::account_keys::ACCOUNT_SEPARATOR`] suffix — `HYPERLIQUID_LIVE_API_KEY__ALT` is
    /// account `ALT`. `vike_model::account_keys::accounts_in_store` is what enumerates them, from
    /// the STORE rather than from any list here: this table states ceilings for accounts, it does
    /// not declare that they exist.
    ///
    /// ⚠ **Three validations, in three different places, for the same structural reason
    /// [`PolicyPatch::venues`] gives:** `deny_unknown_fields` governs this struct's FIELD names and
    /// reaches inside no map. So the MODE is refused by serde (the value type is an enum), the
    /// VENUE by [`Policy::apply`] against [`vike_model::VENUES`], and the LABEL by
    /// [`Policy::apply`] against [`vike_model::account_keys::AccountLabel::parse`] — which is also
    /// what refuses the reserved `DEFAULT` spelling, whose ceiling is the venue's own line.
    ///
    /// ⚠ **STEP 1: this table is PARSED, VALIDATED, STORED — and folded by nothing.** It is
    /// accepted now, ahead of its consumer, deliberately: an unknown key is refused BY NAME and
    /// takes the whole `policy.toml` down with it, so a binary that does not yet understand
    /// `[accounts]` would turn an operator's forward-looking file into a total settings failure —
    /// every ceiling in it lost, not just the new one. Landing the parse first is what makes the
    /// step-2 rollout an ordinary upgrade. Because an inert settings key is the exact failure this
    /// workspace engineers against, [`crate::load`] emits a WARNING naming this table whenever one
    /// is present, so nobody can believe an account ceiling is armed while it is not.
    pub accounts: Option<BTreeMap<String, BTreeMap<String, VenueMode>>>,
}

/// The file shape of the removed `[rate]` table — kept ONLY so [`Policy::apply`] can refuse the key
/// by name. See [`PolicyPatch::rate`].
///
/// `max_utilization` stays an `Option<f64>` rather than becoming a catch-all so `[rate]` with any
/// OTHER key inside it still fails through `deny_unknown_fields` on the inner table, and the one
/// key an operator plausibly wrote gets the explanatory refusal.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatePolicyPatch {
    /// See [`PolicyPatch::rate`] — a tombstone, refused at apply time.
    pub max_utilization: Option<f64>,
}

impl Policy {
    /// Fold one file's patch in, validating each named value against `file` so the error can
    /// point at the exact file the operator must open.
    ///
    /// Validation happens HERE — at apply time, per layer — rather than once at the end, for
    /// exactly that reason: a merged value has no file attached to it any more, and
    /// `"max_leverage = 0.5 is below the minimum"` without a filename is a grep, not a fix.
    pub(crate) fn apply(&mut self, patch: PolicyPatch, file: &Path) -> Result<(), ConfigError> {
        if let Some(v) = patch.max_leverage {
            if !v.is_finite() {
                return Err(ConfigError::value(file, "max_leverage", v, "is not a finite number"));
            }
            if v < 1.0 {
                return Err(ConfigError::value(
                    file,
                    "max_leverage",
                    v,
                    "is below the minimum leverage 1 (1 = no leverage; a ceiling under 1x is not \
                     representable, and 0 would divide by zero deriving the margin requirement)",
                ));
            }
            self.max_leverage = v;
        }
        // TOMBSTONE — see `PolicyPatch::max_total_exposure`. Refused, never applied: this key was
        // a ceiling nothing read, so accepting it would re-create exactly the false belief its
        // removal exists to end. The message names the authority that DOES enforce the concept.
        if let Some(v) = patch.max_total_exposure {
            return Err(ConfigError::value(
                file,
                "max_total_exposure",
                v,
                "is no longer a policy key — it was read by NOTHING here, so it capped nothing. \
                 The exposure cap that IS enforced is `max_total_exposure` in your RUN PROFILE's \
                 `[risk]` table (vike_exec::ProfileRisk -> RiskLimits), which RiskGate evaluates \
                 on every order and which a live mount already refuses to start without — but it \
                 caps ONE SYMBOL at ONE VENUE, not the account, so size it per instrument. There \
                 is no account-aggregate ceiling today. Delete this line from policy.toml and set \
                 the per-symbol cap there",
            ));
        }
        if let Some(v) = patch.max_notional_per_order {
            check_positive(file, "max_notional_per_order", v)?;
            self.max_notional_per_order = Some(v);
        }
        if let Some(v) = patch.market_slippage {
            const KEY: &str = "market_slippage";
            if !v.is_finite() {
                return Err(ConfigError::value(file, KEY, v, "is not a finite number"));
            }
            // REJECTED, not clamped: a band is the worst price an emulated market order may reach,
            // and a silently-corrected one is a limit the operator believes they set and does not
            // have. `vike_bridge_core::market_slippage::resolve_market_slippage` clamps instead,
            // because by then there is no file to name — this edge is the one that can say WHERE.
            if v > MAX_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    v,
                    &format!(
                        "exceeds the allowed maximum {MAX_MARKET_SLIPPAGE} (a band is a FRACTION: \
                         0.002 = 0.2%). The maximum is the widest band already compiled in, so this \
                         key can only tighten a venue, never widen one"
                    ),
                ));
            }
            if v < MIN_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    v,
                    &format!(
                        "is below the allowed minimum {MIN_MARKET_SLIPPAGE} — a band this tight is \
                         not marketable, so the emulated order cancels unfilled, which on a tripped \
                         stop is a protective exit that did not exit"
                    ),
                ));
            }
            self.market_slippage = Some(v);
        }
        // No numeric validation: the value space is the enum, and an illegal spelling has already
        // failed at deserialization with the legal set in the message. An unmentioned key inherits,
        // which for this field means the compiled-in `admit` — today's behaviour on every venue.
        if let Some(v) = patch.halt_admit {
            self.halt_admit = v;
        }
        // The dead-man timeout. REJECTED at the file, never clamped, on both sides: the value
        // becomes `vike_core::DeadManConfig::timeout` verbatim (that type clamps only a zero to
        // 1 ms, and a zero never reaches it — it is the explicit-off spelling, folded to `None` by
        // the composition root, as an absent key is). A sub-second value is a false halt on any
        // quiet second; a multi-day one is a switch that will never fire. Both are typos an
        // operator must be told about, and this edge is the one that can say WHERE. An unmentioned
        // key INHERITS the layer below — which for the first layer is `None`, so a file that
        // never names the key leaves it absent, and the mount's warning can see that it was.
        if let Some(v) = patch.deadman_timeout_ms {
            const KEY: &str = "deadman_timeout_ms";
            if v != DEADMAN_DISABLED_MS && v < MIN_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} is below the minimum armed timeout {MIN_DEADMAN_TIMEOUT_MS} ms (one \
                         second). The switch trips on SILENCE across the whole core's ingest, and a \
                         live instrument goes sub-second stretches without a tick routinely — a \
                         timeout this short is a false halt on any quiet second. Write \
                         {DEADMAN_DISABLED_MS} to disable the switch outright, or \
                         {MIN_DEADMAN_TIMEOUT_MS} or more to arm it"
                    ),
                });
            }
            if v > MAX_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} exceeds the maximum {MAX_DEADMAN_TIMEOUT_MS} ms (one day). A dead-man \
                         that waits longer than a day is not a dead-man — a feed silent that long \
                         has left a book resting through an entire session, which is what the \
                         switch exists to prevent. This is usually a unit slip (seconds written as \
                         milliseconds); the value is in MILLISECONDS"
                    ),
                });
            }
            self.deadman_timeout_ms = Some(v);
        }
        // No numeric validation: the value space is the enum, and an illegal spelling has already
        // failed at deserialization with the legal set in the message.
        if let Some(v) = patch.deadman_action {
            self.deadman_action = v;
        }
        // TOMBSTONE — see `PolicyPatch::rate`. Refused, never applied: this ceiling clamped
        // `Preferences::rate_utilization`, which nothing read, so it bounded nothing.
        if let Some(v) = patch.rate.and_then(|rate| rate.max_utilization) {
            return Err(ConfigError::value(
                file,
                "rate.max_utilization",
                v,
                "is no longer a policy key — it clamped `preferences.rate_utilization`, which was \
                 read by NOTHING, so this ceiling bounded nothing. Every pacer takes its target \
                 fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION \
                 instead. Delete this line (and `rate_utilization` from preferences.toml); the \
                 knob returns, per-venue, when vike_model::RateLimitConfig can reach a pacer",
            ));
        }
        // The `[venues]` ceilings. The MODE has already been validated by serde (the value type is
        // an enum, so an illegal spelling never reaches here); what is left is the VENUE, which
        // `deny_unknown_fields` structurally cannot see — see `PolicyPatch::venues`.
        //
        // An unmentioned venue INHERITS, like every other key: a `[venues]` table naming two venues
        // leaves the other twelve at whatever the layer below said, which for the only layer that
        // exists is the `paper` default.
        if let Some(venues) = patch.venues {
            for (name, mode) in venues {
                let Some(id) = roster_id(&name) else {
                    return Err(unknown_venue(file, &name, mode));
                };
                self.venues.set(id, mode);
            }
        }
        // The `[accounts]` per-account ceilings. Same split as `[venues]` above, one level deeper:
        // serde has already refused an illegal MODE, so what is left is the VENUE and the LABEL.
        // An unnamed account inherits nothing — `VenuePolicy::account` resolves a labelled account
        // with no line to `paper`, which is the safe end and the one this file's absence must mean.
        if let Some(accounts) = patch.accounts {
            for (name, labelled) in accounts {
                let Some(id) = roster_id(&name) else {
                    return Err(unknown_account_venue(file, &name));
                };
                for (label, mode) in labelled {
                    let parsed = AccountLabel::parse(&label).map_err(|e| {
                        bad_account_label(file, &name, &label, mode, &e.to_string())
                    })?;
                    // `parse` returns `Named` for everything it accepts, so this is `Some`.
                    // Refusing rather than dropping the line keeps that assumption from failing
                    // silently if it ever stops holding.
                    let Some(text) = parsed.text() else {
                        return Err(bad_account_label(
                            file,
                            &name,
                            &label,
                            mode,
                            "resolves to the default account, whose ceiling is the venue's own line",
                        ));
                    };
                    self.venues.set_account(id, text, mode);
                }
            }
        }
        Ok(())
    }
}

/// The refusal for an `[accounts]` table naming no venue — the `[venues]` refusal one level down,
/// and separate from it because the KEY it must blame is different and an operator greps for the
/// key they typed.
fn unknown_account_venue(file: &Path, name: &str) -> ConfigError {
    let hint = match did_you_mean(name) {
        Some(id) if name.to_ascii_lowercase() == id => {
            format!(" Did you mean `{id}`? Venue ids are lowercase.")
        }
        Some(id) => format!(" Did you mean `{id}`?"),
        None => String::new(),
    };
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("accounts.{name}"),
        message: format!(
            "names no venue vike has a bridge for, so every account ceiling under it would apply \
             to NOTHING while reading like it applies to something.{hint} The venues are: {}.",
            VENUES.join(", "),
        ),
    }
}

/// The refusal for an `[accounts]` key that is not a legal account label.
///
/// Refused rather than ignored, for the reason every refusal in this file is: an operator who
/// wrote `[accounts.bybit] alt = "paper"` believes they capped an account, while the credential
/// store they wrote the labelled key into spells the label the other way. A loader that silently
/// dropped the line — or quietly uppercased it — would hand them positive confirmation of a ceiling
/// they do not have, and here the belief runs the dangerous way round.
///
/// `reason` is [`vike_model::account_keys::AccountKeyError`]'s own rendering, so the legal shape a
/// refusal describes and the shape the parser accepts cannot drift.
fn bad_account_label(
    file: &Path,
    venue: &str,
    label: &str,
    mode: VenueMode,
    reason: &str,
) -> ConfigError {
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("accounts.{venue}.{label}"),
        message: format!(
            "\"{mode}\" is stated for an account label vike cannot address: {reason}. An account \
             label is the suffix of that account's credential keys, after the double underscore. \
             (The legal modes are {}.)",
            legal_modes(),
        ),
    }
}

/// The refusal for a `[venues]` key naming no venue — by NAME, with the roster in the message.
///
/// Refused rather than ignored, for the reason every tombstone above is refused: an operator who
/// wrote `venues.bybitt = "paper"` believes they capped bybit, and a load that accepted the line
/// would hand them positive confirmation of a ceiling they do not have. That is worse here than for
/// a tombstone, because the belief runs the DANGEROUS way round — they think a venue is held to
/// paper and it is not.
///
/// The roster is rendered from [`vike_model::VENUES`] and the modes from
/// [`crate::venue_mode::legal_modes`], so neither list can drift from what the loader accepts.
fn unknown_venue(file: &Path, name: &str, mode: VenueMode) -> ConfigError {
    let hint = match did_you_mean(name) {
        // Not repaired, only suggested — see `did_you_mean`'s doc. The lowercase remark is added
        // ONLY when case is the whole difference: appended to a plain typo it reads as an
        // explanation of that typo and sends the operator looking at the wrong thing.
        Some(id) if name.to_ascii_lowercase() == id => {
            format!(" Did you mean `{id}`? Venue ids are lowercase.")
        }
        Some(id) => format!(" Did you mean `{id}`?"),
        None => String::new(),
    };
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("venues.{name}"),
        message: format!(
            "\"{mode}\" names no venue vike has a bridge for, so this ceiling would apply to \
             NOTHING while reading like it applies to something.{hint} The venues are: {}. \
             (The legal modes are {}.)",
            VENUES.join(", "),
            legal_modes(),
        ),
    }
}

/// A ceiling denominated in money must be a finite, strictly positive number: `0` would deny
/// every order (a silent halt), and a negative one is meaningless.
fn check_positive(file: &Path, key: &str, v: f64) -> Result<(), ConfigError> {
    if !v.is_finite() {
        return Err(ConfigError::value(file, key, v, "is not a finite number"));
    }
    if v <= 0.0 {
        return Err(ConfigError::value(
            file,
            key,
            v,
            "must be greater than 0 (a ceiling of 0 denies every order — omit the key to leave \
             it uncapped)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> &'static Path {
        Path::new("policy.toml")
    }

    #[test]
    fn defaults_are_the_conservative_end() {
        let p = Policy::default();
        assert_eq!(p.max_leverage, 1.0);
        assert_eq!(p.max_notional_per_order, None);
        // `None`, NOT a number: every venue keeps its own historical band, byte-identically. A
        // default here would re-price the market orders of every existing deployment on upgrade.
        assert_eq!(p.market_slippage, None);
        // ⚠ `Admit`, NOT the stricter `Verify`, and here the conservative end is the PERMISSIVE
        // one — for once. `Verify` narrows what a halt lets OUT, i.e. what an operator can still
        // close with, so defaulting to it would change the behaviour of every existing deployment's
        // kill switch from a file nobody wrote. It is opt-in for the same reason `market_slippage`
        // is `None`.
        assert_eq!(p.halt_admit, HaltAdmit::Admit);
        // ⚠ ABSENT, like every other scalar. For one morning this assertion read
        // `assert_eq!(p.deadman_timeout_ms, DEFAULT_DEADMAN_TIMEOUT_MS)` — "the ONE default that
        // is the ARMED end" — and existed to make an "align it with the other scalars" edit a red
        // test. The alignment is now the ruling (the field's doc records why: the switch observes
        // silence, so an armed default halts every session-bounded venue at every close), and this
        // assertion is what makes a future "arm it by default, it is a safety switch" edit the red
        // test instead. `None` and `Some(0)` are DISTINCT here: the mount warns on the first only.
        assert_eq!(p.deadman_timeout_ms, None, "a policy that says nothing arms NO dead-man");
        assert_ne!(p.deadman_timeout_ms, Some(DEADMAN_DISABLED_MS), "absent is not explicit-off");
        assert_eq!(p.deadman_action, DeadManActionSetting::CancelAllAndHalt);
    }

    // -- the dead-man switch ------------------------------------------------------------------

    /// The key loads from the file, `0` is the EXPLICIT-off spelling and loads as `Some(0)` (so
    /// the mount can tell it from an absent key and stay quiet), and an unmentioned key INHERITS
    /// (the layered-patch contract — which here runs the safe way round too: a later layer that
    /// says nothing cannot re-arm a switch an earlier one disabled, nor arm one no layer named).
    #[test]
    fn the_deadman_timeout_loads_disables_on_zero_and_inherits_when_unmentioned() {
        let mut p = Policy::default();
        // A patch that never names the key leaves it ABSENT — this is the "file with no line"
        // case the mount warns on, and it must not read as `Some(0)`.
        p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
        assert_eq!(p.deadman_timeout_ms, None, "unmentioned on the first layer stays absent");

        let patch: PolicyPatch = toml::from_str("deadman_timeout_ms = 5000\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.deadman_timeout_ms, Some(5000));

        let patch: PolicyPatch = toml::from_str("deadman_timeout_ms = 0\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(
            p.deadman_timeout_ms,
            Some(DEADMAN_DISABLED_MS),
            "zero is legal, means OFF, and is DISTINCT from absent"
        );

        p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
        assert_eq!(
            p.deadman_timeout_ms,
            Some(0),
            "an unmentioned key inherits — it must not re-arm"
        );
    }

    /// Every value in `1..1000` is refused BY NAME, naming the file — a sub-second dead-man is a
    /// false halt on any quiet second — and the refusal tells the operator both legal moves.
    #[test]
    fn a_sub_second_deadman_timeout_is_rejected_naming_file_key_and_the_two_legal_moves() {
        for v in [1u64, 2, 500, 999] {
            let err = Policy::default()
                .apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
                .expect_err("{v} ms must be refused");
            let msg = err.to_string();
            assert!(msg.starts_with(&format!("policy.toml: deadman_timeout_ms = {v} ")), "{msg}");
            assert!(msg.contains("below the minimum armed timeout 1000 ms"), "{msg}");
            assert!(msg.contains("false halt"), "says what the value would DO: {msg}");
            assert!(msg.contains("Write 0 to disable"), "names the OFF spelling: {msg}");
        }
    }

    /// Above a day is refused too — a switch that never fires — and the message names the unit,
    /// because seconds-written-as-milliseconds is the typo that lands here.
    #[test]
    fn a_deadman_timeout_above_a_day_is_rejected_and_names_the_unit() {
        // ⚠ `600_000_000` (a "600 s" slip written as microseconds) rather than `60_000_000`: the
        // latter is 16.7 HOURS, inside the day, and the first draft of this test used it — the
        // value was accepted and the test failed for the right reason. A day is a wide ceiling.
        for v in [MAX_DEADMAN_TIMEOUT_MS + 1, 600_000_000, u64::MAX] {
            let err = Policy::default()
                .apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
                .expect_err("{v} ms must be refused");
            let msg = err.to_string();
            assert!(msg.starts_with(&format!("policy.toml: deadman_timeout_ms = {v} ")), "{msg}");
            assert!(msg.contains("exceeds the maximum 86400000 ms"), "{msg}");
            assert!(msg.contains("MILLISECONDS"), "names the unit: {msg}");
        }
    }

    /// Both bounds are inclusive on the legal side, and the disabling zero sits outside the range
    /// rather than at its bottom — pinned so a `<=` slip on either comparison fails here.
    #[test]
    fn the_deadman_timeout_bounds_are_inclusive_and_zero_is_its_own_case() {
        for v in [
            DEADMAN_DISABLED_MS,
            MIN_DEADMAN_TIMEOUT_MS,
            RECOMMENDED_DEADMAN_TIMEOUT_MS,
            MAX_DEADMAN_TIMEOUT_MS,
        ] {
            let mut p = Policy::default();
            p.apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
                .unwrap_or_else(|e| panic!("{v} must be accepted: {e}"));
            assert_eq!(p.deadman_timeout_ms, Some(v));
        }
        // …and the recommendation is itself a legal armed value, or the warning would be
        // recommending a line the loader refuses.
        assert!(
            (MIN_DEADMAN_TIMEOUT_MS..=MAX_DEADMAN_TIMEOUT_MS)
                .contains(&RECOMMENDED_DEADMAN_TIMEOUT_MS)
        );
    }

    /// The action loads by its file spelling, defaults to the HALTING one, and an unknown
    /// spelling is refused at DESERIALIZATION naming the legal two — so `"halt"` or `"cancel"`
    /// cannot load as something else.
    #[test]
    fn the_deadman_action_loads_and_an_unknown_spelling_names_the_legal_ones() {
        let mut p = Policy::default();
        let patch: PolicyPatch =
            toml::from_str("deadman_action = \"cancel_all\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.deadman_action, DeadManActionSetting::CancelAll);

        let patch: PolicyPatch =
            toml::from_str("deadman_action = \"cancel_all_and_halt\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.deadman_action, DeadManActionSetting::CancelAllAndHalt);

        for bad in ["halt", "cancel", "CancelAll", "CANCEL_ALL", "off", "true", ""] {
            let err = toml::from_str::<PolicyPatch>(&format!("deadman_action = {bad:?}\n"))
                .expect_err("{bad} must not load");
            let msg = err.to_string();
            for legal in [DeadManActionSetting::CancelAllAndHalt, DeadManActionSetting::CancelAll] {
                assert!(
                    msg.contains(legal.as_str()),
                    "{bad}: {} missing from {msg}",
                    legal.as_str()
                );
            }
        }
    }

    /// The serialized shape carries both keys as plain leaves — the timeout as JSON `null` when
    /// absent (the `max_notional_per_order` shape: a real leaf in JSON, an ABSENT key in TOML,
    /// which is what lets `config show` report `default` for it) and as the integer when written;
    /// the action as a string. The shape `crates/vike-config/src/provenance.rs`'s rows path into
    /// and the templates are gated against. Pinned here because the `Serialize` impl is
    /// hand-written, so a field added to the struct and forgotten there would otherwise vanish
    /// from every disclosure surface at once.
    #[test]
    fn the_deadman_keys_serialize_as_leaves() {
        let json = serde_json::to_value(Policy::default()).expect("serializes");
        assert_eq!(json["deadman_timeout_ms"], serde_json::Value::Null, "absent is null, not 0");
        assert_eq!(json["deadman_action"], serde_json::json!("cancel_all_and_halt"));

        let written = Policy { deadman_timeout_ms: Some(60_000), ..Policy::default() };
        let json = serde_json::to_value(written).expect("serializes");
        assert_eq!(json["deadman_timeout_ms"], serde_json::json!(60_000));
    }

    /// The knob loads from the file it is supposed to live in, both ways round, and an unmentioned
    /// key inherits rather than resetting (the layered-patch contract).
    #[test]
    fn halt_admit_loads_from_the_policy_file_and_inherits_when_unmentioned() {
        let mut p = Policy::default();
        let patch: PolicyPatch = toml::from_str("halt_admit = \"verify\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.halt_admit, HaltAdmit::Verify);

        // A later layer naming only another key must not reset it back to `admit`.
        p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
        assert_eq!(p.halt_admit, HaltAdmit::Verify, "an unmentioned key inherits");

        // …and it can be set back explicitly.
        let patch: PolicyPatch = toml::from_str("halt_admit = \"admit\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.halt_admit, HaltAdmit::Admit);
    }

    /// An illegal mode is refused BY NAME at load, with the legal set in the message — including
    /// `"refuse"`, the mode this design deliberately does not offer (it disarmed the panic button
    /// in exactly the situations that halt on their own). An operator must not be able to write a
    /// mode that silently loads as something else.
    #[test]
    fn an_unknown_halt_admit_mode_is_rejected_and_names_the_legal_ones() {
        for bad in ["refuse", "Verify", "VERIFY", "off", "true", ""] {
            let err = toml::from_str::<PolicyPatch>(&format!("halt_admit = {bad:?}\n"))
                .expect_err("{bad} must not load");
            let msg = err.to_string();
            assert!(msg.contains("admit") && msg.contains("verify"), "{bad}: {msg}");
        }
    }

    /// The tombstone REFUSES rather than ignoring — and the refusal has to be actionable, because
    /// the operator who wrote this key wanted a real protection that exists in a different file.
    #[test]
    fn the_removed_max_total_exposure_key_is_refused_and_names_where_it_lives_now() {
        let err = Policy::default()
            .apply(PolicyPatch { max_total_exposure: Some(25_000.0), ..Default::default() }, file())
            .expect_err("a ceiling nothing reads must not load silently");
        let msg = err.to_string();
        assert!(msg.starts_with("policy.toml: max_total_exposure = 25000 "), "{msg}");
        assert!(msg.contains("no longer a policy key"), "says it is gone: {msg}");
        assert!(msg.contains("[risk]"), "names the table that replaces it: {msg}");
        assert!(msg.contains("RiskGate"), "names what actually enforces it: {msg}");
        // …and the redirect must NOT be sold as an equivalence. The operator who wrote THIS key
        // wanted a book-wide ceiling; the run-profile key they are sent to caps ONE symbol at ONE
        // venue (`vike_exec::RiskLimits::max_total_exposure`'s doc is the authority, pinned by
        // `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
        // `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`). Describing it as aggregate
        // would re-create this very field's defect one file over: a number that looks like the
        // protection asked for and is N× weaker.
        assert!(
            msg.contains("ONE SYMBOL") && msg.contains("not the account"),
            "the refusal must state the replacement's REAL scope: {msg}"
        );
        assert!(
            msg.contains("no account-aggregate ceiling"),
            "must say plainly that the protection this operator wanted does not exist: {msg}"
        );
    }

    /// …and it is refused on its own merits, not merely as a side effect of validation: a value
    /// that would have PASSED `check_positive` still fails, and an out-of-range one does not
    /// silently take the old "not finite / not positive" path instead.
    #[test]
    fn the_tombstone_refuses_every_value_including_ones_that_used_to_be_valid() {
        for v in [1.0, 25_000.0, f64::MAX, -1.0, 0.0, f64::NAN] {
            let err = Policy::default()
                .apply(PolicyPatch { max_total_exposure: Some(v), ..Default::default() }, file())
                .expect_err("{v} must be refused");
            assert!(err.to_string().contains("no longer a policy key"), "{v}: {err}");
        }
    }

    /// The tombstone is NOT a general amnesty on unknown keys: `deny_unknown_fields` still rejects
    /// a genuine typo by name, which is the property that makes a mistyped ceiling visible.
    #[test]
    fn a_mistyped_key_is_still_rejected_by_name() {
        let err = toml::from_str::<PolicyPatch>("max_total_exposur = 1.0\n")
            .expect_err("a typo must not be accepted");
        assert!(err.to_string().contains("max_total_exposur"), "{err}");
    }

    #[test]
    fn a_market_slippage_band_inside_the_range_is_accepted() {
        let mut p = Policy::default();
        p.apply(PolicyPatch { market_slippage: Some(0.002), ..Default::default() }, file())
            .unwrap();
        assert_eq!(p.market_slippage, Some(0.002));
        // Both endpoints are legal.
        for v in [MIN_MARKET_SLIPPAGE, MAX_MARKET_SLIPPAGE] {
            let mut p = Policy::default();
            p.apply(PolicyPatch { market_slippage: Some(v), ..Default::default() }, file())
                .unwrap();
            assert_eq!(p.market_slippage, Some(v));
        }
    }

    /// The money case: "set it to 50% so orders always fill" is refused BY NAME, at the file, rather
    /// than silently clamped — an operator who wrote it must learn that they do not have it.
    #[test]
    fn a_market_slippage_band_above_the_ceiling_is_rejected_naming_file_key_and_bound() {
        let err = Policy::default()
            .apply(PolicyPatch { market_slippage: Some(0.5), ..Default::default() }, file())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.starts_with("policy.toml: market_slippage = 0.5 "), "{msg}");
        assert!(msg.contains("exceeds the allowed maximum 0.05"), "{msg}");
    }

    /// A band too tight to cross the book cancels unfilled — on a tripped stop, an exit that did not
    /// happen. Also refused, and the message says why rather than just quoting a number.
    #[test]
    fn a_market_slippage_band_below_the_floor_is_rejected() {
        let err = Policy::default()
            .apply(PolicyPatch { market_slippage: Some(0.0), ..Default::default() }, file())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("below the allowed minimum 0.001"), "{msg}");
        assert!(msg.contains("did not exit"), "{msg}");
    }

    #[test]
    fn a_non_finite_market_slippage_band_is_rejected() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = Policy::default()
                .apply(PolicyPatch { market_slippage: Some(bad), ..Default::default() }, file())
                .unwrap_err();
            assert!(err.to_string().contains("market_slippage"), "{err}");
        }
    }

    /// The two edges must agree on WHICH values are in bounds: this file edge (which rejects,
    /// naming the key) and the wire edge (`vike_bridge_core::market_slippage`, which clamps) both
    /// answer to `vike_model`'s range test. Driven off that predicate rather than restating the
    /// numbers, so a bound that moved in one place and not the other fails here.
    #[test]
    fn accepted_bands_are_exactly_those_vike_model_calls_usable() {
        use vike_model::market_slippage::is_usable_market_slippage;
        for v in [
            0.0,
            0.0009,
            MIN_MARKET_SLIPPAGE,
            0.002,
            0.01,
            MAX_MARKET_SLIPPAGE,
            0.0500001,
            0.5,
            f64::NAN,
            f64::INFINITY,
        ] {
            let accepted = Policy::default()
                .apply(PolicyPatch { market_slippage: Some(v), ..Default::default() }, file())
                .is_ok();
            assert_eq!(
                accepted,
                is_usable_market_slippage(v),
                "the file edge and vike-model disagree about {v}"
            );
        }
    }

    #[test]
    fn an_unmentioned_key_is_inherited_not_reset() {
        let mut p = Policy::default();
        p.apply(PolicyPatch { max_leverage: Some(10.0), ..Default::default() }, file()).unwrap();
        // A second patch naming only the band must not reset max_leverage to the code default.
        p.apply(PolicyPatch { market_slippage: Some(0.01), ..Default::default() }, file()).unwrap();
        assert_eq!(p.max_leverage, 10.0);
        assert_eq!(p.market_slippage, Some(0.01));
    }

    /// The SECOND tombstone: `[rate] max_utilization` was a ceiling on a preference nothing read.
    ///
    /// It is refused rather than dropped for the same reason `max_total_exposure` is — an operator
    /// who wrote it believes a pacing limit is armed, and `deny_unknown_fields`' generic "unknown
    /// field" would tell them only that the key is wrong. Every value is refused, including ones
    /// that used to VALIDATE (`0.6`) and ones that used to be rejected by the bounds (`1.5`,
    /// `0.001`): the key means nothing now, so accepting any of them would be the false confirmation
    /// this whole taxonomy exists to prevent.
    #[test]
    fn the_removed_rate_max_utilization_key_is_refused_and_names_where_the_number_lives_now() {
        for v in [0.6, 1.5, 0.001, 0.95, 0.05] {
            let err = Policy::default()
                .apply(
                    PolicyPatch {
                        rate: Some(RatePolicyPatch { max_utilization: Some(v) }),
                        ..Default::default()
                    },
                    file(),
                )
                .expect_err("a ceiling that bounded nothing must not load silently");
            let msg = err.to_string();
            assert!(msg.starts_with("policy.toml: rate.max_utilization = "), "{msg}");
            assert!(msg.contains("no longer a policy key"), "says it is gone: {msg}");
            assert!(
                msg.contains("preferences.rate_utilization"),
                "names the dead value it clamped: {msg}"
            );
            assert!(
                msg.contains("DEFAULT_UTILIZATION"),
                "names where the number really comes from: {msg}"
            );
        }
    }

    /// An EMPTY `[rate]` table is not an operator claiming a ceiling, so it loads — the refusal is
    /// keyed on the value, exactly like the `max_total_exposure` tombstone.
    #[test]
    fn an_empty_rate_table_is_not_refused() {
        Policy::default()
            .apply(
                PolicyPatch { rate: Some(RatePolicyPatch::default()), ..Default::default() },
                file(),
            )
            .expect("`[rate]` with nothing in it claims nothing");
    }

    #[test]
    fn sub_1x_leverage_is_rejected() {
        let err = Policy::default()
            .apply(PolicyPatch { max_leverage: Some(0.0), ..Default::default() }, file())
            .unwrap_err();
        assert!(err.to_string().starts_with("policy.toml: max_leverage = 0 "), "{err}");
    }

    // -- `[venues]` — the per-venue arming ceilings ------------------------------------------------

    /// **The default is a FILLED map: one `paper` entry per roster venue.** Exhaustive over
    /// `vike_model::VENUES`, so a new bridge crate reddens this until it has a ceiling — the same
    /// contract every per-venue capability table carries.
    ///
    /// ⚠ Not merely "the default is safe": an EMPTY map would also be safe to READ and would make
    /// `crates/vike-config/tests/provenance.rs`'s completeness gate pass vacuously (its walk only
    /// records a leaf at a non-object node), so the setting would exist and `vike-cli config show`
    /// would never mention it. The length assertion is what distinguishes the two.
    #[test]
    fn every_roster_venue_defaults_to_paper() {
        let p = Policy::default();
        assert_eq!(p.venues.len(), VENUES.len(), "one entry per roster venue, never an empty map");
        for venue in VENUES {
            assert_eq!(p.venues.get(venue), VenueMode::Paper, "{venue} must default to paper");
        }
    }

    /// A `[venues]` table sets the venues it names and leaves every other one alone — the layered
    /// -patch contract, applied one level down inside a map.
    #[test]
    fn a_venues_table_sets_what_it_names_and_inherits_the_rest() {
        let mut p = Policy::default();
        let patch: PolicyPatch =
            toml::from_str("[venues]\nbybit = \"live\"\nbinance = \"demo\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.venues.get("bybit"), VenueMode::Live);
        assert_eq!(p.venues.get("binance"), VenueMode::Demo);
        assert_eq!(p.venues.get("okx"), VenueMode::Paper, "an unnamed venue keeps the default");

        // A later layer naming only ANOTHER venue must not reset the first two.
        let patch: PolicyPatch = toml::from_str("[venues]\nokx = \"demo\"\n").expect("parses");
        p.apply(patch, file()).unwrap();
        assert_eq!(p.venues.get("bybit"), VenueMode::Live, "an unmentioned venue inherits");
        assert_eq!(p.venues.get("okx"), VenueMode::Demo);

        // …and a patch naming no `[venues]` table at all leaves the whole map alone.
        p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
        assert_eq!(p.venues.get("bybit"), VenueMode::Live);
    }

    /// **An unknown VENUE is refused by name, with the roster in the message** — the half
    /// `deny_unknown_fields` structurally cannot do, because serde treats a map's keys as data.
    ///
    /// The dangerous direction is the reason: an accepted `venues.bybitt = "paper"` reads to its
    /// author as "bybit is held to paper", and bybit would not be.
    #[test]
    fn an_unknown_venue_is_refused_by_name_and_the_roster_is_listed() {
        for bad in ["bybitt", "kraken", "", "binance-futures"] {
            let text = format!("[venues]\n{bad:?} = \"paper\"\n");
            let patch: PolicyPatch =
                toml::from_str(&text).expect("the MODE is legal, so it parses");
            let err = Policy::default()
                .apply(patch, file())
                .expect_err("a ceiling on a venue that does not exist must not load");
            let msg = err.to_string();
            assert!(msg.starts_with(&format!("policy.toml: venues.{bad} = ")), "{msg}");
            assert!(msg.contains("names no venue"), "{msg}");
            assert!(msg.contains("apply to NOTHING"), "says what the line would do: {msg}");
            for venue in VENUES {
                assert!(msg.contains(*venue), "the roster must be listed; {venue} missing: {msg}");
            }
        }
    }

    /// A CASE mismatch is refused too — and the refusal suggests the canonical id rather than
    /// silently repairing it. A spelling the loader fixes on your behalf is a spelling nobody
    /// learns, and the next reader of the file greps the tree for a key that is not there.
    #[test]
    fn a_miscased_venue_is_refused_and_the_canonical_id_is_suggested() {
        let patch: PolicyPatch = toml::from_str("[venues]\nBybit = \"live\"\n").expect("parses");
        let err = Policy::default().apply(patch, file()).expect_err("ids are lowercase");
        let msg = err.to_string();
        assert!(msg.contains("Did you mean `bybit`?"), "{msg}");
        assert!(msg.contains("lowercase"), "{msg}");
    }

    /// An unknown MODE is refused at DESERIALIZATION, naming the legal three — the `halt_admit`
    /// idiom, free because the value type is an enum. `"true"`/`"1"`/`"mainnet"` are the plausible
    /// wrong spellings; none of them may load as something else.
    #[test]
    fn an_unknown_mode_is_rejected_and_names_the_legal_ones() {
        for bad in ["mainnet", "testnet", "Live", "LIVE", "true", "1", ""] {
            let text = format!("[venues]\nbybit = {bad:?}\n");
            let err = toml::from_str::<PolicyPatch>(&text).expect_err("{bad} must not load");
            let msg = err.to_string();
            for mode in VenueMode::ALL {
                assert!(msg.contains(mode.as_str()), "{bad}: {mode} missing from {msg}");
            }
        }
    }

    /// An EMPTY `[venues]` table claims nothing, so it loads — the same rule as the empty `[rate]`
    /// table, and the same reason: the refusal is keyed on a VALUE somebody wrote.
    ///
    /// ⚠ …and it declares nothing either, which is the half the stage-3 migration warning rests on:
    /// a section header with no venue under it has stated no arming intent, so silencing the warning
    /// on it would leave a credentialled box on all-paper with nothing said.
    #[test]
    fn an_empty_venues_table_is_not_refused() {
        let patch: PolicyPatch = toml::from_str("[venues]\n").expect("parses");
        let mut p = Policy::default();
        p.apply(patch, file()).expect("`[venues]` with nothing in it claims nothing");
        assert_eq!(p.venues, Policy::default().venues);
        assert!(!p.venues.is_declared(), "a table naming no venue has declared no venue");
    }

    /// **The migration warning's self-silencing fact, driven through the REAL patch path.** A table
    /// that sets every venue to the value they already had is a DECISION, and after this the warning
    /// must never fire again on that box — while the ceiling it produced is the default's.
    ///
    /// Driven through `apply` rather than by calling `VenuePolicy::set` directly, because the claim
    /// is about what a FILE does: `apply` is the only production caller, and a test that pokes the
    /// setter would stay green if the `[venues]` arm stopped reaching it.
    #[test]
    fn an_all_paper_table_declares_while_no_table_does_not() {
        let mut untouched = Policy::default();
        assert!(!untouched.venues.is_declared(), "the compiled-in default is nobody's decision");
        // A policy file that sets OTHER ceilings and no `[venues]` table is still undeclared —
        // this is the the CI box shape (`settings/policy.toml` sets only the notional cap).
        untouched
            .apply(
                PolicyPatch { max_notional_per_order: Some(250.0), ..Default::default() },
                file(),
            )
            .unwrap();
        assert!(!untouched.venues.is_declared(), "another key is not a venue decision");

        let mut declared = Policy::default();
        let all_paper: String = std::iter::once("[venues]".to_string())
            .chain(VENUES.iter().map(|v| format!("{v} = \"paper\"")))
            .collect::<Vec<_>>()
            .join("\n");
        declared.apply(toml::from_str(&all_paper).expect("parses"), file()).unwrap();
        assert!(declared.venues.is_declared(), "an all-paper table IS a stated decision");
        assert!(
            declared.venues.iter().eq(untouched.venues.iter()),
            "…and it produced exactly the default CEILING, which is why the map alone cannot \
             answer the question this flag answers"
        );
    }

    /// A venue this BUILD cannot mount is still a legal ceiling — the file is portable across
    /// boxes whose cargo features differ, and a ceiling for an unmountable venue caps at paper
    /// anyway. See `crate::venue_mode`'s module doc for the full argument.
    #[test]
    fn a_feature_gated_venue_is_still_a_legal_ceiling() {
        for venue in ["ibkr", "polymarket", "fxcm"] {
            let text = format!("[venues]\n{venue} = \"demo\"\n");
            let patch: PolicyPatch = toml::from_str(&text).expect("parses");
            let mut p = Policy::default();
            p.apply(patch, file()).unwrap_or_else(|e| panic!("{venue} must be settable: {e}"));
            assert_eq!(p.venues.get(venue), VenueMode::Demo);
        }
    }

    #[test]
    fn a_zero_money_ceiling_is_rejected_rather_than_silently_halting_trading() {
        let err = Policy::default()
            .apply(PolicyPatch { max_notional_per_order: Some(0.0), ..Default::default() }, file())
            .unwrap_err();
        assert!(err.to_string().contains("max_notional_per_order"), "{err}");
        assert!(err.to_string().contains("denies every order"), "{err}");
    }
}
