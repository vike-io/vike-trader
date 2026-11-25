//! A strategy FILE: one `.rs` or `.rhai` file under `<project>/strategies/`, its declarations in a
//! header inside the file. This module is the header's format, its parser, the load-time checks of
//! a `[strategy.params]` table against it, and the scan of the folder ([`scan`]).
//!
//! # The format
//!
//! The header is the FIRST fenced `toml` block in the file's leading comment lines. "Leading" means
//! from the first line down to the first line that is neither a comment (`//`, `///`, `//!`) nor
//! blank. `//` is a comment in Rust and in Rhai, so one rule serves both, and rustdoc never
//! compiles a `toml` block. The comment marker and ONE space after it are stripped from each line.
//!
//! ```rust,ignore
//! //! Grid: a ladder of resting limits around an anchor.
//! //!
//! //! ```toml
//! //! [strategy]
//! //! runs_on = "live"            # backtest | paper | live (absent: paper)
//! //! why = "rests only post-only limits inside the band"   # why it is safe live
//! //! routes = "single_leg"       # single_leg | multi_leg (absent: single_leg)
//! //! # multi_leg_why = "…"       # required with multi_leg, refused without it
//! //! optimistic_quotes = false   # backtest only; a stated profile key wins (absent: false)
//! //! fill = "next_open"          # next_open | close: where a bar's market order fills (absent: next_open)
//! //! # sdk = 3                   # a .rs file's pin: the numbered SDK it builds against (absent: the newest)
//! //!
//! //! [params.anchor]
//! //! type = "text"               # number | integer | text | bool | table | text_or_integer | array | model
//! //! default = "first"           # shown to the user; the host never injects it
//! //! [params.anchor_price]
//! //! type = "number"
//! //! needs = { anchor = ["fixed"] }     # consumed only while `anchor` resolved to "fixed"
//! //! [params.rungs]
//! //! type = "integer"
//! //! range = [1, 500]            # inclusive; a value outside is refused
//! //! [params.symbol]
//! //! type = "text"
//! //! route = "symbol"            # symbol | venue | venue_map: a key naming WHERE an order goes
//! //!
//! //! [run]                       # order-language files only: the `vike_tactics::RuleRunOptions`
//! //! bound_exits = true
//! //! ```
//! ```
//!
//! Every table refuses an unknown key (`deny_unknown_fields`). The rules:
//!
//! * **`[strategy]` is required**; each of its keys is optional, with the default above.
//!   `runs_on`: `backtest` (simulator only), `paper` (also the paper core) or `live` (also a demo or
//!   live account). `why` is the deleted registry's `LIVE_CAPABLE` reason, moved verbatim.
//!   `multi_leg_why` is its `ParamRoutes::MultiLeg` reason, moved verbatim: required with
//!   `routes = "multi_leg"`, refused without. `fill` is when an order the strategy places on a bar
//!   fills, read by the backtest and the paper book from here alone (decision 0285's amendment of
//!   2026-11-16): `next_open` at the first price after that bar (the next bar's open in a backtest,
//!   the first quote of the next bar on paper), `close` at that bar's close. Any other word is refused.
//!   `sdk` is a `.rs` file's PIN, an integer: the numbered, frozen SDK it builds against
//!   (`docs/decisions/0296-a-rust-strategy-file-builds-against-a-numbered-frozen-sdk-it-may-pin.md`);
//!   absent, the newest SDK on the box. A `.rhai` file naming it is refused
//!   ([`StrategyFileHeader::parse_file`]): it runs in-process on this vike's own code.
//! * **`[params.<key>]`, one per key the file reads.** `type` is required. `default` must be of
//!   that type and inside `range`. `range` is `[low, high]`, inclusive, on a `number` or `integer`
//!   only. `needs` is the registry's `PARAM_GATES` row of this key: `{ other = ["value", …] }`
//!   (consumed only while `other` RESOLVED to one of the values) or `{ other = "positive" }`
//!   (consumed only while `other` resolved to a number above zero); every entry must hold, and
//!   `other` is a declared key. `route` is the registry's `PARAM_ROUTES` kind of this key; a
//!   `symbol` or `venue` route is a `text` key, a `venue_map` route a `table` key.
//! * **`type = "model"`** (opt-in, W32): the profile or mount names a RUN ID, and the file is handed
//!   that run's model TEXT in its place ([`StrategyFileHeader::with_model_texts`]). A file reads no
//!   file; its host does.
//! * **A file with no `[params]` accepts NO key**: every key a profile hands it is refused with
//!   "declare it in the file's header".
//! * **`[run]`**: `bound_exits`, `stacked_closes`, `touch_at_market` (absent: `false`) and
//!   `fills_a_day` (absent: no cap): today's `vike_tactics::RuleRunOptions`, for a file whose entry is
//!   `pub fn rule`.
//!
//! # The entry a file exports
//!
//! One of `pub fn build<B: vike_model::OrderBroker + 'static>(params: &Params) -> Box<dyn
//! Strategy<B> + Send>` or `pub fn rule(params: &Params) -> Box<dyn vike_tactics::Rule + Send>`,
//! plus optionally `pub fn refuse(params: &Params) -> Option<String>` (a whole-table refusal, the
//! registry's `unarmable_params`) and `pub fn echo(params: &Params) -> Vec<(&'static str, String)>`
//! (the registry's `resolved_params`, the rows [`StrategyFileHeader::unmet_needs`] judges), or
//! `echo(params, facts)` for a file whose effective params are its mount's facts too. The
//! template that builds a `.rs` file calls them; this module reads the header only.
//!
//! # The load-time checks
//!
//! [`StrategyFileHeader::unknown_params`], [`StrategyFileHeader::mistyped_params`],
//! [`StrategyFileHeader::misrouted_params`], [`StrategyFileHeader::out_of_range_params`] and
//! [`StrategyFileHeader::unmet_needs`]. Their findings render in the words of the registry checks
//! they replaced (its `unknown_params`, `mistyped_params`, `misrouted_params` and `Gate::unmet`),
//! byte for byte: the words MOVED here, and were held equal to the registry's over every registry
//! row until the registry and its compiled strategies were deleted. The sentence around a finding
//! is the consumer's.

