//! `config` — the `vike-tradehub` daemon PROFILE (headless two-layer plan, Layer 2, PR-9).
//!
//! ONE reviewable TOML file replaces a pile of env vars (LEAN "environments", steal S8): it names
//! the mount (venue / `symbol` / interval), the `[strategy]` table naming WHICH strategy FILE to
//! run, and the daemon settings (snapshot-summary cadence, bounded-shutdown deadline).
//!
//! ## The `[strategy]` table — the same key a backtest profile uses
//! `[strategy] file = "<path>"` + `[strategy.params]`: the path of a strategy file under the
//! project's `strategies/` folder (`vike_paths::state_path::PROJECT_STRATEGIES_DIR`), the same key
//! `vike_backtest::run::profile::ProfileBacktest` takes, so the profile that was backtested is the
//! profile that trades. No strategy is compiled into this program: a `.rhai` file runs in this
//! process (`vike_rhai::RhaiStrategy` at `LiveBroker`, decision 0024's rails), a `.rs` file in a
//! sandboxed process of its own (the seam `config/sandboxed.rs` names). The file's own header
//! declares where it may run and what it reads (`vike_strategy::file`); the mechanics are
//! [`ProfileDaemon::resolve_mount`]'s.
//!
//! **A mount without a file is refused at load, naming the folder**: there is no default strategy.
//! The retired `name = "…"` and `rhai = "…"` keys are refused with the one-line fix, and so are the
//! retired top-level `qty`/`half_spread`/`resolution_ts_ms`.
//!
//! ## Scope — the mount config, PAPER or LIVE
//! This profile lowers into the strategy-free [`vike_tradehub_core::MountPlacement`] and the resolved
//! strategy that BOTH the paper mount ([`vike_tradehub_core::build_paper_strategy_core_with`], the
//! default) and the opt-in LIVE `assemble_runtime` mount
//! ([`vike_tradehub_core::build_live_multi_strategy_core_armed`], under the `flags.tradehub_live` row)
//! build from. The
//! daemon's LIVE path additionally calls [`ProfileDaemon::validate_for_live`] to reject a venue it
//! has not wired for live and a symbol its engine would silently drop. The auth'd network control
//! server is PR-10/11/12. Existing settings the mount already honors (`config.journal_dir`,
//! `config.pin_cores`, …) keep their meaning — this profile deliberately
//! does not re-parse them.

use std::time::Duration;

use serde::Deserialize;
use vike_config::ProfileRun;
use vike_model::ProfileRiskError;
use vike_model::RiskLimits;
use vike_model::params::Params;
use vike_tradehub_core::MountPlacement;

mod sandboxed;

pub use sandboxed::{
    FileBuilder, MountBuilds, OpenRefusal, SandboxBuild, SandboxOpen, SandboxOpened, use_builder,
};
pub(crate) use sandboxed::{building_words, not_at_runtime, open_built};

/// What [`ProfileDaemon::resolve_mount`] resolved from the mount's strategy FILE, and — the point —
/// WHERE it runs: in this process (a `.rhai` file) or in a sandboxed process of its own (a `.rs`
/// file). Both are the same thing to the mount builders (a `Strategy<LiveBroker> + Send`), so the
/// daemon boxes them and forgets the difference one line later ([`Self::into_strategy`]).
///
/// Each variant carries the audit pair beside the box — `path` as the profile named the file and
/// `sha256` of the bytes that were ACTUALLY read — so the "which code traded" claim the resolve logs
/// at INFO is also assertable by a test without a log subscriber, and `params`, the one-line echo
/// of what the mount RESOLVED ([`Self::effective_params`]).
pub enum MountResolvedStrategy {
    /// A `.rhai` file, compiled in this process at [`vike_core::LiveBroker`] by `vike-rhai`'s one
    /// engine builder (`docs/decisions/0024-rhai-strategies-live.md`).
    Script {
        /// The compiled script.
        strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
        /// The file as the profile named it (relative to the strategies folder).
        path: String,
        /// Lowercase-hex sha256 of the file's bytes ([`vike_hub_protocol::hash::sha256_hex`]).
        sha256: String,
        /// The effective-params line ([`Self::effective_params`]).
        params: String,
        /// The header's `fill = "close"` ([`Self::fills_at_close`]).
        fill_at_close: bool,
    },
    /// A `.rs` file, run in a sandboxed process of its own (`config/sandboxed.rs`'s seam): the
    /// strategy here is the proxy that speaks to that process.
    Sandboxed {
        /// The proxy of the sandboxed process.
        strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
        /// The file as the profile named it (relative to the strategies folder).
        path: String,
        /// Lowercase-hex sha256 of the file's bytes.
        sha256: String,
        /// The effective-params line: the rows the file's `echo` answered when it opened.
        params: String,
        /// The host's handle on the sandboxed mount ([`SandboxOpened::remote`]).
        remote: Option<vike_tradehub_core::remote_strategy::RemoteHandle>,
        /// The header's `fill = "close"` ([`Self::fills_at_close`]).
        fill_at_close: bool,
        /// The SDK the file was built against ([`SandboxOpened::sdk`]), `None` until a build answers.
        sdk: Option<u32>,
    },
}

impl MountResolvedStrategy {
    /// The sandboxed mount's handle (a `.rs` file opened in its own process), `None` otherwise.
    #[must_use]
    pub fn remote(&self) -> Option<vike_tradehub_core::remote_strategy::RemoteHandle> {
        match self {
            MountResolvedStrategy::Sandboxed { remote, .. } => remote.clone(),
            MountResolvedStrategy::Script { .. } => None,
        }
    }

