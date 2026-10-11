//! **The backtest PROFILE's schema, as DATA** — every TOML key the harness deserialises, derived
//! from the type that declares it.
//!
//! # Why this exists
//!
//! A run profile is the whole run as one TOML file, and it is the surface an operator actually
//! types. Nothing published it: the FLAG surface got this treatment in `vike_cli::surface`, while
//! the profile's own keys were described by hand, in prose, on the far side.
//!
//! MEASURED, not feared. The published page describing this schema says `[walkforward]` "carries
//! one knob, `n_splits >= 1`". `harness::profile::WalkforwardCfg` declares NINE fields,
//! and two of them (`purge`, `embargo`) carry refusals of their own. The same page names the grid
//! table `[sweep]`; the field is `paramscan`, and `sweep` is only an alias. Both sentences were
//! right when they were written. That is the failure class this module ends.
//!
//! # Why it is DERIVED rather than declared
//!
//! `vike_cli::surface` is a hand-maintained table, and that is defensible at its size: the facts
//! it carries — does a flag take a value, which sub-verb accepts it — are CONTROL FLOW, and no
//! parser recovers them from the source. A profile key is the opposite. It is a struct field, and
//! every fact a reader wants is already written down beside it: the type, whether serde requires
//! it, what it defaults to, and what it is for. Eighty-odd of them is too many to copy by hand,
//! and a hand copy is exactly how "one knob" happened.
//!
//! So this module PARSES the sources that own the schema, compiled in with `include_str!`. The
//! parse is TOTAL, and that is the property that makes it safe: **a shape the parser cannot read
//! PANICS**, so a schema change that would silently drop a key fails the gate instead of shipping
//! a short table. The export cannot fall behind the type, because there is no second copy of it.
//!
//! ⚠ **The one hand-declared fact about the shape is that `ROOT_STRUCT` is the root.** Every
//! table path below it is DERIVED by walking fields whose type names another parsed struct — so
//! `FeeCfg` publishes as `engine.fee` because `EngineCfg::fee` is the field that reaches it, not
//! because a row here says so. A parsed struct the walk never reaches is a gate failure.
//!
//! # What it deliberately does NOT carry
//!
//! - **`[risk]`'s keys.** Its type is `vike_model::ProfileRisk`, in another crate; reaching its
//!   source would mean an `include_str!` across a package boundary, which is a path dependency
//!   `crates/vike-ops/tests/release/packaging_gate.rs` would rightly object to. It publishes as a key
//!   whose nested struct is FOREIGN, naming the file that owns it — a DECLARED hole, not a silent
//!   one. `FOREIGN_STRUCTS` is the whole list.
//! - **A roster whose accepted set is a hand-written list inside a message.** A roster is exported
//!   only where the set is DATA in the source: a serde enum's variants, or a `match` whose arms
//!   ARE the roster (`engine.sizer.kind`). `engine.queue_model` names its three values inside its
//!   own refusal text, and re-deriving that list here would be a SECOND copy of the same hand
//!   list — the thing this module exists to avoid. The refusal ships verbatim instead, which is
//!   the authoritative sentence anyway.
//! - **Argument.** Like `vike_cli::surface`, this module is DATA. Rationale belongs in the module
//!   docs of the code it describes and in `crates/vike-backtest/CLAUDE.md`; a row here carries
//!   what a renderer needs plus the symbol it was read from.
//!
//! # Two gates here are about PRODUCTION code, not about the export
//!
//! Parsing the source made two hand-written lists inside it checkable, and both are lists the
//! source's own comments warn will rot: `tests::the_sizer_wrapping_set_agrees_with_the_arms_that_read_base`
//! holds `SizerCfg::build`'s `matches!` equal to the arms that actually read `base`, and
//! `tests::the_unknown_sizer_kind_message_names_every_arm` holds its unknown-kind message equal
//! to the arm roster. Those catch a bug in the engine, not a stale docs page.
//!
//! # Key SETS are the contract, never key ORDER
//!
//! `serde_json`'s map flavour flips with `preserve_order`, which this crate's DataFusion lanes
//! enable through feature unification. Everything order-bearing here is an ARRAY carrying an
//! explicit name field.
//!
//! ⚠ **Line endings are normalised before anything is parsed.** `include_str!` embeds the file as
//! it sits on disk at COMPILE time, and a Windows checkout of this repo has CRLF where the Linux
//! CI runner has LF (measured: 2061 CRs across the 2061-line production half of one source).
//! Without the normalisation the rendered asset would differ BY BOX, and the fixture gate would
//! pass on one and fail on the other.

