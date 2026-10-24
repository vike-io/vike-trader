//! [`Preferences`] — TASTE and TUNING. Full override chain, but nothing here is a ceiling.
//!
//! The line against [`crate::Config`] is "would a second machine need a different value?" — a
//! log level or a chart style would not, they express what the operator likes. The line against
//! [`crate::Policy`] is sharper: **policy sets the BOUND, a preference sets the VALUE inside it.**
//!
//! ## ⚠ `rate_utilization` is a TOMBSTONE
//!
//! **Nothing read it.** Every pacer takes its target fraction from
//! `vike_binance::family::klines::KlineSpec::utilization`, and every construction site of that
//! field — binance's two consts, aster's `aster_kline_spec` — fills it with the compiled-in
//! [`vike_model::rate_limits::DEFAULT_UTILIZATION`]. Both halves
//! ([`PreferencesPatch::rate_utilization`] and [`crate::PolicyPatch::rate`]) REFUSE the key by
//! name. The concept did not move out of the product, only out of here: [`vike_model::rate_limits`]
//! owns the number, its bounds, and — the part a single float in this struct could never express —
//! per-VENUE overrides ([`vike_model::RateLimitConfig`]). A pacing knob that cannot say "40 % on
//! binance, 20 % on aster" is the wrong shape, and this one additionally lived in a crate no pacer
//! can reach: the consumer is `vike-backfill`, which reads no setting but its log level and binds
//! every venue's `fetch_klines_range` through one uniform 4-arg signature that carries no
//! utilization. It comes
//! back the day that path can carry it — with a consumer, this time.
//!
//! (The field also deliberately never got a `VIKE_RATE_UTILIZATION` env name, for a reason that
//! still holds for any future one: a program whose end state is "no crate calls `env::var` except
//! `vike-config`" should not open by MINTING a new environment variable.)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{ConfigError, stale_row_removal};
use crate::layers::{CliOverride, CliOverrides};

/// Default console log filter — `vike_log::LogConfig`'s own default.
pub const DEFAULT_LOG_LEVEL: &str = "info";
/// Default JSON-file log level. ⚠ `trace` is why an unattended backfill once wrote 341 GB; it is
/// kept here so this crate changes nothing, and the field exists so a deployment can turn it down
/// in a row instead of an env var.
pub const DEFAULT_LOG_FILE_LEVEL: &str = "trace";

/// `preferences.theme`'s words — the four themes (design system spec §3.1, §5), spelled as
/// `vike_ui_theme::theme::ThemeId::key` spells them. This crate (layer 20) cannot name that one
/// (layer 70), so the words are data here, and
/// `crates/vike-app-core/src/ui/appearance_settings.rs`'s tests hold the two spellings equal.
///
/// ⚠ **A word added to one of these lists later is a STORE-FORMAT change for every older
/// binary.** The loader refuses a word it does not know (`one_of`), exactly as it refuses a key it
/// does not know, so once a newer build writes its new word, an older build reading the same store
/// refuses the store rather than falling back to a default. That is deliberate — a word the GUI
/// cannot show would otherwise display as effective in `config show` — and it holds for the
/// BACKEND's store too: an appearance row written there (these keys are editable in Connections →
/// Backend settings) stops an older daemon's boot after a rollback, although no daemon reads it.
/// Rebuild every binary that reads a store before writing a new word into it.
pub const THEMES: [&str; 4] = ["graphite", "midnight", "dusk", "carbon"];
/// `preferences.market_colors`'s words (spec §3.2). Held equal to `MarketId::key` the same way;
/// the store-format warning on [`THEMES`] applies.
pub const MARKET_COLOR_SETS: [&str; 4] = ["classic", "tradingview", "exchange", "colorblind"];
/// `preferences.density`'s words (spec §3.4); the warning on [`THEMES`] applies.
pub const DENSITIES: [&str; 3] = ["compact", "normal", "comfortable"];
/// `preferences.text_size`'s words (spec §3.3); the warning on [`THEMES`] applies. Three since
/// 2026-10-05: `small` is the scale `standard` used to be, `standard` (still the default) is the one
/// `large` used to be, and `large` is new. A store row written before then keeps its word and
/// means the next scale up for `standard` and `large` alike.
pub const TEXT_SIZES: [&str; 3] = ["small", "standard", "large"];