use serde::Deserialize;
use vike_model::params::ParamValue;

mod header;
mod model;
mod params_check;
pub mod scan;

#[cfg(test)]
mod tests;

/// A strategy file's header: the `toml` block of the module doc's format, parsed and checked.
/// Built only by [`StrategyFileHeader::parse`] (a whole file) or
/// [`StrategyFileHeader::from_toml`] (the block's text), each of which also runs the header's own
/// checks.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyFileHeader {
    /// `[strategy]`.
    pub strategy: StrategyFileHeaderStrategy,
    /// `[params.<key>]`, in the order the header declares them (the order a "here is what it
    /// reads" list prints). Empty: the file accepts no key.
    #[serde(default, deserialize_with = "header::in_document_order")]
    pub params: Vec<(String, StrategyFileHeaderParam)>,
    /// `[run]`, for an order-language file. Absent: every switch off.
    pub run: Option<StrategyFileHeaderRun>,
}

/// The `[strategy]` table: where the file may run and how its orders are routed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyFileHeaderStrategy {
    /// Where it may run. Absent: [`StrategyFileRunsOn::Paper`] (decision 0273, answer 1).
    #[serde(default)]
    pub runs_on: StrategyFileRunsOn,
    /// Why it is safe on a live account: the registry's `LIVE_CAPABLE` reason, moved verbatim.
    pub why: Option<String>,
    /// Whether its route keys name the mount's own market. Absent: single-leg.
    #[serde(default)]
    pub routes: StrategyFileRoutes,
    /// Why the route keys name legs that are not the mount's own: the registry's
    /// `ParamRoutes::MultiLeg` reason, moved verbatim. Required with `routes = "multi_leg"`, refused without it.
    pub multi_leg_why: Option<String>,
    /// The backtest's optimistic quote fills (the registry's `OPTIMISTIC_QUOTES`); a key the profile
    /// states wins. Absent: `false`.
    #[serde(default)]
    pub optimistic_quotes: bool,
    /// When an order the strategy places on a bar fills, for the backtest and the paper book alike.
    /// Absent: [`StrategyFileFill::NextOpen`].
    #[serde(default)]
    pub fill: StrategyFileFill,
    /// The numbered SDK a `.rs` file builds against (decision 0296, verdict 2): an exact pin, never a
    /// range. Absent: the newest SDK on the box, resolved at each build and recorded. A `.rhai` file
    /// may not name it ([`StrategyFileHeader::parse_file`]).
    pub sdk: Option<u32>,
}

