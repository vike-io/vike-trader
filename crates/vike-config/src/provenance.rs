//! **Which layer set each typed setting** — the file half of `vike-cli config show`.
//!
//! [`crate::load`] answers *what* the settings resolved to. It cannot answer the question an
//! operator actually asks, which is *did my `policy.toml` get read, and did this key take effect?*
//! Twelve PRs consolidated every setting into `<project>/settings/*.toml` and nothing disclosed the
//! result: `config show` printed the environment-variable registry and **zero** TOML rows, so
//! `max_notional_per_order` — the one ceiling with no env override BY DESIGN, and therefore the one
//! whose only proof of effect is a file being read — did not appear anywhere. A typo'd filename or
//! a settings directory resolved from the wrong project produced a silently uncapped node.
//!
//! # Provenance is measured, never inferred
//!
//! An [`Origin`] is decided by **key PRESENCE in a parsed layer**, not by diffing effective values
//! against defaults. The difference matters: a `policy.toml` that sets `max_leverage = 1.0` — the
//! same number as the code default — is a file that WAS read and DID configure that key, and a
//! value-diff would report `default` and tell the operator their file did nothing. Presence is a
//! fact about a parsed table; "the value equals the default" is a coincidence.
//!
//! The layers are examined highest-precedence first, exactly mirroring [`crate::load_with_cli`]'s
//! own order: env → the settings file → the code default. There is deliberately no CLI arm —
//! `vike-cli`'s dispatcher resolves settings through [`crate::load`], so a CLI layer would be a
//! layer this description's caller never applied.
//!
//! **[`Policy`](crate::Policy) rows can only ever report `default` or `policy.toml`.** That is not a
//! limitation of this module, it is the taxonomy showing through: `Policy` implements neither
//! `EnvOverride` nor `CliOverride`, both sealed, so there is no other layer that could have set one.
//!
//! # The key table, part declared and part derived
//!
//! [`setting_keys`] has three halves — the flat policy / config / preferences rows, written out;
//! one derived row per [`crate::FLAG_REGISTRY`] entry; and one derived row per
//! [`vike_model::VENUES`] id for the `policy.venues.*` ceilings. No count is written here, because
//! the one that used to be stopped matching the table the first time a field was added.
//!
//! Both derived halves are derived for the same reason: their rosters are ALREADY gated
//! elsewhere — `crates/vike-config/tests/flag_registry.rs` holds `FLAG_REGISTRY` field-for-field
//! against [`crate::Flags`], and `vike_model::VENUES` is derived from the `crates/bridges/*` tree
//! by its own test — so re-listing either here would add a second copy for a gate to keep honest,
//! and the second copy is what rots. The declared half is gated too, by
//! `crates/vike-config/tests/provenance.rs`'s `every_settings_field_has_a_provenance_row`, which
//! walks the real serialized [`crate::Settings`] rather than any list written by hand.
//!
//! # Values come from the types, not from a hand-written getter
//!
//! Each row's effective value is looked up by PATH in `toml::Value::try_from(&settings.<section>)`,
//! and its default by the same path in the same serialization of `<Section>::default()`. So there
//! is no per-field accessor to forget to update, and an `Option::None` field is simply absent from
//! the serialized table — which is exactly "unset", the thing it means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{
    BACKTEST_ADDR_ENV, DATAHUB_ADDR_ENV, DATAHUB_ADVERTISE_ADDR_ENV, INSTANCE_ORIGIN_ENV,
    JOURNAL_DIR_ENV, LOG_DIR_ENV, STATE_DIR_ENV, STORE_ROOT_ENV, TRADEHUB_ADDR_ENV,
};
use crate::error::ConfigError;
use crate::flags::{FLAG_REGISTRY, POLY_REDEEM_HALT_ENV};
use crate::load::{CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE, Settings, load};
use crate::preferences::{
    CHART_STYLE_ENV, LOG_FILE_LEVEL_ENV, LOG_LEVEL_ENV, RUST_LOG_ENV, SWEEP_THREADS_ENV,
};