/// `preferences.observe_stall_timeout_ms`'s built-in value AND its floor, in milliseconds: how long a
/// subscribed observe stream may be silent before the client treats the link as dead. RAISE-ONLY — a
/// row below it is refused, because a shorter wait than three of the node's heartbeats tears a healthy
/// but loaded link down. Restated here (this crate, layer 20, cannot name the layer-25 constant it
/// mirrors, `vike_tradehub_client::liveness::OBSERVE_READ_TIMEOUT`) and held equal to it by
/// `crates/vike-app-core/src/backend/observe_bridge_tests.rs`'s
/// `the_link_patience_bounds_restate_the_liveness_constants`.
pub const DEFAULT_OBSERVE_STALL_TIMEOUT_MS: u32 = 45_000;
/// `preferences.observe_stall_timeout_ms`'s ceiling: five minutes, past which a silently dead link
/// is noticed too late to matter to a person watching the frame. Refused above, never clamped.
pub const MAX_OBSERVE_STALL_TIMEOUT_MS: u32 = 300_000;
/// `preferences.control_reply_timeout_ms`'s built-in value AND its floor, in milliseconds: how long a
/// control connection waits for the node's reply to a command before it gives the command up as
/// outcome-unknown. RAISE-ONLY for the reason [`DEFAULT_OBSERVE_STALL_TIMEOUT_MS`] gives; the node's
/// settings-write budget is argued against this FLOOR (`crates/vike-tradehub/src/server/settings.rs`),
/// so every accepted value keeps that margin. Held equal to
/// `vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT` by the same test.
pub const DEFAULT_CONTROL_REPLY_TIMEOUT_MS: u32 = 30_000;
/// `preferences.control_reply_timeout_ms`'s ceiling: ten minutes, the bound the one-shot replies'
/// own compile-time assertion puts on a reply wait. Refused above, never clamped.
pub const MAX_CONTROL_REPLY_TIMEOUT_MS: u32 = 600_000;
// A window with no room above its floor would be a row that can only restate the default.
const _: () = assert!(
    DEFAULT_OBSERVE_STALL_TIMEOUT_MS < MAX_OBSERVE_STALL_TIMEOUT_MS
        && DEFAULT_CONTROL_REPLY_TIMEOUT_MS < MAX_CONTROL_REPLY_TIMEOUT_MS,
    "each link-patience window needs room above its floor to be a setting at all"
);

/// The defaults (spec §5's table).
pub const DEFAULT_THEME: &str = "graphite";
pub const DEFAULT_MARKET_COLORS: &str = "classic";
pub const DEFAULT_DENSITY: &str = "normal";
pub const DEFAULT_TEXT_SIZE: &str = "standard";
/// `preferences.vsync`'s default: on. It is also what the GUI library does when nobody configures
/// anything, so a default install has no "configured" startup attempt to distinguish from the
/// library's own (`crates/vike-app-core/src/ui/gpu_startup.rs`'s `plan_rungs` relies on that, and
/// its tests hold the two equal).
pub const DEFAULT_VSYNC: bool = true;

/// [`Preferences::observe_poll_ms`]'s default, in milliseconds: the sleep between the desktop's
/// observe bridge reads of the push-fed cell, which it has always been (it capped a refresh at
/// about twenty a second before the row existed).
pub const DEFAULT_OBSERVE_POLL_MS: u32 = 50;
/// [`Preferences::observe_poll_ms`]'s floor. Below ten milliseconds the bridge thread spins on a
/// cell whose frames arrive at the node's publish cadence, burning a core for repaints the GUI
/// cannot draw.
pub const MIN_OBSERVE_POLL_MS: u32 = 10;
/// [`Preferences::observe_poll_ms`]'s ceiling. Past one second the chart trails the node by more
/// than a human reads as live.
pub const MAX_OBSERVE_POLL_MS: u32 = 1_000;