/// `fill`: when an order a strategy places on a bar fills (decision 0285's amendment of 2026-11-16).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyFileFill {
    /// At the first price after the bar: the next bar's open in a backtest, the next bar's first
    /// quote on a paper book.
    #[default]
    NextOpen,
    /// At the bar's own close (a TradingView `process_orders_on_close` script).
    Close,
}

/// `runs_on`: the furthest a file may run. Each step includes the ones before it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyFileRunsOn {
    /// The simulator only (the registry's `NotLive` and `SimulatorOnly`).
    Backtest,
    /// Also the paper core.
    #[default]
    Paper,
    /// Also a demo or live account.
    Live,
}

/// `routes`: the registry's `ParamRoutes::SingleLeg` / `ParamRoutes::MultiLeg`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyFileRoutes {
    /// Every route key names the ONE market the mount trades, so the mount checks each against its
    /// own `(venue, symbol)` ([`StrategyFileHeader::misrouted_params`]).
    #[default]
    SingleLeg,
    /// The route keys name legs that are not the mount's own; they are not judged.
    MultiLeg,
}

/// One `[params.<key>]` declaration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyFileHeaderParam {
    /// The value kind the file's reader takes.
    pub r#type: ParamType,
    /// The value the reader falls back to, shown to the user. The host never injects it.
    pub default: Option<ParamValue>,
    /// `[low, high]`, inclusive. A present value outside it is refused.
    pub range: Option<[f64; 2]>,
    /// The other keys this one is consumed under (the registry's `PARAM_GATES`), by key, sorted.
    #[serde(default)]
    pub needs: std::collections::BTreeMap<String, StrategyFileNeed>,
    /// What this key names when it names WHERE an order goes (the registry's `PARAM_ROUTES`).
    pub route: Option<RouteKind>,
}

/// One entry of `needs`: the registry's `Gate::Is` / `Gate::Positive`, judged against the RESOLVED rows
/// the file's `echo` reports.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "header::NeedText")]
pub enum StrategyFileNeed {
    /// `other = ["value", …]`: the other key resolved to one of these values.
    OneOf(Vec<String>),
    /// `other = "positive"`: the other key resolved to a number strictly above zero.
    Positive,
}

/// The `[run]` table: today's `vike_tactics::RuleRunOptions`, one switch per field of its name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyFileHeaderRun {
    /// `RuleRunOptions::bound_exits`.
    #[serde(default)]
    pub bound_exits: bool,
    /// `RuleRunOptions::stacked_closes`.
    #[serde(default)]
    pub stacked_closes: bool,
    /// `RuleRunOptions::fills_a_day`; absent: no cap.
    pub fills_a_day: Option<usize>,
    /// `RuleRunOptions::touch_at_market`.
    #[serde(default)]
    pub touch_at_market: bool,
}