    /// The file this mount read and the sha256 of its bytes, as the journal records them when the mount
    /// opens (`vike_journal::JournalRecord::StrategyFileMounted`).
    #[must_use]
    pub fn strategy_file(&self) -> vike_journal::JournalStrategyFile {
        // A `.rhai` file has no SDK; a `.rs` file's is the builder's answer (decision 0296, verdict 3), `None` while
        // its build is awaited (`crate::tradehub_cli::awaited_builds` records it when the build answers).
        let (path, sha256, sdk) = match self {
            MountResolvedStrategy::Script { path, sha256, .. } => (path, sha256, None),
            MountResolvedStrategy::Sandboxed { path, sha256, sdk, .. } => (path, sha256, *sdk),
        };
        vike_journal::JournalStrategyFile { path: path.clone(), sha256: sha256.clone(), sdk }
    }

    /// Whether the file's header declares `fill = "close"` (decision 0285's amendment of 2026-11-16): on a
    /// paper engine its bar orders fill at that bar's close (`vike_tradehub_core::MountPlacement::fill_at_close`).
    #[must_use]
    pub fn fills_at_close(&self) -> bool {
        match self {
            MountResolvedStrategy::Script { fill_at_close, .. }
            | MountResolvedStrategy::Sandboxed { fill_at_close, .. } => *fill_at_close,
        }
    }

    /// The strategy, boxed for the mount builders.
    #[must_use]
    pub fn into_strategy(self) -> Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> {
        match self {
            MountResolvedStrategy::Script { strategy, .. }
            | MountResolvedStrategy::Sandboxed { strategy, .. } => strategy,
        }
    }

    /// The EFFECTIVE parameters of the mounted strategy, as one log-safe line: what each knob
    /// RESOLVED TO, never what was typed.
    ///
    /// ⚠ Not decoration. A daemon that logged `strategy = <name>` and nothing about its numbers left
    /// an operator unable to tell which configuration traded, and an echo of the RAW table was worse
    /// than nothing (`size = "2"` printed `size="2"` while the strategy ran `size = 1`: the
    /// declared-but-unread failure `Policy::max_total_exposure` was deleted over). So a `.rs` file
    /// reports its own `echo` rows (`key=<what the reader landed on>`), and a `.rhai` file reports its path plus the numeric overrides the compile baked
    /// in — every key printed was asked for by the script and DID land, because the resolve refuses
    /// an override the script never passes to `param(…)`.
    ///
    /// ⚠ It says what each knob RESOLVED TO and does not claim any knob is in force: whether a
    /// resolved value is CONSUMED can depend on the other knobs and on the market (`anchor_price` on
    /// a grid whose `anchor` is unset resolves to the typed number and is read by nobody).
    #[must_use]
    pub fn effective_params(&self) -> &str {
        match self {
            MountResolvedStrategy::Script { params, .. }
            | MountResolvedStrategy::Sandboxed { params, .. } => params,
        }
    }
}

/// Strategy selection: the strategy FILE, plus a free-form [`Params`] table the file's own reader
/// interprets. The same `file` key `vike_backtest::run::profile::ProfileStrategy` takes (one selection rule
/// for both profiles), so a profile that was backtested can be traded by copying the table across.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileStrategy {
    /// The strategy file: its path RELATIVE to the project's strategies folder
    /// (`<project>/strategies/`, `vike_paths::state_path::PROJECT_STRATEGIES_DIR`), a `.rs` or a
    /// `.rhai` file, for example `"trend/momentum.rs"`. Absolute and `..` paths are refused at load
    /// (`vike_strategy::file::scan::resolve`'s rule).
    ///
    /// The file is read at RESOLVE time, not at load: [`ProfileDaemon::validate`] is pure over the
    /// TOML text, so a missing file, a broken header, a key the header does not declare and a script
    /// that does not compile fail at [`ProfileDaemon::resolve_mount`] — still before any core spawns.
    /// At resolve the file's path and sha256 are logged at INFO (the audit trail for WHICH code
    /// traded: [`MountResolvedStrategy`]).
    #[serde(default)]
    pub file: Option<String>,
    /// ⚠ RETIRED, refused at load with the fix ([`ProfileDaemon::validate`]): a strategy was once
    /// mounted by a registry NAME. Nothing is compiled into this program any more, so a name selects
    /// nothing. Kept as a field only so the refusal can say what to write instead.
    #[serde(default)]
    pub name: Option<String>,
    /// ⚠ RETIRED, refused at load with the fix: a Rhai script path anywhere on the box. A script is a
    /// strategy file now, under the strategies folder, named by [`Self::file`].
    #[serde(default)]
    pub rhai: Option<String>,
    /// The strategy's own knobs. Free-form on the wire (each file reads its own), and **not**
    /// free-form in effect: the file's header declares every key its reader takes, and the resolve
    /// refuses a key the header does not declare, a value of a type or range the header refuses, and
    /// a key that names a market this mount does not trade, because a params reader ignores what it
    /// does not recognise and the knob then mounts at its compiled default — on this daemon, a live
    /// order at a size nobody typed.
    ///
    /// Under a `.rhai` file every key is also a `param(name, default)` OVERRIDE baked into the script
    /// scope at compile, so [`ProfileDaemon::validate`] refuses a non-numeric value (`param` takes an
    /// `f64`, so a quoted number configures nothing), and the resolve refuses a key the script's own
    /// top level never passes to `param(…)` (`vike_rhai::discover_params`).
    ///
    /// A TABLE by type: `params = 5` is a load (serde) refusal, not a value nobody reads. Absent, it
    /// is the empty table.
    #[serde(default)]
    pub params: Params,
}