/// [`Preferences::chart_history_bars`]'s default: how many bars back one chart read of the
/// backend's store asks for. It is the depth the desktop always used.
pub const DEFAULT_CHART_HISTORY_BARS: u32 = 3_000;
/// [`Preferences::chart_history_bars`]'s floor: a read shallower than this paints a chart a screen
/// wide with no history to scroll into.
pub const MIN_CHART_HISTORY_BARS: u32 = 300;
/// [`Preferences::chart_history_bars`]'s ceiling, and it is set by a number in another crate: the
/// GUI's bar store trims what it holds per series to
/// `vike_app_core::data::data_sink::DIRECT_BAR_CLOSED_CAP`, and a read deeper than that cap would
/// have its oldest rows dropped on the way in, so the chart would show less history than the store
/// returned. A `const` assertion in `vike-app-core` keeps the cap above this number.
pub const MAX_CHART_HISTORY_BARS: u32 = 4_000;

/// [`Preferences::backfill_lookback_days`]'s default: how far back the Data Manager's bulk
/// Backfill asks for a series that has no recorded gap. It is the thirty days the desktop always
/// used.
pub const DEFAULT_BACKFILL_LOOKBACK_DAYS: u32 = 30;
/// [`Preferences::backfill_lookback_days`]'s floor: a window of less than one day backfills nothing
/// a refresh would not.
pub const MIN_BACKFILL_LOOKBACK_DAYS: u32 = 1;
/// [`Preferences::backfill_lookback_days`]'s ceiling: a bulk backfill over many symbols pages every
/// one of them this far back, and a year is already a long run of venue requests.
pub const MAX_BACKFILL_LOOKBACK_DAYS: u32 = 365;

/// [`Preferences::ai_anthropic_model`]'s default: the model id the Studio chat has always asked
/// Anthropic for.
pub const DEFAULT_AI_ANTHROPIC_MODEL: &str = "claude-opus-4-8";
/// [`Preferences::ai_cerebras_model`]'s default: the model id the Studio chat has always asked
/// Cerebras for.
pub const DEFAULT_AI_CEREBRAS_MODEL: &str = "llama-3.3-70b";
/// The longest model id either `ai_*_model` row takes, in characters.
pub const MAX_AI_MODEL_ID_CHARS: usize = 100;
/// The characters a model id may carry besides ASCII letters and digits (`claude-opus-4-8`,
/// `llama-3.3-70b`, `meta/llama:70b`).
pub const AI_MODEL_ID_PUNCTUATION: &str = "._:/-";

/// [`Preferences::ai_timeout_secs`]'s default: the whole-request HTTP bound the Studio chat has
/// always used.
pub const DEFAULT_AI_TIMEOUT_SECS: u32 = 120;
/// [`Preferences::ai_timeout_secs`]'s floor: below half a minute a large model's slower turns fail
/// as timeouts instead of as answers.
pub const MIN_AI_TIMEOUT_SECS: u32 = 30;
/// [`Preferences::ai_timeout_secs`]'s ceiling: the request runs on a worker thread, and a quarter
/// of an hour is a wedged connection rather than a slow model.
pub const MAX_AI_TIMEOUT_SECS: u32 = 900;