mod parse;
mod render;
mod walk;

pub use self::render::rendered_files;

/// The file name this module publishes, and the name the far side's asset list must carry.
///
/// ⚠ It reaches that list as a BARE LITERAL, because `vike-ops` — which owns the gate holding the
/// rendered asset set equal to the release workflow's and the mirror's — cannot depend on this
/// crate in the direction that would let it name this constant. The same declared hole
/// `vike_cli::surface`'s `CLI_JSON` already carries.
pub const PROFILE_JSON: &str = "profile.json";

/// The schema version, bumped on any change a consumer could not read.
///
/// ⚠ A DECLARATION, not a gate. The far side's fetch reads ONE asset's version and demotes every
/// asset to its committed snapshot only when a fetch or a parse FAILS, so bumping this protects
/// nobody until the far side reads it. Treat a shape change as breaking regardless of the number.
pub const SCHEMA_VERSION: u32 = 1;

/// The plane this schema belongs to.
pub const PLANE: &str = "backtest";

/// The struct the walk starts from — the ONE hand-declared fact about the shape.
pub const ROOT_STRUCT: &str = "BacktestProfile";

/// Repo-relative path of the source that owns the profile schema.
///
/// ⚠ The ROOT of the `harness::profile` module, which is a SET of files: its tables live in the
/// children under `harness/profile/`. Every struct, roster and refusal is read file by file and
/// carries the path of the file it was read from; this path names the module as a whole.
pub const PROFILE_SRC_PATH: &str = "crates/vike-backtest/src/harness/profile.rs";

/// Repo-relative path of the source that owns `[[data.series]]`.
pub const SERIES_SRC_PATH: &str = "crates/vike-backtest/src/hist_replay.rs";

const SERIES_SRC: &str = include_str!("hist_replay.rs");

/// One harness module whose refusals govern a profile.
#[derive(Debug, Clone, Copy)]
pub struct HarnessModule {
    /// Repo-relative path — the evidence file for every refusal read out of it.
    pub path: &'static str,
    /// What this module decides about a profile.
    pub owns: &'static str,
    src: &'static str,
}