/// The layer that set an effective value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Nothing set it — the compiled-in default stands.
    Default,
    /// A settings-directory file, by NAME (`policy.toml`, `config.toml`, …).
    File(&'static str),
    /// An environment variable, by NAME — the one that actually matched, which for
    /// `preferences.log_level` distinguishes `RUST_LOG` from its `VIKE_LOG` alias.
    Env(&'static str),
}

impl Origin {
    /// The stable machine word: `default` / `file` / `env`. Pinned by test — a tool reading
    /// `--json`'s `origin` field must keep reading the same three.
    ///
    /// ⚠ **A fourth word, `project`, is RETIRED.** It was produced only by the per-project
    /// `<project>/vike.toml` layer, and that layer is gone — a present file is now refused at load
    /// (see [`crate::removed`]), so no row can carry it under any configuration. The three
    /// surviving words keep their exact meanings and their exact spellings, so the `--json` domain
    /// NARROWED rather than changing: a consumer that matched on `"project"` is left with a branch
    /// that can no longer be taken, which is dead code, not a break. A consumer that matched on
    /// `"file"`, `"env"` or `"default"` sees no difference at all. Narrowing is the only
    /// compatible way to remove a layer; renaming one of the three would not have been.
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::File(_) => "file",
            Origin::Env(_) => "env",
        }
    }

    /// The layer's own name: the file name, the variable name, or `""` for a default.
    pub fn detail(&self) -> &'static str {
        match self {
            Origin::Default => "",
            Origin::File(f) => f,
            Origin::Env(v) => v,
        }
    }

    /// One human cell: `default`, `policy.toml`, or `env:VIKE_RECONCILE`.
    pub fn label(&self) -> String {
        match self {
            Origin::Default => "default".to_string(),
            Origin::File(f) => (*f).to_string(),
            Origin::Env(v) => format!("env:{v}"),
        }
    }
}

/// One advertised layer: the machine word [`Origin::kind`] returns for it, and how the printed
/// precedence header names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    /// The stable machine word — exactly what [`Origin::kind`] returns for this layer.
    pub kind: &'static str,
    /// How [`precedence_line`] names it to an operator.
    pub label: &'static str,
}

/// **The layers this crate advertises, HIGHEST precedence first** — the order [`describe`] tries
/// them, which is [`crate::load_with_cli`]'s own order reversed.
///
/// This is the list `vike-cli config show` renders its precedence header from ([`precedence_line`]),
/// so the header cannot name a layer that is not here. Two gates keep the rest honest:
/// `crates/vike-config/tests/layers_are_reachable.rs` pins it against [`Origin`]'s variants and
/// against the order [`describe`] really resolves in, and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` proves every entry actually takes effect
/// through the shipped binary.
///
/// That second gate is the point. An advertised layer that no composition root reaches gives the
/// operator positive confirmation of something false — the same defect [`crate::consumed`] gates one
/// level down, where it is a KEY nothing reads instead of a LAYER nothing applies. A `project` row
/// naming `<project>/vike.toml` stood here and was exactly that: fully implemented, fully tested,
/// printed in this header, and passed `None` by every binary in the workspace. Wiring it made the
/// header true for an hour; REMOVING the layer made it true for good, and the row went with it —
/// the header is derived, so it could not be left behind.
///
/// There is deliberately no CLI row: [`describe`] takes no [`crate::CliOverrides`], so a CLI layer
/// here would be one this description's caller never applied.
pub const PRECEDENCE: [Layer; 3] = [
    Layer { kind: "env", label: "env" },
    Layer { kind: "file", label: "the file above" },
    Layer { kind: "default", label: "default" },
];

/// The precedence header `vike-cli config show` prints, rendered from [`PRECEDENCE`] itself.
///
/// Derived rather than written out because the header being FALSE is the defect this whole pair of
/// gates exists to remove: a hand-typed line naming a layer nobody reads is how the per-project
/// file was advertised for two months, and a hand-typed line is also what would still be naming it
/// today, one change after the layer was deleted. A layer added to [`PRECEDENCE`] shows up here
/// automatically and the reachability gates then refuse to go green until a binary really applies
/// it; a layer removed from it disappears from the header in the same commit, with nothing to
/// remember.
pub fn precedence_line() -> String {
    let layers: Vec<&str> = PRECEDENCE.iter().map(|l| l.label).collect();
    format!("precedence: {}   (policy: FILE ONLY)", layers.join(" > "))
}

/// One key this crate can resolve: where it lives in a file, and which variables can override it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingKey {
    /// The dotted key as an operator names it — `policy.max_notional_per_order`. Its first segment
    /// is the [`Settings`] section, which is also how the value is looked up.
    pub key: String,
    /// The settings-directory file this key belongs to.
    pub file: &'static str,
    /// The key's path INSIDE that file — no section prefix (`policy.toml` holds `max_leverage`,
    /// not `policy.max_leverage`).
    pub path: Vec<&'static str>,
    /// Environment variables that can override it, in the order [`crate::layers`] tries them.
    /// Empty for a key with no env layer at all — every [`crate::Policy`] key.
    pub env: Vec<&'static str>,
    /// `true` for the one variable whose mere PRESENCE arms it, whatever the value
    /// ([`POLY_REDEEM_HALT_ENV`] — a kill switch no value may un-set).
    pub env_presence_only: bool,
}