/// Operator taste and tuning.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preferences {
    /// Console log filter directive (`info`, `vike_core=debug`, …).
    ///
    /// Was `VIKE_LOG`. Defaults to [`DEFAULT_LOG_LEVEL`]. `RUST_LOG`, when set, still filters the
    /// CONSOLE above this row inside `vike_log` — a developer switch, never a setting.
    pub log_level: String,

    /// JSON log-FILE level. Independent of `log_level`, which filters the console only.
    ///
    /// Defaults to [`DEFAULT_LOG_FILE_LEVEL`]. Every binary that writes a rolling file reads this
    /// row BEFORE its subscriber exists — the daemons and the desktop from their boot, the batch
    /// tools (which compile in their own quieter default) through [`crate::log_file_level`] —
    /// and hands it to `vike_log` as a parameter. Was `VIKE_LOG_FILE_LEVEL`.
    pub log_file_level: String,

    /// Default chart style for new chart windows. `None` = the GUI's own default.
    ///
    /// Was `VIKE_STYLE`.
    pub chart_style: Option<String>,

    /// Bound on the bounded rayon pool a parameter sweep installs. `None` =
    /// `min(4, available_parallelism())`, `vike-backtest`'s own fallback.
    ///
    /// Was `VIKE_SWEEP_THREADS`.
    pub sweep_threads: Option<usize>,

    // -- The desktop's tuning rows and the Studio chat's. Each carries today's value as its
    //    default, so a box with no row behaves exactly as it did; a value outside the stated
    //    range is REFUSED at the write and at the load, and nothing clamps it. Read by the
    //    desktop on the PC, from the PC's own database. ⚠ A row an older binary does not know
    //    makes it refuse the store: unset the key before downgrading.
    /// The sleep between the desktop's observe bridge reads of the snapshot the node pushed, in
    /// milliseconds, from [`MIN_OBSERVE_POLL_MS`] to [`MAX_OBSERVE_POLL_MS`]. Defaults to
    /// [`DEFAULT_OBSERVE_POLL_MS`].
    ///
    /// The bridge thread (never the GUI's frame thread) polls the cell the node's frames land in
    /// and publishes a new one to the chart, so this is the ceiling on how often a chart can
    /// refresh from the node: the default is about twenty times a second. A smaller value
    /// refreshes sooner and costs the bridge thread more wakeups; a larger one trades latency for
    /// fewer. A stop (a backend switch, the window closing) is noticed within one short slice
    /// whatever this is set to; a dropped link is noticed on the next poll.
    pub observe_poll_ms: u32,

    /// How many bars back one chart read of the backend's store asks for, from
    /// [`MIN_CHART_HISTORY_BARS`] to [`MAX_CHART_HISTORY_BARS`]. Defaults to
    /// [`DEFAULT_CHART_HISTORY_BARS`].
    ///
    /// The depth of history a chart has to scroll into when it is opened on an interval the node
    /// does not stream. The server's own seed depth is a separate, fixed number and is not
    /// affected. The ceiling is the number the GUI's bar store trims each series to, so a full
    /// answer is never cut on the way in.
    pub chart_history_bars: u32,

    /// How many days back the Data Manager's bulk Backfill / Update asks for a series that has no
    /// recorded gap, from [`MIN_BACKFILL_LOOKBACK_DAYS`] to [`MAX_BACKFILL_LOOKBACK_DAYS`].
    /// Defaults to [`DEFAULT_BACKFILL_LOOKBACK_DAYS`].
    ///
    /// Still clamped, per venue, to the oldest history that venue is known to serve; a series with
    /// recorded gaps backfills those gaps and ignores this window.
    pub backfill_lookback_days: u32,

    /// The model id the Studio chat asks Anthropic for: 1 to [`MAX_AI_MODEL_ID_CHARS`] characters
    /// from ASCII letters and digits and [`AI_MODEL_ID_PUNCTUATION`]. Defaults to
    /// [`DEFAULT_AI_ANTHROPIC_MODEL`].
    ///
    /// Only the SHAPE is checked here; the provider decides whether the id exists. An id it does
    /// not know comes back as an HTTP error line in the chat pane (a 4xx from the provider), and
    /// nothing else changes: the chat generates and backtests strategies, and places no order.
    pub ai_anthropic_model: String,

    /// The model id the Studio chat asks Cerebras for; the same rule as
    /// [`Preferences::ai_anthropic_model`]. Defaults to [`DEFAULT_AI_CEREBRAS_MODEL`].
    pub ai_cerebras_model: String,

    /// The whole-request HTTP timeout of the Studio chat's provider calls, in seconds, from
    /// [`MIN_AI_TIMEOUT_SECS`] to [`MAX_AI_TIMEOUT_SECS`]. Defaults to
    /// [`DEFAULT_AI_TIMEOUT_SECS`]. It bounds one request to the provider, not the whole
    /// generate-and-repair loop, which makes several.
    pub ai_timeout_secs: u32,

    /// **The GUI's theme** (design system spec §5): one of [`THEMES`]. Read by the desktop at
    /// start and changed live by its Settings window. ⚠ No environment variable, by ruling: an
    /// appearance is a person's preference, not a deployment knob.
    pub theme: String,
    /// The GUI's market-colour set: one of [`MARKET_COLOR_SETS`].
    pub market_colors: String,
    /// The chart's background gradient in window headers. Off by default.
    pub header_gradient: bool,
    /// The GUI's density: one of [`DENSITIES`].
    pub density: String,
    /// The GUI's text size: one of [`TEXT_SIZES`].
    pub text_size: String,

    /// The ADVISORY order-quantity cap `vike-cli`'s `trade` and `mcp` previews flag an order
    /// against — finite and above 0. Advisory, which is why it is a preference and not `policy`:
    /// the preview marks an order over it and refuses nothing, and the node enforces its own
    /// limits regardless. `None` — the default — is no quantity cap.
    ///
    /// Read on the CLIENT's box, from that box's own database. Was `VIKE_MAX_ORDER_QTY`.
    pub max_order_qty: Option<f64>,

    /// Where the desktop's File → Export chart image… writes its PNGs. `None` — the default — is
    /// `<exe_dir>/exports`, the same `<exe_dir>/…` convention the log fallback uses.
    ///
    /// Read by the desktop on the PC, from the PC's own database. Was `VIKE_EXPORT_DIR`.
    pub export_dir: Option<PathBuf>,

    /// **The operator's patience on the OBSERVE stream**, in milliseconds: how long a client waits
    /// in silence on a subscribed observe stream before it ends the link and reconnects. The node
    /// beats every 15 s whatever this says (a protocol fact, `vike_tradehub_client::liveness`), so
    /// this is only the CLIENT's tolerance for a bad link — a flaky Wi-Fi or VPN that tears the
    /// stream down repeatedly wants it raised.
    ///
    /// Defaults to [`DEFAULT_OBSERVE_STALL_TIMEOUT_MS`], which is also the floor: a value below it
    /// is REFUSED, at the write and at the load, never clamped; above [`MAX_OBSERVE_STALL_TIMEOUT_MS`]
    /// is refused the same way. Armed only against a node that advertises its heartbeat. Read by the
    /// desktop and by `vike-cli`'s `trade` and `mcp`, from that box's own database (a client's
    /// preference: a second machine on a better link would not need it).
    pub observe_stall_timeout_ms: u32,

    /// **The operator's patience on a CONTROL reply**, in milliseconds: how long a control
    /// connection waits for the node's answer to a command it wrote before giving that command up
    /// as outcome-unknown and ending the link. A slow link that reaps healthy control connections
    /// wants it raised.
    ///
    /// Defaults to [`DEFAULT_CONTROL_REPLY_TIMEOUT_MS`], which is also the floor (raise-only, as
    /// [`Preferences::observe_stall_timeout_ms`]); above [`MAX_CONTROL_REPLY_TIMEOUT_MS`] is refused.
    /// Read by the desktop's write channel and by `vike-cli`'s `trade` and `mcp`. It does not move
    /// the one-shot replies (the directory read, the strategy and settings verbs), which keep their
    /// own fixed bounds.
    pub control_reply_timeout_ms: u32,

    /// **Whether the desktop waits for the display's vertical blank when it presents a frame.**
    /// `true` (the default, [`DEFAULT_VSYNC`]) is wgpu's `AutoVsync` present mode, `false` its
    /// `AutoNoVsync`, and nothing else is expressible: an explicit `Fifo`/`Mailbox`/`Immediate`/
    /// `FifoRelaxed` is a mode the adapter may not support, support cannot be asked before the
    /// window exists, and wgpu PANICS on an unsupported explicit mode. The two `Auto*` modes fall
    /// back to one every adapter has.
    ///
    /// STARTUP-ONLY: the present mode is fixed when the window opens, so a change lands at the next
    /// start (the Settings window says so beside the control). With it off the GUI library has no
    /// frame-rate cap of its own, so the desktop adds one (`crates/vike-app-core/src/ui/frame_cap.rs`).
    ///
    /// Read by the desktop on the PC, from the PC's own database; a row set on a headless box
    /// does nothing.
    pub vsync: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            log_level: DEFAULT_LOG_LEVEL.to_string(),
            log_file_level: DEFAULT_LOG_FILE_LEVEL.to_string(),
            chart_style: None,
            sweep_threads: None,
            observe_poll_ms: DEFAULT_OBSERVE_POLL_MS,
            chart_history_bars: DEFAULT_CHART_HISTORY_BARS,
            backfill_lookback_days: DEFAULT_BACKFILL_LOOKBACK_DAYS,
            ai_anthropic_model: DEFAULT_AI_ANTHROPIC_MODEL.to_string(),
            ai_cerebras_model: DEFAULT_AI_CEREBRAS_MODEL.to_string(),
            ai_timeout_secs: DEFAULT_AI_TIMEOUT_SECS,
            theme: DEFAULT_THEME.to_string(),
            market_colors: DEFAULT_MARKET_COLORS.to_string(),
            header_gradient: false,
            density: DEFAULT_DENSITY.to_string(),
            text_size: DEFAULT_TEXT_SIZE.to_string(),
            max_order_qty: None,
            export_dir: None,
            observe_stall_timeout_ms: DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
            control_reply_timeout_ms: DEFAULT_CONTROL_REPLY_TIMEOUT_MS,
            vsync: DEFAULT_VSYNC,
        }
    }
}