/// Every harness module that raises a refusal, and what each one decides.
///
/// ⚠ **This list exists because the export was SHORT and nothing said so.** The first version
/// parsed `harness/profile.rs` alone and published 62 refusals as "what the profile refuses" —
/// while `harness/windows.rs` was refusing four more about the very same `[walkforward]` table,
/// including the one an operator is most likely to hit (`step` shorter than `test`). A published
/// page then told a reader that nothing refused it. The schema's own file is where the KEYS live;
/// the refusals are spread across the modules that read them.
///
/// `tests::every_harness_module_that_refuses_is_parsed` walks the real source folders it derives
/// from these rows (`src/harness` plus each row's folder and that folder's module root), so a NEW
/// module carrying a refusal fails the gate until it is listed here — the export cannot go short
/// again without saying so.
///
/// ⚠ **One row per FILE, and `harness::profile` is nine of them**: the root and its children under
/// `harness/profile/`. A row's `path` is the evidence file of what is read out of it, so the
/// schema's own module is listed file by file rather than as one concatenated text, and
/// `profile_modules` selects that set from here by path — there is no second list to fall behind.
pub const HARNESS_MODULES: &[HarnessModule] = &[
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile.rs",
        owns: "the schema's root — `BacktestProfile`, `[strategy]` and the `engine.decide` mode",
        src: include_str!("harness/profile.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/walkforward_cfg.rs",
        owns: "the [walkforward] table's keys and their per-key validation",
        src: include_str!("harness/profile/walkforward_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/data_cfg.rs",
        owns: "the [data] table's keys and their per-key validation",
        src: include_str!("harness/profile/data_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/engine_cfg.rs",
        owns: "the [engine] table's keys and their per-key validation",
        src: include_str!("harness/profile/engine_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/fee_cfg.rs",
        owns: "the [engine.fee] table's keys and their per-key validation",
        src: include_str!("harness/profile/fee_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/resolution_cfg.rs",
        owns: "the [engine.resolution] table's keys and their per-key validation",
        src: include_str!("harness/profile/resolution_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/impact_cfg.rs",
        owns: "the [engine.impact] table's keys and their per-key validation",
        src: include_str!("harness/profile/impact_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/sizer_cfg.rs",
        owns: "the [engine.sizer] table's keys and the sizer kind roster",
        src: include_str!("harness/profile/sizer_cfg.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/profile/validate.rs",
        owns: "the cross-table validation of a whole profile, first-error and accumulating",
        src: include_str!("harness/profile/validate.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/walkforward/windows.rs",
        owns: "resolving [walkforward] into a window list: the bar-count relations between train, \
               test, step, purge and embargo",
        src: include_str!("walkforward/windows.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/walkforward/runner.rs",
        owns: "running the walk: what the bar lane and a single series can carry",
        src: include_str!("walkforward/runner.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/sweep.rs",
        owns: "expanding [paramscan] into a grid",
        src: include_str!("harness/sweep.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/optimize.rs",
        owns: "the search over that grid",
        src: include_str!("harness/optimize.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/search/euler.rs",
        owns: "the successive-halving schedule a search may run under",
        src: include_str!("search/euler.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/registry.rs",
        owns: "resolving [strategy].name to something this binary can build",
        src: include_str!("harness/registry.rs"),
    },
    HarnessModule {
        path: "crates/vike-backtest/src/harness/mod.rs",
        owns: "the error type itself — it carries a Display arm, not a refusal of its own",
        src: include_str!("harness/mod.rs"),
    },
];

/// A refusal site whose message is not a literal at the site: it forwards a value the parser cannot
/// resolve from text.
///
/// ⚠ Declared with a COUNT per module, and gated both ways, so "the parser found no message here"
/// can never quietly mean "this module refuses nothing". Each row says what the site forwards.
pub const INDIRECT_REFUSAL_SITES: &[(&str, usize, &str)] = &[
    (
        "crates/vike-backtest/src/harness/mod.rs",
        1,
        "the `Display` impl's own match arm — `Validation(e) => write!(…)` renders a message, it \
         does not raise one",
    ),
    (
        "crates/vike-backtest/src/harness/registry.rs",
        1,
        "`Validation(e.to_string())` forwards a strategy constructor's own error text, which lives \
         in the strategy rather than here",
    ),
    (
        "crates/vike-backtest/src/harness/profile/engine_cfg.rs",
        1,
        "`let bad = |m: String| Validation(m)` — a closure its callers hand a `format!` to, and \
         those calls ARE parsed through the `bad(format!(` marker",
    ),
];

/// One struct a profile field reaches whose source this crate cannot read.
#[derive(Debug, Clone, Copy)]
pub struct ForeignStruct {
    pub strukt: &'static str,
    pub owner: &'static str,
    pub why: &'static str,
}

/// The DECLARED-HOLE list: a struct the walk reaches, owned elsewhere, with the reason.
///
/// A referenced struct that is neither parsed nor named here fails
/// `tests::every_referenced_struct_is_parsed_or_declared_foreign`.
/// ⚠ **EMPTY, and it took a correction to get here.** It carried one row — `ProfileRisk`, the
/// `[risk]` table — on the stated ground that "an `include_str!` across a package boundary is a
/// path dependency the packaging gate refuses". That was WRONG:
/// `crates/vike-ops/tests/release/packaging_gate.rs` is about the `cargo binstall` URL template and
/// mentions `include_str!` nowhere. So eleven keys were missing from the published reference —
/// among them the per-order notional cap a live mount refuses to start without — because of a
/// justification nobody checked, mine included.
///
/// The cure is the shape to reuse: the crate that OWNS the type exposes its own source as data
/// (`vike_model::risk::surface`), and this module reads it through the dependency edge that already
/// exists. A foreign struct belongs here only when its owner cannot be made to do that, and the
/// row has to say why in terms somebody can check.
pub const FOREIGN_STRUCTS: &[ForeignStruct] = &[];

/// A named `#[serde(default = "fn")]` whose one-line body is a PATH rather than a literal,
/// answered by the compiler instead of by the parser.
///
/// ⚠ Five arms, and they exist because the parser reads TEXT: `fn default_ac_gamma() -> f64 {
/// vike_sim::AC_GAMMA }` carries no number to read. Naming the SAME constant the function
/// returns is what keeps the published value from drifting — change the constant and this changes
/// with it. A path-bodied named default NOT answered here fails
/// `tests::every_unreadable_named_default_is_resolved`; a stale arm fails its twin.
///
/// `default_params` is the one arm naming no constant: its body CONSTRUCTS an empty table
/// (`toml::Value::Table(Default::default())`), and an empty TOML table has one spelling.
fn resolved_default(default_fn: &str) -> Option<String> {
    Some(match default_fn {
        "default_window_secs" => vike_model::fair::UPDOWN_WINDOW_SECS.to_string(),
        "default_impact_window" => vike_sim::DEFAULT_IMPACT_WINDOW.to_string(),
        "default_ac_gamma" => vike_sim::AC_GAMMA.to_string(),
        "default_ac_eta" => vike_sim::AC_ETA.to_string(),
        "default_params" => "{}".to_string(),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// The parsed shapes
// ---------------------------------------------------------------------------------------------

/// One field, exactly as the source declares it.
#[derive(Debug, Clone)]
struct Field {
    name: String,
    ty: String,
    /// The first paragraph of the `///` block, link brackets unwrapped; empty when undocumented.
    doc: String,
    /// The contents of each `#[serde(...)]` attribute, comma parts unsplit.
    serde_attrs: Vec<String>,
}

/// One struct, fields in declaration order.
#[derive(Debug, Clone)]
struct Struct {
    name: String,
    source: &'static str,
    /// `#[serde(deny_unknown_fields)]` — whether the table refuses an undeclared key.
    closed: bool,
    fields: Vec<Field>,
}

/// One serde enum whose unit variants ARE a value roster.
#[derive(Debug, Clone)]
struct Enum {
    name: String,
    source: &'static str,
    /// Lowercased under `rename_all = "lowercase"`; otherwise verbatim.
    members: Vec<String>,
    /// The `#[default]` variant's wire spelling, if one is marked.
    default: Option<String>,
}

/// One refusal, as the source raises it.
#[derive(Debug, Clone)]
struct Refusal {
    /// The enclosing `impl` type, when there is one.
    on_type: Option<String>,
    /// The enclosing function — the evidence symbol, never a line number.
    on_fn: String,
    source: &'static str,
    /// The message VERBATIM. A `format!` hole stays as its template, like `vike_cli::surface`.
    message: String,
}

/// One `[engine.sizer]` kind, with the knobs its own arm requires.
#[derive(Debug, Clone)]
struct SizerKind {
    kind: String,
    requires: Vec<String>,
    wraps_base: bool,
}

/// One key of the published schema.
#[derive(Debug, Clone)]
struct Key {
    table: String,
    key: String,
    path: String,
    ty: String,
    /// Serde REQUIRES the key: a profile without it fails to load.
    ///
    /// ⚠ **This doc used to claim the opposite about `Option<T>`** — that an `Option` with no
    /// `#[serde(default)]` is still required, "and calling that optional would be wrong". MEASURED
    /// against serde with a two-field probe struct: an absent `Option<f64>` with no attribute at
    /// all deserialises to `None` and the parse SUCCEEDS, while an absent `f64` is an error. So the
    /// rule is `no default AND not an Option`, and the old rule would have published eleven of
    /// `ProfileRisk`'s keys as required when every one of them is optional. Nothing in
    /// `harness/profile.rs` exercised the difference — every `Option` there carries an explicit
    /// `#[serde(default)]` — which is exactly why the wrong rule survived being written down.
    required: bool,
    /// The declared type is `Option<…>`.
    nullable: bool,
    shape: &'static str,
    default_kind: &'static str,
    default_fn: Option<String>,
    default_value: Option<String>,
    aliases: Vec<String>,
    nested_struct: Option<String>,
    nested_table: Option<String>,
    nested_is_foreign: bool,
    /// The field re-enters a struct already on its own path (`engine.sizer.base`).
    recursive: bool,
    doc: String,
    /// The declaring struct — the evidence symbol.
    strukt: String,
    source: &'static str,
}

/// One table of the published schema.
#[derive(Debug, Clone)]
struct Table {
    table: String,
    strukt: String,
    closed: bool,
    array_of_tables: bool,
    /// The field that reaches it carries no serde default (the root is required by definition).
    required: bool,
    source: &'static str,
}

/// A field serde never reads from TOML, kept visible so the accounting is not silent.
#[derive(Debug, Clone)]
struct Skipped {
    strukt: String,
    field: String,
    doc: String,
}

struct Walked {
    tables: Vec<Table>,
    keys: Vec<Key>,
    skipped: Vec<Skipped>,
}

#[cfg(test)]
use self::parse::{parse_refusals, parse_sizer_kinds, parse_sizer_wrapping_set, production_half};
#[cfg(test)]
use self::render::{all_refusals, all_structs, profile_src, render_profile_json};
#[cfg(test)]
use self::walk::walk;
#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};

#[path = "profile_surface_tests.rs"]
#[cfg(test)]
mod profile_surface_tests;