/// A Rhai profile's `[strategy.params]` as the `param(name, default)` override map
/// [`vike_rhai::RhaiStrategy::compile_with_params`] bakes into the script scope — every NUMERIC
/// key (float or integer, [`Params::numeric`]'s narrowing), in key order. The same collection
/// the backtest's script arm builds, minus its reserved `src` key, which does not exist
/// in this spelling (the script arrives as a FILE). Non-numeric values never reach here:
/// `ProfileDaemon::validate_script` refused them at load, so the dropped-key half of
/// [`Params::numeric`] is empty on every profile that loaded and is not reported.
fn script_overrides(params: &Params) -> indexmap::IndexMap<String, f64> {
    params.numeric().0
}

/// The whole daemon profile: the mount fields (flattened at the top level), the `[strategy]`
/// table naming the mount's strategy file, and a `[daemon]` table. Every mount field except the SYMBOL is optional and,
/// when omitted, takes its venue's default ([`Self::to_mount_spec`]) — so a minimal profile is just
/// a symbol. `deny_unknown_fields` turns a typo'd key into a parse error instead of
/// a silent no-op.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDaemon {
    /// venue tag (default `"polymarket"`) — also the paper client's + engine's venue. An `Option`
    /// (resolved through [`Self::venue`], the accessor) rather than a serde-defaulted `String`, so
    /// [`Self::validate`] can tell an explicit `venue = "…"` from an omitted one — the difference
    /// between refusing and silently ignoring a top-level venue beside a `[[mounts]]` array.
    #[serde(default)]
    pub venue: Option<String>,
    /// **WHAT PRODUCT this mount trades** — see [`MountConfig::asset_class`], which this is the
    /// single-mount spelling of. `Option` here, `NOT NULL` in the row; the asymmetry is the
    /// migration, and it is argued at that field.
    #[serde(default)]
    pub asset_class: Option<String>,
    /// The engine/mount SYMBOL the strategy trades, on every venue: a hyperliquid mount names `"BTC"`, a
    /// polymarket one its outcome token id. Required ([`Self::validate`]). PM-3 retired its `token_id` twin: a
    /// document still saying `token_id` is refused at load (`deny_unknown_fields`), never read as nothing.
    #[serde(default)]
    pub symbol: Option<String>,
    /// WHICH strategy file to mount ([`ProfileStrategy::file`]). An `Option` only so its absence is
    /// refused at load in this crate's own words, naming the folder ([`Self::validate`]): there is no
    /// default strategy.
    #[serde(default)]
    pub strategy: Option<ProfileStrategy>,
    /// ⚠ RETIRED, refused at load ([`Self::validate`]): the maker's `[strategy.params] resolution_ts`
    /// (step 10, Q4). Kept as a field only so the refusal can name where it went; a stored mount row's
    /// column lowers into it and is refused the same way.
    #[serde(default)]
    pub resolution_ts_ms: Option<i64>,
    /// mounted bar-series interval (default `"1m"`).
    #[serde(default)]
    pub interval: Option<String>,
    /// synth-bar window in ms (default `60_000`); must match `interval`.
    #[serde(default)]
    pub interval_ms: Option<i64>,
    /// ⚠ RETIRED, refused at load: the maker's `[strategy.params] qty` ([`Self::resolution_ts_ms`]'s doc).
    #[serde(default)]
    pub qty: Option<f64>,
    /// ⚠ RETIRED, refused at load: the maker's `[strategy.params] half_spread`
    /// ([`Self::resolution_ts_ms`]'s doc).
    #[serde(default)]
    pub half_spread: Option<f64>,
    /// The mount's venue PRICE GRID ([`MountPlacement::tick_size`]): the step its feed sizes the L2 book on
    /// and the tick size a sandboxed strategy file is told when it opens (default `0.01` on
    /// polymarket, `1.0` elsewhere).
    #[serde(default)]
    pub tick_size: Option<f64>,
    /// paper account seed equity of this mount's book on the paper-only node (default: the ONE paper starting
    /// balance, `vike_tradehub_core::PAPER_SEED_CASH`, on every venue). An account-row paper engine of the live
    /// node starts on that one balance whatever a mount states here (`vike_tradehub_core::engine_seed`).
    #[serde(default)]
    pub seed_cash: Option<f64>,
    /// The DATA-PLANE-ONLY declaration (the data-only credential seam): `true` tells `live_runtime`
    /// to WITHHOLD this venue's credentials from the exec mount, so no venue client is armed (a
    /// `paper` account runs the paper simulator, a `demo` or `live` one does not run) while the
    /// venue's CREDENTIALED market feed still
    /// authenticates (its plan resolves the credentials BEFORE the withhold). Valid only for the
    /// credentialed-data feed venues (`crate::feeds::data_only_feed`) — a keyless-data venue is refused at load, because its
    /// data-only mount is "withhold the credentials from the store" and a declaration there could
    /// only mislead the arming disclosure. Absent/`false` (the default) is byte-identical to a
    /// profile without the key: credentials present ⇒ exec arms, the workspace live gate.
    /// Refused when the live gate is OFF (`tradehub_cli.rs`'s paper arm) — the paper daemon mounts no
    /// venue feed, so the key would configure nothing while reading as real.
    #[serde(default)]
    pub data_only: Option<bool>,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on** — an `account` row's `label` (the
    /// `__<LABEL>` its credential keys carry), e.g. `account = "ALT"`. Absent (every profile that has ever shipped) is the venue's
    /// DEFAULT account: the same engine, the same route key, the same `LIVE-<route_key>.lock`
    /// filename, byte for byte — until the venue runs a SECOND account: an account-less mount on a
    /// venue with two engines is ambiguous, so that ONE mount is not mounted (an `error!` and a
    /// `MOUNT REFUSED` note listing the accounts to choose from; the rest of the node keeps
    /// running). `account = "DEFAULT"` names the unlabelled account explicitly and is never
    /// ambiguous; `account = "<LABEL>"` names a labelled one.
    ///
    /// Set it and this mount's orders AND its `OrderBroker` reads go to that account's own engine, and
    /// [`Self::mount_symbol`] becomes the symbol that account is armed on
    /// (`vike_tradehub_core::account_symbols_for`) — which is how a second account reaches a second
    /// instrument at all, since the venue's DEFAULT account's row states one symbol (`account.symbol`) for the DEFAULT
    /// account.
    ///
    /// ⚠ **An account this box will not ARM is a hard startup failure naming venue, account and
    /// mount** (`crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`), not a fallback to the default
    /// account. It is the one refusal in this daemon that does not degrade, because the degraded
    /// state is "trading, on a book the author did not choose".
    #[serde(default, with = "vike_model::accounts::account_keys::wire_account_option")]
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// **This mount's own LEVERAGE CAP** (the `mount.max_leverage` column; decision 0249's amendment of
    /// 2026-11-23): its gross notional after an opening order, over its account engine's sizing equity, may
    /// not exceed it. It may only NARROW the `policy.max_mount_leverage` row
    /// (`crate::venue_arming::mount_budget`'s `mount_budgets_from_columns` takes the lower of the two).
    /// Absent (every profile that has ever shipped) is the policy row alone, or no cap. Finite and `>= 1`,
    /// refused at load otherwise ([`Self::validate`]).
    #[serde(default)]
    pub max_leverage: Option<f64>,
    /// daemon runtime settings (summary cadence + shutdown deadline).
    #[serde(default)]
    pub daemon: ProfileDaemonConfig,
    /// **The `[inputs]` REMAP of the series a mount's strategy reads by name** (decision 0271): the
    /// backtest profile's table, in the same shape and through the same parser
    /// (`vike_data::store::series::unparsed_input_rows`, the one load-time check both profiles run, in
    /// the backtest's words: `ProfileDaemon::validate`): `"<the name the strategy's code
    /// writes>" = "<the series to read instead>"`, both in the input grammar. DAEMON-WIDE, like
    /// `[daemon]`: every mount reads through it (each `[[mounts]]` row, whose lowering carries it, and a
    /// runtime mount, through the core's one input opener), a one-part name (`"1d"`) as that mount's own
    /// symbol. Absent (the default) reads every input as its code names it, byte-identical to a profile
    /// before the table. A row no boot mount's strategy declares is refused at start
    /// (`crates/vike-tradehub/src/tradehub_cli/profile.rs`'s `resolve_mounts`), as a backtest refuses one
    /// at its run.
    #[serde(default)]
    pub inputs: std::collections::BTreeMap<String, String>,
    /// The MULTI-mount spelling (split-plane I10, Pattern A: strategies sharing an account share a
    /// PROCESS): `[[mounts]]` — an ARRAY of mount blocks, each carrying the SAME fields the
    /// single-mount spelling puts at the top level (venue / `symbol` / interval /
    /// the maker knobs / an optional `[mounts.strategy]` table). Empty (the default) ⇒ the
    /// historical single-mount profile, byte-identically.
    ///
    /// The two spellings are MUTUALLY EXCLUSIVE — a profile that sets any top-level mount field
    /// beside a `[[mounts]]` array is refused at load ([`Self::validate`]), because a top-level
    /// knob beside `[[mounts]]` would configure NOTHING while reading as real (the
    /// declared-but-unread failure this repo's settings rule exists for). Each row validates
    /// through the SAME per-mount refusals a single-mount profile passes ([`Self::mount_rows`]
    /// lowers each row to a single-mount profile and validates it), each failure naming its row.
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

