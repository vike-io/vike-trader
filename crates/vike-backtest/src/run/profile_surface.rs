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
//! one knob, `n_splits >= 1`". `run::profile::ProfileWalkforward` declares NINE fields,
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
//! it, what it defaults to, and what it is for. A hand copy is exactly how "one knob" happened.
//!
//! So a key comes from the TYPES, behind this crate's off-by-default `profile-schema` feature (this
//! module exists only under it, and every test build of this crate turns it on through the self
//! dev-dependency):
//! - the `schemars` derive on each table's struct says the key, its type, whether it is required,
//!   the value a named default produces and its description;
//! - serde's own field list (`vike_utility::serde_fields::field_groups`) says the declaration ORDER
//!   (the schema's properties are a sorted map, and `serde_json/preserve_order` would flip every map
//!   in the build) and each field's aliases: a name serde reads that the schema does not carry;
//! - what neither derive can say is an attribute beside the field it is about: a default's FUNCTION
//!   name (`#[schemars(extend("x-default-function" = ...))]` on the field) and each `#[serde(skip)]`
//!   field with its reason (`x-skipped-fields` on the struct, which the schema leaves the field out of).
//!
//! Nothing here reads a `.rs` file. The tests do, to hold each hand-written fact to the code it is
//! about (`every_serde_fact_the_derives_cannot_say_is_declared_beside_it`,
//! `every_table_row_names_the_file_declaring_its_struct`).
//!
//! ⚠ **The hand-declared facts about the shape are `ROOT_STRUCT` and [`profile_tables`]**: which struct
//! is the root, and which file declares each table's struct (a type cannot say its file). Every table
//! PATH below the root is DERIVED by walking fields whose type is another table's struct, so
//! `ProfileEngineResolution` publishes as `engine.resolution` because `ProfileEngine::resolution` is the
//! field that reaches it, not because a row here says so. A struct the walk reaches with no row PANICS,
//! and a row the walk never reaches is a test failure.
//!
//! # The refusals are TYPES, not text
//!
//! Every refusal is a variant of `crate::run::refusals::ProfileRefusal` (or of
//! `vike_fills::refusals::CostRefusal`, the cost params'), which `HarnessError::Validation` carries,
//! so the published list is complete by type: `render` walks each id enum's `ALL` and publishes its
//! id and its template. Where each one is raised is the one fact a type cannot say, declared per id
//! in `refusal_sites` by an exhaustive `match`.
//!
//! # What it deliberately does NOT carry
//!
//! - **A roster whose accepted set is a hand-written list inside a message.** A roster is exported
//!   only where the set is a TYPE: a serde enum a key's type names ([`profile_rosters`]), or
//!   `crate::run::profile::SizerKind` (`engine.sizer.kind`, a string the engine matches on).
//!   `engine.queue_model` names its three values inside its own refusal text, and re-deriving that
//!   list here would be a SECOND copy of the same hand list — the thing this module exists to
//!   avoid. The refusal ships verbatim instead, which is the authoritative sentence anyway.
//! - **Argument.** Like `vike_cli::surface`, this module is DATA. Rationale belongs in the module
//!   docs of the code it describes and in `crates/vike-backtest/CLAUDE.md`; a row here carries
//!   what a renderer needs plus the symbol it was read from.
//!
//! # Key SETS are the contract, never key ORDER
//!
//! `serde_json`'s map flavour flips with `preserve_order`, which this crate's DataFusion lanes
//! enabled through feature unification up to datafusion 55.1.0 (nothing in `Cargo.lock` enables
//! it since 55.2.0). Everything order-bearing here is an ARRAY carrying an explicit name field.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::de::value::{Error as ValueError, StrDeserializer};
use serde_json::Value;

mod refusal_sites;
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

/// The struct the walk starts from — the ONE hand-declared fact about the shape's root.
pub const ROOT_STRUCT: &str = "ProfileBacktest";

/// Repo-relative path of the source that owns the profile schema.
///
/// ⚠ The ROOT of the `run::profile` module, which is a SET of files: its tables live in the
/// children under `run/profile/`. Every table carries the path of the file declaring its struct
/// ([`profile_tables`]); this path names the module as a whole.
pub const PROFILE_SOURCE_PATH: &str = "crates/vike-backtest/src/run/profile.rs";

/// Repo-relative path of the source that owns `[[data.series]]`.
pub const SERIES_SOURCE_PATH: &str = "crates/vike-backtest/src/run/tick_replay.rs";

/// A named default whose published value is the source's spelling, which its JSON does not reproduce:
/// `default_liquidation_buffer` returns `0.10`, published as written, and JSON has only `0.1`. A released
/// value does not change wording. `tests::no_default_spelling_is_stale` holds each row to the function
/// a field still names and to a value that really differs.
const DEFAULT_SPELLINGS: &[(&str, &str)] = &[("default_liquidation_buffer", "0.10")];

/// What the derive says of a `vike_model::params::Params` field: a free-form table, whose keys belong
/// to the strategy. A stand-in rather than a derive on `Params`, whose home `vike-model` is inside
/// `vike-core`'s latency closure, for a table this schema publishes no key of.
pub(crate) struct ParamsTable;

impl schemars::JsonSchema for ParamsTable {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Params".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "object" })
    }
}

// ---------------------------------------------------------------------------------------------
// What a type cannot say: which file declares it
// ---------------------------------------------------------------------------------------------

/// One table's struct: the file declaring it, and serde's names for its fields, one group per field
/// (`vike_utility::serde_fields::field_groups`).
struct ProfileTable {
    file: &'static str,
    fields: Vec<Vec<&'static str>>,
}