/// One resolved setting, ready to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSetting {
    /// The dotted key.
    pub key: String,
    /// The settings file this key belongs to.
    pub file: &'static str,
    /// The EFFECTIVE value, rendered. `None` means unset — an `Option` field nothing filled in.
    pub value: Option<String>,
    /// The compiled-in default, rendered, or `None` when the default is "unset".
    pub default: Option<String>,
    /// The layer that set it.
    pub origin: Origin,
    /// What that layer LITERALLY holds, rendered — `None` for [`Origin::Default`].
    pub origin_value: Option<String>,
    /// `true` when [`Self::value`] is not what [`Self::origin_value`] says, i.e. a later rule moved
    /// it. ⚠ NO such rule exists today: the only one was the policy-clamps-preference edge in
    /// [`crate::load`], whose single instance (`rate.max_utilization` over `rate_utilization`)
    /// bounded a value nothing read and is gone. The field stays because the CONCEPT is the
    /// taxonomy's spine and the next real bound/value pair will set it again; reporting it is how
    /// an operator learns their file was not taken literally.
    ///
    /// Numeric comparison, not string comparison: `max_leverage = 3` in a file is a TOML integer
    /// while the effective value is the float `3.0`, and reporting that as an adjustment would be a
    /// formatting artifact reported as a fact.
    pub adjusted: bool,
}

/// One settings file, and whether it was actually there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    /// The file NAME (`policy.toml`).
    pub name: &'static str,
    /// Its full path — the answer to "which project am I reading?".
    pub path: PathBuf,
    /// Whether it exists AND parsed. A file that exists but is broken never reaches here:
    /// [`describe`] runs [`load`] first, which fails naming the file and the key.
    pub present: bool,
    /// How many keys it sets (leaf keys, so `rate.max_utilization` counts as one).
    pub keys: usize,
}

/// Everything `config show`'s file half needs, from one pass.
#[derive(Debug, Clone)]
pub struct Description {
    /// THE settings directory, or `None` when no project sits above the caller's working
    /// directory — in which case every row below reports `default` and no file was read at all.
    pub settings_dir: Option<PathBuf>,
    /// The four settings files, in application order.
    ///
    /// ⚠ Exactly the files the loader consults, and no others — the invariant, not an accident of
    /// which rows somebody remembered to push. A per-project `<project>/vike.toml` row appeared
    /// here for one change and is gone with the layer: listing a file the loader ignores would
    /// invert the very defect this list exists for, telling an operator a file is part of the
    /// answer when it is not. A present one does not get a row saying `absent` either — it gets a
    /// startup refusal ([`crate::removed`]), which is louder than any row.
    pub files: Vec<FileStatus>,
    /// The fully-resolved settings, including [`Settings::warnings`] — the caller SURFACES those
    /// (this crate deliberately does not log; see [`Settings::warnings`]).
    pub settings: Settings,
    /// One row per key, sorted by key.
    pub rows: Vec<ResolvedSetting>,
}