/// One `[[mounts]]` row — the single-mount profile's mount fields, verbatim, MINUS the `[daemon]`
/// table (daemon-wide, not per-mount) and minus a nested `mounts` (rows do not recurse).
///
/// `venue` is an `Option` here where [`ProfileDaemon`] defaults it silently, so
/// [`ProfileDaemon::mount_rows`] can apply the same `"polymarket"` default per row; every other
/// field is the exact `Option` the top level has. Field docs live on [`ProfileDaemon`] — one
/// authority, and these ARE those fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountConfig {
    #[serde(default)]
    pub venue: Option<String>,
    /// **WHAT PRODUCT this mount trades** — the stored word of a `vike_model::AssetClass`
    /// (`"CryptoSpot"`, `"CryptoPerp"`, `"Option"`, …). Phase 5 of
    /// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
    /// type if it is spot or perp or option or anything else bcz it will help us in future"*.
    ///
    /// ⚠ **`Option` HERE and NOT NULL in the ROW, and the asymmetry is the migration.** A
    /// `tradehub.toml` written before this key existed must keep parsing (its in-memory TOML
    /// loader still reads that shape) — the live daemon's own profile was a file until
    /// decision 0086 moved it into rows, and making it a required TOML key would have stopped that
    /// box starting. But
    /// `vike_secrets::profile_store::MountRow` requires the class, so
    /// [`crate::profile_rows::daemon_profile_to_rows`] REFUSES to lower a mount that does not name
    /// one. The cost of making it mandatory is therefore exactly what 0061 priced: **one value in
    /// one file**, paid by whoever migrates that profile into rows, and nothing before then.
    ///
    /// A `String` rather than a typed enum so the TOML spelling and the COLUMN carry one word;
    /// [`crate::profile_rows::parse_mount_asset_class`] is where it becomes an `AssetClass`, and an
    /// unknown word is refused there by name.
    #[serde(default)]
    pub asset_class: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub strategy: Option<ProfileStrategy>,
    #[serde(default)]
    pub resolution_ts_ms: Option<i64>,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub interval_ms: Option<i64>,
    #[serde(default)]
    pub qty: Option<f64>,
    #[serde(default)]
    pub half_spread: Option<f64>,
    #[serde(default)]
    pub tick_size: Option<f64>,
    #[serde(default)]
    pub seed_cash: Option<f64>,
    #[serde(default)]
    pub data_only: Option<bool>,
    /// WHICH ACCOUNT of this row's venue it trades on — see [`ProfileDaemon::account`], which this
    /// lowers into verbatim.
    #[serde(default, with = "vike_model::accounts::account_keys::wire_account_option")]
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// This row's own LEVERAGE CAP — see [`ProfileDaemon::max_leverage`], which this lowers into verbatim.
    #[serde(default)]
    pub max_leverage: Option<f64>,
    /// **THE EXPLICIT PRIMARY** — `primary = true` on at most ONE row.
    ///
    /// This is the field 0057's *tradehub.toml* verdict says must exist before mounts become rows:
    /// *"the first mount row silently means three different things and means a fourth thing not at
    /// all"*, and **SQL rows have no inherent order**, so a table cannot reproduce "the first one"
    /// without either an ordinal or a declaration.
    ///
    /// It is deliberately NOT lowered onto [`ProfileDaemon`] by [`ProfileDaemon::mount_rows`].
    /// Primacy is a fact about a row's place in the SET, and a lowered row is a single-mount profile
    /// that no longer has a set to be first in; carrying it down would have made
    /// `ProfileDaemon::validate` answer a question that only `validate_multi` can.
    /// [`ProfileDaemon::primary_mount`] reads it off `mounts` directly, which is the one resolution.
    ///
    /// ⚠ **Absent on every row is the historical answer and stays byte-identical**: the FIRST row
    /// is the primary, exactly as `resolved[0]` made it before this key existed
    /// ([`MountPrimaryIndex::ImplicitFirst`]). Nothing that has ever shipped sets this, so nothing moves.
    #[serde(default)]
    pub primary: Option<bool>,
}