fn table<T: schemars::JsonSchema + DeserializeOwned>(file: &'static str) -> (String, ProfileTable) {
    let fields = vike_utility::serde_fields::field_groups::<T>();
    (T::schema_name().into_owned(), ProfileTable { file, fields })
}

/// Every struct a profile table IS, by its schema name, with the file that declares it. Two of them
/// are `vike-simulator`'s and one is `vike-data`'s (`[engine.in_bar]`, `[engine.margin_call]`,
/// `[data] file_format`), and `[risk]` is `vike-model`'s.
fn profile_tables() -> BTreeMap<String, ProfileTable> {
    use crate::run::profile::{
        ProfileBacktest, ProfileData, ProfileDataOptions, ProfileEngine, ProfileEngineResolution,
        ProfileEngineSizer, ProfileMount, ProfilePermutation, ProfileStrategy, ProfileWalkforward,
        QuotesConfig,
    };
    use crate::run::tick_replay::{ProfileDataMark, ProfileDataReference, ProfileDataSeries};
    BTreeMap::from([
        table::<ProfileBacktest>(PROFILE_SOURCE_PATH),
        table::<ProfileStrategy>(PROFILE_SOURCE_PATH),
        table::<ProfileData>("crates/vike-backtest/src/run/profile/data_cfg.rs"),
        table::<ProfileDataOptions>("crates/vike-backtest/src/run/profile/data_cfg.rs"),
        table::<ProfileEngine>("crates/vike-backtest/src/run/profile/engine_config.rs"),
        table::<QuotesConfig>("crates/vike-backtest/src/run/profile/engine_config.rs"),
        table::<ProfileMount>("crates/vike-backtest/src/run/profile/mounts_config.rs"),
        table::<ProfileWalkforward>("crates/vike-backtest/src/run/profile/walkforward_config.rs"),
        table::<ProfilePermutation>("crates/vike-backtest/src/run/profile/permutation_config.rs"),
        table::<ProfileEngineResolution>(
            "crates/vike-backtest/src/run/profile/resolution_config.rs",
        ),
        table::<ProfileEngineSizer>("crates/vike-backtest/src/run/profile/sizer_config.rs"),
        table::<ProfileDataSeries>(SERIES_SOURCE_PATH),
        table::<ProfileDataReference>(SERIES_SOURCE_PATH),
        table::<ProfileDataMark>(SERIES_SOURCE_PATH),
        table::<vike_model::ProfileRisk>("crates/vike-model/src/risk/profile.rs"),
        table::<vike_data::csv_bars::CsvBarsFormatWords>(
            "crates/vike-data/src/csv_bars/format_words.rs",
        ),
        table::<vike_simulator::ShortfallCover>(
            "crates/vike-simulator/src/engine/sim_broker/knobs.rs",
        ),
        table::<vike_simulator::InBarRules>(
            "crates/vike-simulator/src/engine/sim_broker/in_bar.rs",
        ),
    ])
}

/// One value roster: a serde enum a profile key's type names, its words as `schemars` states them.
struct Roster {
    name: String,
    file: &'static str,
    members: Vec<String>,
    /// The word of the enum's `Default` member, for an enum that has one.
    default: Option<String>,
}

fn roster<T: schemars::JsonSchema + DeserializeOwned + PartialEq>(
    file: &'static str,
    default: Option<T>,
) -> Roster {
    let members = enum_words(&schemars::schema_for!(T).to_value());
    // The default's word is the member that reads back as it.
    let default = default.and_then(|default| {
        members
            .iter()
            .find(|word| {
                T::deserialize(StrDeserializer::<ValueError>::new(word.as_str()))
                    .is_ok_and(|member| member == default)
            })
            .cloned()
    });
    Roster { name: T::schema_name().into_owned(), file, members, default }
}

/// Every serde enum a profile key's type names, with the file that declares it. A key whose type is
/// an enum with no row here fails the render (`render`'s roster binding).
fn profile_rosters() -> Vec<Roster> {
    use crate::run::tick_replay::SeriesKind;
    use vike_model::run_request::replay::ReplayKind;
    vec![
        roster::<ReplayKind>("crates/vike-model/src/run_request/replay.rs", None),
        roster(SERIES_SOURCE_PATH, Some(SeriesKind::default())),
    ]
}

/// A unit-variant enum's words, in declaration order, as `schemars` states them: a plain `enum` list,
/// or, when its variants carry docs, one `const` per variant under `oneOf`. ⚠ TOTAL: any other shape
/// (a variant with data, or documented and undocumented variants mixed, which `schemars` reorders)
/// panics rather than publishing a roster in the wrong order.
fn enum_words(schema: &Value) -> Vec<String> {
    let words: Option<Vec<String>> = match (schema["enum"].as_array(), schema["oneOf"].as_array()) {
        (Some(words), None) => words.iter().map(|w| w.as_str().map(str::to_string)).collect(),
        (None, Some(variants)) => {
            variants.iter().map(|v| v["const"].as_str().map(str::to_string)).collect()
        }
        _ => None,
    };
    words.unwrap_or_else(|| {
        panic!("profile_surface: an enum schema this export does not model as a roster: {schema}")
    })
}

// ---------------------------------------------------------------------------------------------
// The walked shapes
// ---------------------------------------------------------------------------------------------

/// One key of the published schema.
#[derive(Debug, Clone)]
struct Key {
    table: String,
    key: String,
    path: String,
    ty: String,
    /// Serde REQUIRES the key: a profile without it fails to load. The schema's `required` list.
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
use self::render::{all_refusals, profile_schema, render_profile_json};
#[cfg(test)]
use self::walk::walk;
#[cfg(test)]
use std::collections::BTreeSet;

#[path = "profile_surface_tests.rs"]
#[cfg(test)]
mod profile_surface_tests;