/// The value kinds a params key's reader takes (moved from the deleted registry's `ParamType`,
/// whose variants `Str`/`StrOrInteger` are `Text`/`TextOrInteger` here, the header's words). The
/// two methods keep the registry's words byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    /// [`ParamValue::as_f64`]: a float OR an integer.
    Number,
    /// [`ParamValue::as_integer`]: an integer only. A float is refused.
    Integer,
    /// [`ParamValue::as_str`].
    Text,
    /// [`ParamValue::as_bool`].
    Bool,
    /// [`ParamValue::as_table`].
    Table,
    /// A string OR an integer, each with its own meaning (the shipped `grid_dca` files' `side`).
    TextOrInteger,
    /// [`ParamValue::as_array`]: a list (`sport_copy_follower`'s `wallets`). The registry had no
    /// twin: it enumerated no array key.
    Array,
    /// A RUN ID (`<project>/user_data/runs/<id>`) in the profile or mount, and that run's model TEXT
    /// ([`ParamValue::as_str`]) in the file: the host puts the text in place of the id
    /// ([`StrategyFileHeader::with_model_texts`]), since a jailed file reads no file.
    Model,
}

// The header's words, each its serde tag (`file/tests/header.rs`'s `every_param_type_word_is_its_serde_tag`).
vike_utility::word_enum! { impl ParamType {
    Number => "number", Integer => "integer", Text => "text", Bool => "bool", Table => "table",
    TextOrInteger => "text_or_integer", Array => "array", Model => "model",
} }

impl ParamType {
    /// The [`ParamValue::type_str`] names this reader accepts.
    pub const fn accepted(&self) -> &'static [&'static str] {
        match self {
            ParamType::Number => &["float", "integer"],
            ParamType::Integer => &["integer"],
            ParamType::Text => &["string"],
            ParamType::Bool => &["boolean"],
            ParamType::Table => &["table"],
            ParamType::TextOrInteger => &["integer", "string"],
            ParamType::Array => &["array"],
            ParamType::Model => &["string"],
        }
    }

    /// Whether `v` is a value this key's reader will actually take.
    pub fn accepts(&self, v: &ParamValue) -> bool {
        self.accepted().contains(&v.type_str())
    }

    /// How to say the requirement to an operator, in the vocabulary of the TOML they typed.
    pub const fn expected(&self) -> &'static str {
        match self {
            ParamType::Number => "a number (integer or float)",
            ParamType::Integer => "an integer",
            ParamType::Text => "a string",
            ParamType::Bool => "a boolean",
            ParamType::Table => "a table",
            ParamType::TextOrInteger => "a string or an integer",
            ParamType::Array => "an array",
            ParamType::Model => "a run id (a string)",
        }
    }
}

/// What a route key names (moved from the deleted registry's `RouteKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// The instrument the strategy stamps on its orders.
    Symbol,
    /// The venue half of the `(venue, symbol)` pair the strategy keys its state under.
    Venue,
    /// A `SYMBOL = "venue"` routing table, one route per row.
    VenueMap,
}

/// One present key whose value is of a type its reader cannot take, or outside its declared range
/// (moved from the deleted registry's `ParamTypeError`; the words are its `Display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamTypeError {
    /// The `[strategy.params]` key, exactly as the operator spelled it.
    pub key: String,
    /// What the reader takes: [`ParamType::expected`], or `a value in [low, high]`.
    pub expected: String,
    /// What the profile handed it: [`ParamValue::type_str`], or the value itself.
    pub got: String,
}

impl std::fmt::Display for ParamTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` wants {}, got {}", self.key, self.expected, self.got)
    }
}

/// One route key whose value names a market the mount does not have (moved from the deleted
/// registry's `RouteMismatch`; the words are its `Display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMismatch {
    /// The key, or `venues.<SYMBOL>` for one row of a routing table.
    pub key: String,
    /// What that value names, in the operator's own vocabulary.
    pub named: String,
    /// What this mount actually routes to.
    pub mounted: String,
}

impl std::fmt::Display for RouteMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` names {}, but this mount routes {}", self.key, self.named, self.mounted)
    }
}