/// **WHICH mount is the daemon's singular identity, and how that was decided.**
///
/// # What "primary" actually is today, measured rather than assumed
///
/// `crates/vike-tradehub/src/tradehub_cli.rs` took `resolved[0]` and its own comment named the three
/// jobs that index does: *"the daemon's historical singular identity (summary token, mode line, seed
/// policy)"*. 0057 adds the half that comment does not, and it is the reason this type exists:
///
/// * on a PAPER core `crates/vike-tradehub-core/src/run.rs` derives ENGINE ORDER from first-appearance venue
///   order, and the first mount naming a symbol supplies that book's fee and slippage scalars;
/// * on a LIVE core row order decides **nothing at all** — `crates/vike-tradehub-core/src/node/build.rs`'s
///   `assemble_runtime` builds its engine list from the wired markets
///   (`vike_tradehub_core::wired_markets`, held in their `engine_rank` order);
/// * so the same index means three things in one binary and nothing in the other, and **no document
///   declared any of them**. That is what made PR #1866's defect reachable from the other side: the
///   order path's silent fall-through to engine zero was a fall-through to THIS, undeclared.
///
/// # Why an enum and not a `usize`
///
/// The migration to rows must not change the answer. [`Self::ImplicitFirst`] is today's rule kept
/// verbatim for every profile that declares nothing — which is every profile that has ever shipped —
/// and [`Self::Declared`] is the new capability. A bare index could not tell a caller which of the
/// two it was holding, and a startup line that cannot say *implicit* is the positive confirmation of
/// something false this workspace deletes settings keys over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPrimaryIndex {
    /// A `[[mounts]]` row set `primary = true`. The index is that row's, which need NOT be `0` —
    /// that is the entire point of the key.
    Declared(usize),
    /// No row declared one, so index `0` is the primary: the first `[[mounts]]` row, or — for the
    /// historical single-mount spelling — the profile itself. **Byte-identical to `resolved[0]`.**
    ImplicitFirst(usize),
}

impl MountPrimaryIndex {
    /// The index into [`ProfileDaemon::mount_rows`]'s output.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            MountPrimaryIndex::Declared(i) | MountPrimaryIndex::ImplicitFirst(i) => i,
        }
    }

    /// The word a startup line prints, so an operator can tell a declaration from an accident.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            MountPrimaryIndex::Declared(_) => "declared",
            MountPrimaryIndex::ImplicitFirst(_) => {
                "implicit (the first row; no `primary = true` set)"
            }
        }
    }
}