/// The FILE shape of [`Preferences`] — all-optional, unknown keys rejected by name.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesPatch {
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Preferences`] field.
    ///
    /// Parsed only so [`Preferences::apply`] can refuse the key by name and say where the number
    /// lives now. See this module's doc for why a preference that validated, displayed and clamped
    /// while changing nothing is worse than one that never existed.
    pub rate_utilization: Option<f64>,
    /// See [`Preferences::log_level`].
    pub log_level: Option<String>,
    /// See [`Preferences::log_file_level`].
    pub log_file_level: Option<String>,
    /// See [`Preferences::chart_style`].
    pub chart_style: Option<String>,
    /// See [`Preferences::sweep_threads`].
    pub sweep_threads: Option<usize>,
    /// See [`Preferences::observe_poll_ms`].
    pub observe_poll_ms: Option<u32>,
    /// See [`Preferences::chart_history_bars`].
    pub chart_history_bars: Option<u32>,
    /// See [`Preferences::backfill_lookback_days`].
    pub backfill_lookback_days: Option<u32>,
    /// See [`Preferences::ai_anthropic_model`].
    pub ai_anthropic_model: Option<String>,
    /// See [`Preferences::ai_cerebras_model`].
    pub ai_cerebras_model: Option<String>,
    /// See [`Preferences::ai_timeout_secs`].
    pub ai_timeout_secs: Option<u32>,
    /// See [`Preferences::theme`].
    pub theme: Option<String>,
    /// See [`Preferences::market_colors`].
    pub market_colors: Option<String>,
    /// See [`Preferences::header_gradient`].
    pub header_gradient: Option<bool>,
    /// See [`Preferences::density`].
    pub density: Option<String>,
    /// See [`Preferences::text_size`].
    pub text_size: Option<String>,
    /// See [`Preferences::max_order_qty`].
    pub max_order_qty: Option<f64>,
    /// See [`Preferences::export_dir`].
    pub export_dir: Option<PathBuf>,
    /// See [`Preferences::observe_stall_timeout_ms`].
    pub observe_stall_timeout_ms: Option<u32>,
    /// See [`Preferences::control_reply_timeout_ms`].
    pub control_reply_timeout_ms: Option<u32>,
    /// See [`Preferences::vsync`]. A value that is not a boolean never reaches [`Preferences::apply`]:
    /// the type refuses it at the write and at the load.
    pub vsync: Option<bool>,
}

impl Preferences {
    /// Fold one file's patch in, validating against `file` so an error names it.
    pub(crate) fn apply(
        &mut self,
        patch: PreferencesPatch,
        file: &Path,
    ) -> Result<(), ConfigError> {
        // TOMBSTONE — see `PreferencesPatch::rate_utilization`. Refused, never applied.
        if let Some(v) = patch.rate_utilization {
            return Err(ConfigError::value(
                file,
                "rate_utilization",
                v,
                &format!(
                    "is no longer a preference — NOTHING read it. Every pacer takes its target \
                     fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION, \
                     so this key validated, displayed as effective, and changed nothing (a \
                     `policy.rate.max_utilization` row has the same problem). \
                     {}. The knob returns, per-venue, when \
                     vike_model::RateLimitConfig can reach a pacer",
                    stale_row_removal("preferences.rate_utilization")
                ),
            ));
        }
        if let Some(v) = patch.log_level {
            check_non_blank(file, "log_level", &v)?;
            self.log_level = v;
        }
        if let Some(v) = patch.log_file_level {
            check_non_blank(file, "log_file_level", &v)?;
            self.log_file_level = v;
        }
        if let Some(v) = patch.chart_style {
            check_non_blank(file, "chart_style", &v)?;
            self.chart_style = Some(v);
        }
        if let Some(v) = patch.sweep_threads {
            if v == 0 {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: "sweep_threads".to_string(),
                    message: "0 would install a pool that never runs a point — omit the key for \
                              the default min(4, available_parallelism())"
                        .to_string(),
                });
            }
            self.sweep_threads = Some(v);
        }
        if let Some(v) = patch.observe_poll_ms {
            if !(MIN_OBSERVE_POLL_MS..=MAX_OBSERVE_POLL_MS).contains(&v) {
                return Err(ConfigError::out_of_range(
                    file,
                    "observe_poll_ms",
                    v,
                    MIN_OBSERVE_POLL_MS,
                    MAX_OBSERVE_POLL_MS,
                    "below the floor the bridge thread spins for repaints the GUI cannot draw, \
                     above the ceiling the chart trails the node by more than a second",
                ));
            }
            self.observe_poll_ms = v;
        }
        if let Some(v) = patch.chart_history_bars {
            if !(MIN_CHART_HISTORY_BARS..=MAX_CHART_HISTORY_BARS).contains(&v) {
                return Err(ConfigError::out_of_range(
                    file,
                    "chart_history_bars",
                    v,
                    MIN_CHART_HISTORY_BARS,
                    MAX_CHART_HISTORY_BARS,
                    "the ceiling keeps a full answer under the GUI bar store's per-series cap, so \
                     a read is never trimmed on the way in",
                ));
            }
            self.chart_history_bars = v;
        }
        if let Some(v) = patch.backfill_lookback_days {
            if !(MIN_BACKFILL_LOOKBACK_DAYS..=MAX_BACKFILL_LOOKBACK_DAYS).contains(&v) {
                return Err(ConfigError::out_of_range(
                    file,
                    "backfill_lookback_days",
                    v,
                    MIN_BACKFILL_LOOKBACK_DAYS,
                    MAX_BACKFILL_LOOKBACK_DAYS,
                    "a bulk backfill pages every selected series this far back",
                ));
            }
            self.backfill_lookback_days = v;
        }
        if let Some(v) = patch.ai_anthropic_model {
            check_model_id(file, "ai_anthropic_model", &v)?;
            self.ai_anthropic_model = v;
        }
        if let Some(v) = patch.ai_cerebras_model {
            check_model_id(file, "ai_cerebras_model", &v)?;
            self.ai_cerebras_model = v;
        }
        if let Some(v) = patch.ai_timeout_secs {
            if !(MIN_AI_TIMEOUT_SECS..=MAX_AI_TIMEOUT_SECS).contains(&v) {
                return Err(ConfigError::out_of_range(
                    file,
                    "ai_timeout_secs",
                    v,
                    MIN_AI_TIMEOUT_SECS,
                    MAX_AI_TIMEOUT_SECS,
                    "below the floor a large model's slower turns fail as timeouts, above the \
                     ceiling a wedged connection holds the chat's worker thread",
                ));
            }
            self.ai_timeout_secs = v;
        }
        if let Some(v) = patch.theme {
            self.theme = one_of(file, "theme", v, &THEMES)?;
        }
        if let Some(v) = patch.market_colors {
            self.market_colors = one_of(file, "market_colors", v, &MARKET_COLOR_SETS)?;
        }
        if let Some(v) = patch.header_gradient {
            self.header_gradient = v;
        }
        if let Some(v) = patch.density {
            self.density = one_of(file, "density", v, &DENSITIES)?;
        }
        if let Some(v) = patch.text_size {
            self.text_size = one_of(file, "text_size", v, &TEXT_SIZES)?;
        }
        if let Some(v) = patch.max_order_qty {
            if !(v.is_finite() && v > 0.0) {
                return Err(ConfigError::value(
                    file,
                    "max_order_qty",
                    v,
                    "is not a quantity cap: it must be a finite number above 0 — omit the key for \
                     no cap",
                ));
            }
            self.max_order_qty = Some(v);
        }
        if let Some(v) = patch.export_dir {
            if v.as_os_str().to_string_lossy().trim().is_empty() {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: "export_dir".to_string(),
                    message: "\"\" is blank — omit the key to keep the default <exe_dir>/exports"
                        .to_string(),
                });
            }
            self.export_dir = Some(v);
        }
        if let Some(v) = patch.observe_stall_timeout_ms {
            check_patience(
                file,
                "observe_stall_timeout_ms",
                v,
                DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
                MAX_OBSERVE_STALL_TIMEOUT_MS,
            )?;
            self.observe_stall_timeout_ms = v;
        }
        if let Some(v) = patch.control_reply_timeout_ms {
            check_patience(
                file,
                "control_reply_timeout_ms",
                v,
                DEFAULT_CONTROL_REPLY_TIMEOUT_MS,
                MAX_CONTROL_REPLY_TIMEOUT_MS,
            )?;
            self.control_reply_timeout_ms = v;
        }
        if let Some(v) = patch.vsync {
            self.vsync = v;
        }
        Ok(())
    }
}

/// A link-patience window in milliseconds, RAISE-ONLY: `v` must lie in `floor..=max`, where `floor`
/// is the built-in value. Below it the client would give up on a healthy link sooner than it does
/// today; above `max` a dead link would go unnoticed for too long. Refused either way, never
/// clamped (a clamp is a setting the operator believes they filed and has not).
fn check_patience(file: &Path, key: &str, v: u32, floor: u32, max: u32) -> Result<(), ConfigError> {
    let message = if v < floor {
        format!(
            "{v} is below the minimum {floor}: this window can only be RAISED above its built-in \
             value (a shorter wait tears healthy links down); omit the key for the default"
        )
    } else if v > max {
        format!("{v} exceeds the allowed maximum {max} (milliseconds)")
    } else {
        return Ok(());
    };
    Err(ConfigError::Value { file: file.to_path_buf(), key: key.to_string(), message })
}

fn check_non_blank(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    if v.trim().is_empty() {
        return Err(ConfigError::Value {
            file: file.to_path_buf(),
            key: key.to_string(),
            message: "\"\" is blank — omit the key to keep the default".to_string(),
        });
    }
    Ok(())
}

/// A model id the Studio chat may send a provider: 1 to [`MAX_AI_MODEL_ID_CHARS`] characters, each
/// an ASCII letter or digit or one of [`AI_MODEL_ID_PUNCTUATION`]. Blank, padded, over-long and
/// foreign-character values are all REFUSED; nothing trims or lower-cases one into shape. The value
/// is not echoed, for the reason [`one_of`] gives: a credential pasted into the wrong key must not
/// be printed by the refusal. Whether the PROVIDER knows the id is not this crate's question.
fn check_model_id(file: &Path, key: &str, v: &str) -> Result<(), ConfigError> {
    let shaped = (1..=MAX_AI_MODEL_ID_CHARS).contains(&v.chars().count())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || AI_MODEL_ID_PUNCTUATION.contains(c));
    if shaped {
        return Ok(());
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!(
            "… is not a model id: it must be 1 to {MAX_AI_MODEL_ID_CHARS} characters, each an \
             ASCII letter or digit or one of `{AI_MODEL_ID_PUNCTUATION}` — omit the key for the \
             default"
        ),
    })
}

/// `v` when it is one of `words`, else a refusal naming the key and the whole list. A word the GUI
/// has no option for would otherwise display as effective in `config show` while the GUI fell
/// back to its default — positive confirmation of something false (`crate::consumed`'s module
/// doc). The value itself is not echoed: a credential pasted into the wrong key must not be
/// printed by the refusal.
fn one_of(file: &Path, key: &str, v: String, words: &[&str]) -> Result<String, ConfigError> {
    if words.contains(&v.as_str()) {
        return Ok(v);
    }
    Err(ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!("… must be one of: {}", words.join(", ")),
    })
}

// ⚠ `Preferences`' environment layer is DELETED (decision 0111): every key is its
// `preferences.*` row, and each former variable is a `crate::REMOVED_ENV` refusal. `RUST_LOG`
// stays in the environment, as the console logger's own developer switch (`vike_log` reads it;
// this crate does not), and so do the GUI library's `WGPU_BACKEND` and `WGPU_POWER_PREF`, which the
// graphics stack (eframe, egui-wgpu, wgpu) reads inside its own `NativeOptions::default()`: third-
// party developer switches, not settings, and not read by any code of ours (decision 0111's
// "AMENDED 2026-10-13" note; `docs/ops/desktop-gpu.md`'s "Troubleshooting").

impl CliOverride for Preferences {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = &cli.log_level {
            self.log_level = v.clone();
        }
        Ok(())
    }
}

#[path = "preferences_tests.rs"]
#[cfg(test)]
mod preferences_tests;