/// Every key this crate resolves: the declared policy/config/preferences rows, then one per
/// registered flag.
///
/// Built rather than `const` because the flags half is derived from [`FLAG_REGISTRY`] and a
/// derived key is an owned `String`. The cost is one allocation per key on a CLI path that prints
/// them all anyway.
pub fn setting_keys() -> Vec<SettingKey> {
    fn row(
        key: &str,
        file: &'static str,
        path: &[&'static str],
        env: &[&'static str],
    ) -> SettingKey {
        SettingKey {
            key: key.to_string(),
            file,
            path: path.to_vec(),
            env: env.to_vec(),
            env_presence_only: false,
        }
    }

    let mut out = vec![
        // -- policy: file only, by construction (no `EnvOverride`, no `CliOverride`, both sealed).
        row("policy.max_leverage", POLICY_FILE, &["max_leverage"], &[]),
        row("policy.max_notional_per_order", POLICY_FILE, &["max_notional_per_order"], &[]),
        // The ACCOUNT-aggregate exposure ceiling. Same empty `env` slice as its per-order sibling
        // and for the same reason — it is the whole point of the field that no exported variable
        // can widen it — and the same `null`-when-absent leaf shape, so `config show` reports
        // `default` (i.e. UNCAPPED) on a box that never wrote the key. That reading is the one an
        // operator most needs from this row: an aggregate ceiling nobody set is the state this
        // axis was added because deployments were silently in.
        row("policy.max_account_exposure", POLICY_FILE, &["max_account_exposure"], &[]),
        // The ceiling on the equity FIGURE the sizing and admission lanes may see — the cap that
        // decouples this deployment's position sizes from a wallet a third party can move. Same
        // empty `env` slice as its two siblings above, and here the emptiness is the whole design:
        // the axis exists because a number nobody on this box wrote was reaching a live risk
        // decision, and a ceiling an exported variable could raise would be the same defect wearing
        // a config key. Same `null`-when-absent leaf shape, so `config show` reports `default`
        // (i.e. UNCAPPED — the venue's own figure) on a box that never wrote the key.
        row("policy.max_sizing_equity", POLICY_FILE, &["max_sizing_equity"], &[]),
        row("policy.market_slippage", POLICY_FILE, &["market_slippage"], &[]),
        // ⚠ The empty `env` slice is the POINT for this one, not boilerplate: it governs what a
        // KILL SWITCH lets out, so `config show` must be able to say "file or default, nothing
        // else" — see `crate::Policy::halt_admit`.
        row("policy.halt_admit", POLICY_FILE, &["halt_admit"], &[]),
        // The dead-man switch. Same empty `env` slice, and it matters in BOTH directions here:
        // nothing in the environment can arm a silence-detector on an FX box (a Friday halt with no
        // diff to review) and nothing can disarm one an operator relies on — see `crate::Policy::
        // deadman_timeout_ms`. Both rows report `default` on a box with no policy.toml, and for
        // the timeout that default is ABSENCE (`null`, the `max_notional_per_order` shape): the
        // switch is OFF, and `vike-tradehub`'s live mount warns once that it is. (This comment
        // said "that default is sixty seconds, not absence" for one morning; the field's doc
        // records the reversal.)
        row("policy.deadman_timeout_ms", POLICY_FILE, &["deadman_timeout_ms"], &[]),
        row("policy.deadman_action", POLICY_FILE, &["deadman_action"], &[]),
        // The LINK dead-man's grace (M13). Same empty `env` slice and the same both-directions
        // argument — and one difference an operator reading `config show` has to be able to see:
        // this key's `default` is ARMED, not absent. The serialized leaf is `null` when the file
        // says nothing, exactly like the timeout above, so the ORIGIN cell reads `default` in both
        // cases while the two defaults mean opposite things; `crate::Policy::link_deadman_grace_ms`
        // and its `link_deadman_grace_ms_effective` resolver are where that asymmetry is argued.
        row("policy.link_deadman_grace_ms", POLICY_FILE, &["link_deadman_grace_ms"], &[]),
        // ⚠ `policy.rate.max_utilization` stood here — this table's FIRST nested key, and a
        // ceiling on a preference nothing read. A REMOVED key must not keep a row: a row is
        // precisely what makes `config show` attribute a value to a file and call it effective.
        // See `crate::policy`'s `PolicyPatch::rate`. (`policy.venues.*`, derived below, is the
        // nested shape that replaced it — and the reason `lookup`'s multi-segment arm and
        // `leaf_count`'s recursive arm have an in-model exercise again.)
        // -- config: full chain.
        row("config.store_root", CONFIG_FILE, &["store_root"], &[STORE_ROOT_ENV]),
        row("config.log_dir", CONFIG_FILE, &["log_dir"], &[LOG_DIR_ENV]),
        row("config.journal_dir", CONFIG_FILE, &["journal_dir"], &[JOURNAL_DIR_ENV]),
        row("config.state_dir", CONFIG_FILE, &["state_dir"], &[STATE_DIR_ENV]),
        // ⚠ The env slice here is NOT empty, and that is the whole of what one merge decided. The
        // client half of this key (ruling 16's `study`) was written against a tree where
        // `VIKE_BACKTEST_ADDR` did not exist yet, so it carried a SECOND `config.backtest_addr`
        // row with an empty slice and a comment saying "when the variable lands, this slice
        // grows". The variable landed on the daemon half's branch. Two rows for one key print the
        // key TWICE in `vike-cli config show` — once claiming an env layer and once denying it —
        // and every gate over this table compares SETS, so nothing would have said so. One row,
        // the slice grown as that comment asked. It also keeps `config.node_addr` below the ONLY
        // `config` key with an empty slice, which is what its own comment claims.
        row("config.backtest_addr", CONFIG_FILE, &["backtest_addr"], &[BACKTEST_ADDR_ENV]),
        row("config.datahub_addr", CONFIG_FILE, &["datahub_addr"], &[DATAHUB_ADDR_ENV]),
        row("config.tradehub_addr", CONFIG_FILE, &["tradehub_addr"], &[TRADEHUB_ADDR_ENV]),
        row(
            "config.datahub_advertise_addr",
            CONFIG_FILE,
            &["datahub_advertise_addr"],
            &[DATAHUB_ADVERTISE_ADDR_ENV],
        ),
        // The CLIENT-side dial address `vike-cli backend connect` writes — the one `config` key with
        // an EMPTY env slice, and `crate::config::Config::node_addr` argues why: `--node` is
        // already its per-invocation override, so an environment layer would be a third spelling of
        // one answer. The empty slice is the same shape every `policy` row carries, and it reaches
        // `config show` as a row whose ENV column is blank rather than as a missing row.
        row("config.node_addr", CONFIG_FILE, &["node_addr"], &[]),
        row("config.instance_origin", CONFIG_FILE, &["instance_origin"], &[INSTANCE_ORIGIN_ENV]),
        // -- preferences: full chain. (`rate_utilization` was here, the one key in this whole table
        //    with no environment variable; it is a tombstone now — see `crate::preferences`.)
        row(
            "preferences.log_level",
            PREFERENCES_FILE,
            &["log_level"],
            &[RUST_LOG_ENV, LOG_LEVEL_ENV],
        ),
        row(
            "preferences.log_file_level",
            PREFERENCES_FILE,
            &["log_file_level"],
            &[LOG_FILE_LEVEL_ENV],
        ),
        row("preferences.chart_style", PREFERENCES_FILE, &["chart_style"], &[CHART_STYLE_ENV]),
        row(
            "preferences.sweep_threads",
            PREFERENCES_FILE,
            &["sweep_threads"],
            &[SWEEP_THREADS_ENV],
        ),
    ];

    // -- policy.venues: derived, one row per ROSTER venue. `policy.toml`'s `[venues]` table keys
    //    ARE the venue ids, so the in-file path is `venues.<id>` and the dotted key adds the
    //    section like every other row. Empty `env`, like every policy row and for the same
    //    structural reason.
    //
    //    ⚠ ONE ROW PER VENUE, not one row for the table. The alternative — a single
    //    `policy.venues` row with path `["venues"]` — would render the whole table as one cell,
    //    report ONE origin for fourteen independently-settable keys, and (because
    //    `crates/vike-config/tests/provenance.rs`'s completeness gate compares against the real
    //    serialized leaves) leave fourteen leaves with no row at all.
    out.extend(vike_model::VENUES.iter().copied().map(|venue| SettingKey {
        key: format!("policy.venues.{venue}"),
        file: POLICY_FILE,
        path: vec!["venues", venue],
        env: Vec::new(),
        env_presence_only: false,
    }));

    // -- policy.accounts: the `[accounts]` table — per-ACCOUNT arming ceilings, `{venue: {LABEL:
    //    mode}}`. Empty `env`, like every policy row and for the same structural reason.
    //
    //    ⚠ ONE row for the whole table, and that is the opposite of the `policy.venues` decision
    //    directly above — because the two tables have opposite key sets. A venue key comes from
    //    `vike_model::VENUES`, a compile-time roster this function can enumerate; an ACCOUNT label
    //    is a name the operator chose, discovered at runtime from the credential store
    //    (`vike_model::account_keys::accounts_in_store`), so there is no set of per-account rows to
    //    derive. Rendering the table as one cell is therefore not a shortcut but the only honest
    //    option: it reports one origin for one table, which is what `policy.toml` states.
    //
    //    The leaf it matches is `Policy`'s hand-written `Serialize` projection of
    //    `VenuePolicy::accounts_by_venue`. It is EMPTY on every box that has not written the table,
    //    which is why `crates/vike-config/tests/provenance.rs`'s walk had to start treating an
    //    empty object as a leaf of its own — see that test's `walk`.
    out.push(SettingKey {
        key: "policy.accounts".to_string(),
        file: POLICY_FILE,
        path: vec!["accounts"],
        env: Vec::new(),
        env_presence_only: false,
    });

    // -- flags: derived, one row per registry entry. `flags.toml` keys ARE the field names.
    out.extend(FLAG_REGISTRY.iter().map(|meta| SettingKey {
        key: format!("flags.{}", meta.field),
        file: FLAGS_FILE,
        path: vec![meta.field],
        env: vec![meta.env],
        env_presence_only: meta.env == POLY_REDEEM_HALT_ENV,
    }));

    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Resolve the settings AND describe where every key's value came from.
///
/// Takes the same two parameters as [`load`], and calls it — so a description can never disagree
/// with the settings the caller is actually running on, and so a disclosure command refuses
/// whatever a daemon would refuse (a present `<project>/vike.toml` included). The files are then
/// re-read as raw tables for the PRESENCE half; by that point they are known to parse, because
/// `load` returned `Ok`.
pub fn describe(
    settings_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<Description, ConfigError> {
    let settings = load(settings_dir, env)?;

    // The raw tables, one per layer. A `None` directory (no project above the CWD) means every
    // table is empty and every row below reports `default` — which IS the answer, and the loudest
    // possible one when the operator expected their file to be read.
    let mut files = Vec::new();
    let mut tables: HashMap<&'static str, toml::Table> = HashMap::new();
    for name in [POLICY_FILE, CONFIG_FILE, PREFERENCES_FILE, FLAGS_FILE] {
        let path = settings_dir.map(|d| d.join(name)).unwrap_or_else(|| PathBuf::from(name));
        let table = settings_dir.and_then(|d| read_table(&d.join(name)));
        files.push(FileStatus {
            name,
            path,
            present: table.is_some(),
            keys: table.as_ref().map(leaf_count).unwrap_or(0),
        });
        tables.insert(name, table.unwrap_or_default());
    }

    // The value side: the effective settings and the code defaults, serialized once each.
    let effective = sections(&settings);
    let defaults = sections(&Settings::default());

    let mut rows = Vec::new();
    for spec in setting_keys() {
        let section = spec.key.split('.').next().unwrap_or_default();
        let value = lookup(effective.get(section), &spec.path);
        let default = lookup(defaults.get(section), &spec.path);

        // Precedence, highest first — the mirror of `load_with_cli`'s own order.
        let (origin, origin_value) = 'origin: {
            for &var in &spec.env {
                if let Some(raw) = env_value(env, var, spec.env_presence_only) {
                    break 'origin (Origin::Env(var), Some(raw));
                }
            }
            if let Some(v) = lookup(tables.get(spec.file), &spec.path) {
                break 'origin (Origin::File(spec.file), Some(v));
            }
            (Origin::Default, None)
        };

        let adjusted = matches!(origin, Origin::File(_))
            && !same_value(origin_value.as_deref(), value.as_deref());

        rows.push(ResolvedSetting {
            key: spec.key,
            file: spec.file,
            value,
            default,
            origin,
            origin_value,
            adjusted,
        });
    }

    Ok(Description { settings_dir: settings_dir.map(Path::to_path_buf), files, settings, rows })
}

/// The four [`Settings`] sections, serialized to TOML tables — the ONE place a value is read from,
/// so no per-field accessor exists to forget.
///
/// An `Option::None` field is ABSENT from the table it serializes into (toml has no null), which is
/// precisely "unset" and is what [`lookup`] returns `None` for. A section that somehow fails to
/// serialize contributes an empty table rather than an error: this is a disclosure command, and a
/// row that reports "unset" is a far better failure than a command that refuses to print anything.
fn sections(settings: &Settings) -> HashMap<&'static str, toml::Table> {
    fn table<T: serde::Serialize>(v: &T) -> toml::Table {
        toml::Table::try_from(v).unwrap_or_default()
    }
    HashMap::from([
        ("policy", table(&settings.policy)),
        ("config", table(&settings.config)),
        ("preferences", table(&settings.preferences)),
        ("flags", table(&settings.flags)),
    ])
}

/// Read one TOML file as a raw table. `None` for absent OR unreadable OR unparseable — [`describe`]
/// runs [`load`] first, so by the time this is called the only reachable `None` is "absent".
fn read_table(file: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(file).ok()?.parse::<toml::Table>().ok()
}

/// Follow a path into a table and render the leaf. `None` when any segment is missing — the
/// unset/absent answer.
fn lookup(table: Option<&toml::Table>, path: &[&str]) -> Option<String> {
    let mut value = table?.get(*path.first()?)?;
    for segment in &path[1..] {
        value = value.as_table()?.get(*segment)?;
    }
    Some(render(value))
}

/// Render a TOML leaf the way a human wrote it: strings bare (never quoted), everything else in its
/// TOML spelling.
fn render(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// How many leaf keys a table sets — a nested `a.b` is one, not two.
///
/// ⚠ No setting in the model is nested today (`policy.rate.max_utilization`, the only one there
/// ever was, is a removed tombstone), so the recursive arm is reachable only from a file whose
/// shape the loader would already have rejected. It is KEPT rather than flattened because the count
/// it produces is what `config show` prints per file, and a future nested table must not silently
/// count as one key; `nested_tables_still_count_and_resolve_by_leaf` holds both arms honest.
fn leaf_count(table: &toml::Table) -> usize {
    table
        .values()
        .map(|v| match v.as_table() {
            Some(t) => leaf_count(t),
            None => 1,
        })
        .sum()
}

/// A variable's value under [`crate::layers`]' own rules: empty is UNSET, except for the one
/// presence-armed kill switch, where any value (including empty) counts.
fn env_value(env: &HashMap<String, String>, var: &str, presence_only: bool) -> Option<String> {
    let raw = env.get(var)?;
    if presence_only || !raw.is_empty() { Some(raw.clone()) } else { None }
}

/// Whether a layer's literal value and the effective value are the SAME value — numerically when
/// both parse as numbers, textually otherwise. `1` and `1.0` are the same ceiling written two ways,
/// and reporting that as an adjustment would be a formatting artifact reported as a fact.
fn same_value(layer: Option<&str>, effective: Option<&str>) -> bool {
    match (layer, effective) {
        (Some(a), Some(b)) => match (a.parse::<f64>(), b.parse::<f64>()) {
            (Ok(x), Ok(y)) => x == y,
            _ => a == b,
        },
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE fixtures here use INVENTED names with no known prefix (`ACME_*`). The settings-registry
    // gate (`crates/vike-ops/tests/settings_registry.rs`) harvests every env-shaped string literal
    // in a `src/` file and demands a `SETTINGS` row for it, so a realistic `VIKE_*` fixture fails
    // that gate for a variable this crate does not read. Doc comments are stripped before the
    // sweep, so prose is safe; string literals are not.
    #[test]
    fn the_origin_words_are_pinned() {
        assert_eq!(Origin::Default.kind(), "default");
        assert_eq!(Origin::File(POLICY_FILE).kind(), "file");
        assert_eq!(Origin::Env("X").kind(), "env");

        assert_eq!(Origin::File(POLICY_FILE).detail(), "policy.toml");
        assert_eq!(Origin::Env("ACME_X").label(), "env:ACME_X");
        assert_eq!(Origin::Default.detail(), "");

        // THREE words, and the retired fourth (`project`) must never come back as a spelling
        // `--json` emits: the layer it named is refused at load, so a row carrying it would be a
        // claim about a file nothing reads. `PRECEDENCE` is the same set, gated in
        // `crates/vike-config/tests/layers_are_reachable.rs`.
        let words: Vec<&str> = [Origin::Default, Origin::File(POLICY_FILE), Origin::Env("X")]
            .iter()
            .map(Origin::kind)
            .collect();
        assert_eq!(words, ["default", "file", "env"]);
    }

    /// `1` and `1.0` are the same ceiling; `info` and `debug` are not.
    #[test]
    fn the_same_value_test_is_numeric_when_it_can_be() {
        assert!(same_value(Some("1"), Some("1.0")));
        assert!(same_value(Some("0.50"), Some("0.5")));
        assert!(same_value(Some("info"), Some("info")));
        assert!(!same_value(Some("0.9"), Some("0.5")));
        assert!(!same_value(Some("info"), Some("debug")));
        assert!(!same_value(Some("x"), None));
        assert!(same_value(None, None));
    }

    #[test]
    fn a_nested_key_counts_as_one_leaf() {
        let t: toml::Table = "a = 1\n[b]\nc = 2\nd = 3\n".parse().unwrap();
        assert_eq!(leaf_count(&t), 3);
        assert_eq!(leaf_count(&toml::Table::new()), 0);
    }

    /// The `layers::get` rule this module mirrors: an exported `NAME=` configures nothing — EXCEPT
    /// for the presence-armed kill switch, which no value may un-set.
    #[test]
    fn an_empty_variable_is_unset_unless_it_arms_by_presence() {
        let env = HashMap::from([("A".to_string(), String::new())]);
        assert_eq!(env_value(&env, "A", false), None);
        assert_eq!(env_value(&env, "A", true), Some(String::new()));
        assert_eq!(env_value(&env, "MISSING", true), None);
    }

    /// An `Option::None` field must SERIALIZE AWAY rather than error — the whole value side rests
    /// on it, and `toml` has no null to represent it with.
    #[test]
    fn an_unset_optional_field_is_absent_from_the_serialized_section() {
        let sections = sections(&Settings::default());
        let policy = &sections["policy"];
        assert!(policy.contains_key("max_leverage"), "a set field is present: {policy:?}");
        assert!(
            !policy.contains_key("max_notional_per_order"),
            "an unset Option must be absent, not an error: {policy:?}"
        );
        assert_eq!(lookup(Some(policy), &["max_notional_per_order"]), None);
    }

    /// `lookup`'s multi-segment arm and `leaf_count`'s recursive arm lost their only in-model
    /// exercise when `policy.rate.max_utilization` was removed. They are asserted directly rather
    /// than left to rot: both are load-bearing the day a nested table returns, and a helper whose
    /// only caller disappeared is exactly the kind of code that is silently wrong when it comes
    /// back.
    #[test]
    fn nested_tables_still_count_and_resolve_by_leaf() {
        let table: toml::Table = "flat = 1\n[nest]\ninner = 2\ndeeper = 3\n".parse().unwrap();
        assert_eq!(leaf_count(&table), 3, "a nested table counts its LEAVES, not itself");
        assert_eq!(lookup(Some(&table), &["nest", "inner"]), Some("2".to_string()));
        assert_eq!(lookup(Some(&table), &["nest", "missing"]), None);
        assert_eq!(lookup(Some(&table), &["flat"]), Some("1".to_string()));
    }

    /// The derived half must cover the registry exactly — one row per flag, keyed and pathed by the
    /// field name, and the kill switch marked as presence-armed.
    #[test]
    fn every_registered_flag_has_a_row() {
        let keys = setting_keys();
        for meta in FLAG_REGISTRY {
            let want = format!("flags.{}", meta.field);
            let row = keys.iter().find(|k| k.key == want).unwrap_or_else(|| panic!("{want}"));
            assert_eq!(row.path, vec![meta.field]);
            assert_eq!(row.env, vec![meta.env]);
            assert_eq!(row.env_presence_only, meta.env == POLY_REDEEM_HALT_ENV);
        }
        assert_eq!(keys.iter().filter(|k| k.file == FLAGS_FILE).count(), FLAG_REGISTRY.len());
    }

    /// The venue half must cover the ROSTER exactly — one row per `vike_model::VENUES` id, keyed
    /// `policy.venues.<id>` and pathed into the `[venues]` table. Exhaustive, so a new bridge crate
    /// reddens this until `config show` can disclose its ceiling.
    #[test]
    fn every_roster_venue_has_a_row() {
        let keys = setting_keys();
        for venue in vike_model::VENUES {
            let want = format!("policy.venues.{venue}");
            let row = keys.iter().find(|k| k.key == want).unwrap_or_else(|| panic!("{want}"));
            assert_eq!(row.file, POLICY_FILE);
            assert_eq!(row.path, vec!["venues", *venue], "the in-file path carries no section");
            assert!(row.env.is_empty(), "a ceiling has no env layer");
        }
        let venue_rows = keys.iter().filter(|k| k.key.starts_with("policy.venues.")).count();
        assert_eq!(venue_rows, vike_model::VENUES.len(), "exactly the roster, no extras");
    }

    /// Policy is file-only BY CONSTRUCTION, and this table must not quietly claim otherwise: a
    /// variable listed on a policy row would be a variable this module tells an operator can raise
    /// a ceiling, which is the exact belief the sealed traits exist to make false.
    #[test]
    fn no_policy_row_declares_an_environment_variable() {
        for row in setting_keys().iter().filter(|k| k.file == POLICY_FILE) {
            assert!(row.env.is_empty(), "{} must have no env layer", row.key);
        }
    }

    #[test]
    fn keys_are_sorted_and_prefixed_by_their_section() {
        let keys = setting_keys();
        let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for k in &keys {
            let section = k.key.split('.').next().unwrap();
            assert!(
                matches!(section, "policy" | "config" | "preferences" | "flags"),
                "{} has no known section",
                k.key
            );
        }
    }
}