/// **A node that cannot honour a name must not choose one** — PR #1866's refusal sentence
/// (`crate::refusals::DoorRefusal::NoEngine`), stated ONCE and shared by the order path and the mount path.
///
/// #1866 fixed exactly this defect on the ORDER path: `CoreThread::route_of` resolved an unknown
/// venue to `.unwrap_or(0)`, so a command naming a venue the process runs no engine for was signed
/// and sent by the PRIMARY venue's execution client. `crate::server::refusal::venue_refusal` calls this, and
/// so does the profile path, because a mount naming an engine-less venue is the same mistake one
/// layer earlier — and 0057's Phase 0 names it as such: *"This is the mount-side twin of the defect
/// the order path had fixed."*
///
/// `engines` EMPTY means the roster is not known here and refuses nothing — #1866's own reading (an
/// empty engine roster means the core has not published yet, not that it runs none).
#[must_use]
pub fn no_engine_refusal(
    venue: &str,
    engines: &[String],
    subject: &str,
) -> Option<crate::refusals::DoorRefusal> {
    if engines.is_empty() || engines.iter().any(|e| e == venue) {
        return None;
    }
    let mut known: Vec<&str> = engines.iter().map(String::as_str).collect();
    known.sort_unstable();
    known.dedup();
    Some(crate::refusals::DoorRefusal::NoEngine {
        venue: venue.to_string(),
        known: known.join(", "),
        subject: subject.to_string(),
    })
}

/// Daemon runtime settings — the two knobs that are the daemon's own, not the mount's.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDaemonConfig {
    /// snapshot-summary cadence in ms (default `5_000`): how often a one-line JSON summary of the
    /// live snapshot is printed to STDOUT.
    #[serde(default = "default_summary_ms")]
    pub summary_ms: u64,
    /// bounded-shutdown deadline in ms (default `5_000`): the whole `run_with_deadline` teardown is
    /// hard-capped at this, so a wedged `shutdown_and_join` can never hang exit.
    #[serde(default = "default_shutdown_deadline_ms")]
    pub shutdown_deadline_ms: u64,
}

/// The historical default venue an omitted `venue` key resolves to ([`ProfileDaemon::venue`]).
const DEFAULT_VENUE: &str = "polymarket";
fn default_summary_ms() -> u64 {
    5_000
}
fn default_shutdown_deadline_ms() -> u64 {
    5_000
}

impl Default for ProfileDaemonConfig {
    fn default() -> Self {
        ProfileDaemonConfig {
            summary_ms: default_summary_ms(),
            shutdown_deadline_ms: default_shutdown_deadline_ms(),
        }
    }
}

impl ProfileDaemon {
    /// Parse a profile from TOML text, then validate it.
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        let profile: ProfileDaemon = toml::from_str(s).map_err(|e| {
            crate::refusals::MountProfileRefusal::ParseFailed { why: e.to_string() }.to_string()
        })?;
        profile.validate()?;
        Ok(profile)
    }

    /// The mount SYMBOL. Panics never: [`Self::validate`] has already rejected a profile that set none,
    /// and every construction path goes through it.
    pub fn mount_symbol(&self) -> &str {
        self.symbol.as_deref().unwrap_or_default()
    }

    /// The mount VENUE, with the historical `"polymarket"` default applied — the one resolution
    /// site for the field's `Option` (see the field doc for why it is one).
    pub fn venue(&self) -> &str {
        self.venue.as_deref().unwrap_or(DEFAULT_VENUE)
    }

    /// The EFFECTIVE data-only verdict for this mount row (see the [`Self::data_only`] field doc):
    /// absent and explicit `false` are the same answer — exec follows the ordinary credential
    /// gate — so a profile that spells the default is byte-identical to one that omits it.
    pub fn data_only_effective(&self) -> bool {
        self.data_only.unwrap_or(false)
    }

    /// This profile as N single-mount rows: itself for the historical single-mount spelling, else
    /// one lowered [`ProfileDaemon`] per `[[mounts]]` row (same `[daemon]` settings, `mounts`
    /// emptied — rows do not recurse). Every consumer of a mount — validation, the mount-config /
    /// mount-spec lowerings, strategy resolve, `effective_params`, `validate_for_live` — runs on a
    /// ROW, so the multi-mount path reuses the single-mount machinery verbatim and the two
    /// spellings cannot diverge on what one mount means.
    pub fn mount_rows(&self) -> Vec<ProfileDaemon> {
        if self.mounts.is_empty() {
            return vec![self.clone()];
        }
        self.mounts
            .iter()
            .map(|m| ProfileDaemon {
                venue: m.venue.clone(),
                asset_class: m.asset_class.clone(),
                symbol: m.symbol.clone(),
                strategy: m.strategy.clone(),
                resolution_ts_ms: m.resolution_ts_ms,
                interval: m.interval.clone(),
                interval_ms: m.interval_ms,
                qty: m.qty,
                half_spread: m.half_spread,
                tick_size: m.tick_size,
                seed_cash: m.seed_cash,
                data_only: m.data_only,
                account: m.account.clone(),
                max_leverage: m.max_leverage,
                daemon: self.daemon.clone(),
                // Daemon-wide, like `[daemon]`: every row reads its inputs through it.
                inputs: self.inputs.clone(),
                mounts: Vec::new(),
            })
            .collect()
    }

    /// **THE ONE PRIMARY RESOLUTION.** Every consumer asks this instead of indexing `[0]`, so the
    /// daemon's singular identity is answered in one place and can be REPORTED rather than assumed.
    ///
    /// * a `[[mounts]]` row with `primary = true` ⇒ [`MountPrimaryIndex::Declared`] at that row's index;
    /// * otherwise ⇒ [`MountPrimaryIndex::ImplicitFirst(0)`](MountPrimaryIndex::ImplicitFirst), which is
    ///   `resolved[0]` — the historical answer, byte for byte, for the single-mount spelling and for
    ///   every `[[mounts]]` profile that declares nothing.
    ///
    /// Two declared primaries are impossible here: [`Self::validate_multi`] refuses the profile at
    /// LOAD, naming both rows, so this function has no tie to break and needs no rule for one.
    #[must_use]
    pub fn primary_mount(&self) -> MountPrimaryIndex {
        match self.mounts.iter().position(|m| m.primary == Some(true)) {
            Some(i) => MountPrimaryIndex::Declared(i),
            None => MountPrimaryIndex::ImplicitFirst(0),
        }
    }

    /// Refuse this profile's PRIMARY mount when the node runs no engine for its venue, **in the
    /// vocabulary PR #1866 established** ([`no_engine_refusal`]).
    ///
    /// The primary is the mount whose venue becomes the daemon's identity and — before #1866 — the
    /// engine an unroutable order silently reached. A profile that declares a primary on a venue
    /// this node cannot run is therefore the same fault the order path now refuses, arriving one
    /// layer earlier, and it is refused with the same words rather than a second vocabulary.
    ///
    /// `engines` empty ⇒ `Ok(())`. See [`no_engine_refusal`] for why.
    ///
    /// # Errors
    ///
    /// The refusal sentence, prefixed with which mount it is about.
    pub fn refuse_unrunnable_primary(&self, engines: &[String]) -> Result<(), String> {
        let primary = self.primary_mount();
        let rows = self.mount_rows();
        let Some(row) = rows.get(primary.index()) else { return Ok(()) };
        match no_engine_refusal(
            row.venue(),
            engines,
            &format!("The {} primary mount (mounts[{}])", primary.word(), primary.index()),
        ) {
            Some(refusal) => Err(refusal.to_string()),
            None => Ok(()),
        }
    }

    /// The DERIVED per-mount controller id a `[[mounts]]` row mounts under:
    /// `{venue}__{symbol}__{interval}__{strategy-identity}` — the runtime's legacy
    /// `{venue}__{symbol}__{interval}` triple (`vike_core::strategy_state`'s `mount_id_with`
    /// fallback) extended by WHAT is mounted, so two different strategies on one series get two
    /// identities (their own durable-state sidecar, their own journal attribution key) instead of
    /// the `assemble_core` duplicate-id panic.
    ///
    /// The strategy identity is the strategy FILE's stem, `rhai-`-prefixed for a `.rhai` file
    /// (`mount_identity`, sanitized to `[A-Za-z0-9_-]` — the id is a state-sidecar FILENAME
    /// component), so two different strategies on one series stay distinct. Two rows deriving the
    /// SAME id are refused at load ([`Self::validate`]), naming both rows.
    ///
    /// Single-mount profiles deliberately do NOT use this: their `controller_id` stays `None`, so
    /// the runtime keeps the legacy triple derivation and an existing deployment's state sidecar /
    /// journal attribution keys are byte-identical.
    ///
    /// # ⚠ THE ACCOUNT IS PART OF THE IDENTITY, and leaving it out REFUSED THE HEADLINE SPREAD
    ///
    /// A labelled row appends `__{LABEL}`. Without it the id above forgets [`Self::account`]
    /// entirely, and two rows differing ONLY by account derive the SAME id — so the duplicate-id
    /// refusal in [`Self::validate`] rejected, at load, exactly the configuration the `account`
    /// field exists to enable: long BTC on the default account, short BTC on a labelled one, one
    /// venue, one symbol, one interval, one strategy. Its message even told the operator to
    /// "make one row distinct — a different interval, symbol or strategy", i.e. to stop running a
    /// spread. The refusal was not wrong; the IDENTITY was, and this is the half that was missing.
    ///
    /// It is a correctness fix as well as an unblocking one. Two accounts are two BOOKS — separate
    /// positions, separate fills, separate reconcile — so two such rows must never share one
    /// durable-state sidecar or one journal attribution key. Had the duplicate check been relaxed
    /// instead, that sharing is precisely what would have happened.
    ///
    /// **The DEFAULT account renders the id unchanged, byte for byte**
    /// ([`vike_model::accounts::account_keys::AccountLabel::text`] is
    /// `None` for it), so no deployment's sidecar or attribution key moves. Nothing has ever
    /// shipped a labelled row — the field is new — so the appended half moves nothing either.
    ///
    /// ⚠ A residual, declared rather than fixed: a SINGLE-mount profile that names an account keeps
    /// `controller_id = None` and therefore the legacy account-blind triple, so switching one from
    /// the default account to a labelled one INHERITS the default account's sidecar. Giving it an
    /// account-aware id would move every shipped single-mount deployment's state, which is the
    /// byte-identity this whole derivation is built around; `[[mounts]]` is the multi-account
    /// spelling, and it is the one that carries the account into the id.
    pub fn derived_controller_id(&self) -> String {
        let spec = self.to_mount_spec();
        let base = format!(
            "{}__{}__{}__{}",
            spec.venue,
            spec.symbol,
            spec.interval,
            self.mount_identity()
        );
        match spec.account.as_ref().and_then(vike_model::accounts::account_keys::AccountLabel::text)
        {
            None => base,
            Some(label) => format!("{base}__{label}"),
        }
    }

    /// The strategy half of [`Self::derived_controller_id`] — the file's stem
    /// ([`Self::strategy_name`]) for a `.rs` file and `rhai-<stem>` for a `.rhai` file, sanitized to
    /// `[A-Za-z0-9_-]` (the id is a state-sidecar FILENAME component).
    ///
    /// ⚠ These are the spellings a registry row (`{name}`) and a script row (`rhai-{stem}`) derived
    /// before strategies were files, ON PURPOSE: a row re-stored to name the file of the strategy it
    /// ran (`spread_maker` → `market_making/spread_maker.rs`) keeps its mount id, so its durable-state
    /// sidecar and its journal attribution key do not move. The cost, declared: two files sharing a
    /// stem in two folders, mounted on one series and one account, derive ONE id and are refused at
    /// load as a duplicate pair naming both rows (the message says to make one row distinct).
    fn mount_identity(&self) -> String {
        let sanitized: String = self
            .strategy_name()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
            .collect();
        match self.strategy_language() {
            Some(vike_strategy::file::scan::StrategyFileLanguage::Rhai) => {
                format!("rhai-{sanitized}")
            }
            _ => sanitized,
        }
    }

    /// The name the mount's strategy is known by: its FILE's stem (`trend/momentum.rs` → `momentum`),
    /// which is the registry name every shipped file had before it was a file. What the startup line,
    /// the summary token and the wire's mount rows print, and the name the mount's strategy code is
    /// kept by (`crate::mount_factory::mount_order_client_id_code`), so a re-stored row keeps its code too. Empty
    /// for a profile that names no file, which [`Self::validate`] refuses at load.
    pub fn strategy_name(&self) -> &str {
        self.strategy_file()
            .and_then(|file| std::path::Path::new(file).file_stem())
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
    }

    /// The strategy file the profile names, as it names it.
    pub fn strategy_file(&self) -> Option<&str> {
        self.strategy.as_ref().and_then(|s| s.file.as_deref())
    }

    /// The language of [`Self::strategy_file`], by its extension.
    pub fn strategy_language(&self) -> Option<vike_strategy::file::scan::StrategyFileLanguage> {
        vike_strategy::file::scan::language_of(std::path::Path::new(self.strategy_file()?))
    }

    /// Snapshot-summary cadence as a [`Duration`].
    pub fn summary_interval(&self) -> Duration {
        Duration::from_millis(self.daemon.summary_ms)
    }

    /// Bounded-shutdown deadline as a [`Duration`].
    pub fn shutdown_deadline(&self) -> Duration {
        Duration::from_millis(self.daemon.shutdown_deadline_ms)
    }
}

// `ProfileDaemon`'s one `impl` is split by concern into the child modules below; this file keeps
// the types, the constants, the load/accessor/identity methods and the free helpers they share.
mod mount;
pub(crate) use mount::echo_params_line;
mod validate;
pub use validate::LiveBox;

/// Resolve the OPERATOR risk budget to arm on the PAPER mount's `RiskGate` from an optional loaded
/// [`vike_config::ProfileRun`] (ProfileRun wiring, Settings STEP 2 PR 1, Task 2). This is the ONE
/// function `tradehub_cli.rs` calls before mounting — see its doc for why only `[risk]` is consumed from
/// the resolved profile (the daemon's own [`ProfileDaemon`] already owns the venue/symbol/A-S
/// mount shape; a `ProfileRun` has no venue/execution-target sections any more — `[event_source]`
/// and `[broker]` were DELETED for being read by nothing — so ONLY
/// the risk budget is layered on top).
///
/// - `profile: None` (no active `run` row) ⇒ `Ok(RiskLimits::new())` —
///   BYTE-IDENTICAL to the pre-Task-2 daemon's hardcoded value. This is the merge-safety property:
///   an operator who never opts into a profile sees no behavior change whatsoever.
/// - `profile: Some(p)` ⇒ `p.apply_risk(RiskLimits::new(), AccountTier::Paper)`, through
///   [`vike_config::ProfileRun::apply_risk`] at the PAPER tier: the grid source is the mounted
///   account's tier (decision 0156 item 7; `vike_model::GridFetch::for_tier`), and this mount is
///   the paper simulator, which never fetches a real venue instrument grid — so it is
///   [`vike_model::GridFetch::NoGridFetched`], and the profile's `[risk]` table may also supply
///   the instrument-grid fields itself (`tick_size`/`lot_size`/`min_qty`/`min_notional`) alongside
///   the operator-budget ones, since nothing else on this mount ever will. The `Result` return is
///   kept for honesty (and so a caller need not special-case an infallible-looking signature)
///   rather than unwrapped here.
pub fn resolve_paper_risk_limits(
    profile: Option<&ProfileRun>,
) -> Result<RiskLimits, ProfileRiskError> {
    match profile {
        None => Ok(RiskLimits::new()),
        Some(p) => p.apply_risk(RiskLimits::new(), vike_model::AccountTier::Paper),
    }
}

#[cfg(test)]
mod tests;
