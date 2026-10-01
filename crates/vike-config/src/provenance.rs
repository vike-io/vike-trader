//! **Which layer set each typed setting** — the row half of `vike-cli config show`.
//!
//! [`fn@crate::load`] answers *what* the settings resolved to. It cannot answer the question an
//! operator actually asks, which is *did my write take effect?* — so this module answers, per key,
//! whether the environment, the settings database, or nothing at all set it.
//!
//! ⚠ **This module used to carry a THIRD source — a settings-directory file — and a `WHICH SOURCE
//! answers` probe that chose between it and the database.** `docs/decisions/0086` removes both:
//! `Origin::File`, `PRECEDENCE_FILES`, `Description::files`/`Description::authority` and every
//! `_FILE` constant this module read are GONE, because there are no settings files any more — a key
//! resolves from a row, or from its compiled-in default, full stop.
//!
//! # Provenance is measured, never inferred
//!
//! An [`Origin`] is decided by **key PRESENCE in a resolved layer**, not by diffing effective values
//! against defaults. A row that sets `max_leverage = 1.0` — the same number as the code default — is
//! a row that WAS written and DID configure that key, and a value-diff would report `default` and
//! tell the operator their write did nothing. Presence is a fact about the store; "the value equals
//! the default" is a coincidence.
//!
//! The layers are examined highest-precedence first, exactly mirroring [`crate::load_with_cli`]'s
//! own order: env → the settings DATABASE → the code default. There is deliberately no CLI arm —
//! `vike-cli`'s dispatcher resolves settings through [`fn@crate::load`], so a CLI layer would be a
//! layer this description's caller never applied.
//!
//! **[`Policy`](crate::Policy) rows can only ever report `default` or `db`.** That is not a
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
    JOURNAL_DIR_ENV, LOG_DIR_ENV, STORE_ROOT_ENV, TRADEHUB_ACCOUNT_ADMIN_ENV, TRADEHUB_ADDR_ENV,
    TRADEHUB_ADVERTISE_ADDR_ENV,
};
use crate::error::ConfigError;
use crate::flags::{FLAG_REGISTRY, POLY_REDEEM_HALT_ENV};
use crate::load::Settings;
use crate::preferences::{
    CHART_STYLE_ENV, LOG_FILE_LEVEL_ENV, LOG_LEVEL_ENV, RUST_LOG_ENV, SWEEP_THREADS_ENV,
};

/// The layer that set an effective value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Nothing set it — the compiled-in default stands.
    Default,
    /// An environment variable, by NAME — the one that actually matched, which for
    /// `preferences.log_level` distinguishes `RUST_LOG` from its `VIKE_LOG` alias.
    Env(&'static str),
    /// **A row of the settings DATABASE** — `<project>/settings/db/vike.db`'s `setting` table, or
    /// its `venue_arming` table for a `policy.venues.*` / `policy.accounts` row.
    ///
    /// ⚠ **This was the "fourth word", temporary while a `File` origin still existed.**
    /// `docs/decisions/0086` retires `File` outright, so `Db` is simply the ordinary settings answer
    /// now — the word it was always going to become once the files retired.
    Db,
}

impl Origin {
    /// The stable machine word: `default` / `db` / `env`. Pinned by test — a tool reading `--json`'s
    /// `origin` field must keep reading the same words.
    ///
    /// ⚠ **Two words are RETIRED.** `project` was produced only by the per-project
    /// `<project>/vike.toml` layer, gone since that file is refused at load (see
    /// [`crate::removed`]). `file` was produced by a settings-directory TOML, gone with
    /// `docs/decisions/0086`. Both are narrowings: a consumer that matched either sees a branch that
    /// can no longer be taken, which is dead code, not a break. A consumer that matched `"env"` or
    /// `"default"` sees no difference at all, and one that matched `"db"` sees it unconditionally
    /// rather than only when the box had crossed over.
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::Env(_) => "env",
            Origin::Db => "db",
        }
    }

    /// The layer's own name: the variable name, the store's own artifact, or `""` for a default.
    pub fn detail(&self) -> &'static str {
        match self {
            Origin::Default => "",
            Origin::Env(v) => v,
            // Not the absolute path: this is a `&'static str`, and the ABSOLUTE location is already
            // disclosed once, by `config show`'s own store line. Here it names the artifact.
            Origin::Db => "db/vike.db",
        }
    }

    /// One human cell: `default`, `env:VIKE_RECONCILE`, or `db`.
    pub fn label(&self) -> String {
        match self {
            Origin::Default => "default".to_string(),
            Origin::Env(v) => format!("env:{v}"),
            Origin::Db => "db".to_string(),
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

/// **The layers this crate advertises, HIGHEST precedence first** — the order [`describe_with_source`]
/// tries them, which is [`crate::load_with_source`]'s own order reversed.
///
/// ⚠ **This used to be a PAIR — `PRECEDENCE_FILES` / `PRECEDENCE_STORE` — chosen per box by
/// `vike_config::Authority`.** `docs/decisions/0086` collapses it to the one table below: there is
/// no second source for any box to answer from any more.
///
/// `crates/vike-config/tests/layers_are_reachable.rs` pins this table against [`Origin`]'s variants
/// and against the order [`describe_with_source`] really resolves in, and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` proves every entry actually takes effect
/// through the shipped binary — an advertised layer that no composition root reaches gives the
/// operator positive confirmation of something false, exactly the defect a `project` row was before
/// its layer was deleted.
///
/// There is deliberately no CLI row: [`describe`] takes no [`crate::CliOverrides`], so a CLI layer
/// here would be one this description's caller never applied.
pub const PRECEDENCE: [Layer; 3] = [
    Layer { kind: "env", label: "env" },
    Layer { kind: "db", label: "the settings database" },
    Layer { kind: "default", label: "default" },
];

/// The precedence header `vike-cli config show` prints, rendered from [`PRECEDENCE`] itself.
///
/// Derived rather than written out because the header being FALSE is the defect this whole pair of
/// gates exists to remove: a hand-typed line naming a layer nobody reads is how the per-project file
/// was advertised for two months. A layer added to the table shows up here automatically and the
/// reachability gates then refuse to go green until a binary really applies it; a layer removed from
/// it disappears from the header in the same commit, with nothing to remember.
#[must_use]
pub fn precedence_line() -> String {
    let layers: Vec<&str> = PRECEDENCE.iter().map(|l| l.label).collect();
    // `Policy` implements neither sealed override trait under any configuration, so this
    // parenthetical is a fact about the type rather than about which source answered — unlike the
    // pre-0086 header, which had to say FILE ONLY or STORE ONLY depending on the box.
    format!("precedence: {}   (policy: STORE ONLY)", layers.join(" > "))
}

/// One key this crate can resolve: where it lives in a section, and which variables can override it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingKey {
    /// The dotted key as an operator names it — `policy.max_notional_per_order`. Its first segment
    /// is the [`Settings`] section, which is also how the value is looked up.
    pub key: String,
    /// The settings SECTION this key belongs to — `"policy"` / `"config"` / `"preferences"` /
    /// `"flags"`. ⚠ Held `"policy.toml"` etc. until `docs/decisions/0086`: it named the
    /// settings-directory FILE a key lived in, and there is no such file any more.
    pub section: &'static str,
    /// The key's path INSIDE that section — no section prefix (the `policy` section holds
    /// `max_leverage`, not `policy.max_leverage`).
    pub path: Vec<&'static str>,
    /// Environment variables that can override it, in the order [`crate::layers`] tries them.
    /// Empty for a key with no env layer at all — every [`crate::Policy`] key.
    pub env: Vec<&'static str>,
    /// `true` for a variable whose mere PRESENCE arms it, whatever the value — only ever
    /// [`POLY_REDEEM_HALT_ENV`], a kill switch no value could un-set, and only while it had an
    /// environment layer. Decision 0095 retired that variable, so this is `false` for every key
    /// today; it stays derived rather than deleted so a re-armed layer could not forget the rule.
    pub env_presence_only: bool,
}

/// One resolved setting, ready to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSetting {
    /// The dotted key.
    pub key: String,
    /// The settings section this key belongs to.
    pub section: &'static str,
    /// The EFFECTIVE value, rendered. `None` means unset — an `Option` field nothing filled in.
    pub value: Option<String>,
    /// The compiled-in default, rendered, or `None` when the default is "unset".
    pub default: Option<String>,
    /// The layer that set it.
    pub origin: Origin,
    /// What that layer LITERALLY holds, rendered — `None` for [`Origin::Default`].
    pub origin_value: Option<String>,
    /// `true` when [`Self::value`] is not what [`Self::origin_value`] says, i.e. a later rule moved
    /// it. ⚠ NO such rule exists today. The field stays because the CONCEPT is the taxonomy's spine
    /// and a future bound/value pair will set it again.
    ///
    /// Numeric comparison, not string comparison: `max_leverage = 3` is a JSON integer while the
    /// effective value is the float `3.0`, and reporting that as an adjustment would be a
    /// formatting artifact reported as a fact.
    pub adjusted: bool,
}

/// Everything `config show`'s row half needs, from one pass.
#[derive(Debug, Clone)]
pub struct Description {
    /// THE settings directory, or `None` when no project sits above the caller's working
    /// directory — in which case every row below reports `default` and no store could even be
    /// opened.
    pub settings_dir: Option<PathBuf>,
    /// The fully-resolved settings, including [`Settings::warnings`] — the caller SURFACES those
    /// (this crate deliberately does not log; see [`Settings::warnings`]).
    pub settings: Settings,
    /// One row per key, sorted by key.
    pub rows: Vec<ResolvedSetting>,
    /// Copied off [`Settings::store_refusal`] so a renderer need not reach into the settings to
    /// find it. `Some` means **the values above are not necessarily what a daemon on this box
    /// would resolve**, and a surface that prints the table without leading with that fact hands
    /// the operator positive confirmation of something false.
    pub store_refusal: Option<String>,
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
        section: &'static str,
        path: &[&'static str],
        env: &[&'static str],
    ) -> SettingKey {
        SettingKey {
            key: key.to_string(),
            section,
            path: path.to_vec(),
            env: env.to_vec(),
            env_presence_only: false,
        }
    }

    let mut out = vec![
        // -- policy: db only, by construction (no `EnvOverride`, no `CliOverride`, both sealed).
        row("policy.max_leverage", "policy", &["max_leverage"], &[]),
        row("policy.max_notional_per_order", "policy", &["max_notional_per_order"], &[]),
        // The ACCOUNT-aggregate exposure ceiling. Same empty `env` slice as its per-order sibling
        // and for the same reason — it is the whole point of the field that no exported variable
        // can widen it — and the same `null`-when-absent leaf shape, so `config show` reports
        // `default` (i.e. UNCAPPED) on a box that never wrote the key.
        row("policy.max_account_exposure", "policy", &["max_account_exposure"], &[]),
        // The ceiling on the equity FIGURE the sizing and admission lanes may see — the cap that
        // decouples this deployment's position sizes from a wallet a third party can move.
        row("policy.max_sizing_equity", "policy", &["max_sizing_equity"], &[]),
        row("policy.market_slippage", "policy", &["market_slippage"], &[]),
        // ⚠ The empty `env` slice is the POINT for this one, not boilerplate: it governs what a
        // KILL SWITCH lets out, so `config show` must be able to say "the row or default, nothing
        // else" — see `crate::Policy::halt_admit`.
        row("policy.halt_admit", "policy", &["halt_admit"], &[]),
        // The dead-man switch. Same empty `env` slice, and it matters in BOTH directions here:
        // nothing in the environment can arm a silence-detector on an FX box (a Friday halt with no
        // diff to review) and nothing can disarm one an operator relies on — see `crate::Policy::
        // deadman_timeout_ms`. Both rows report `default` on a box with no such row, and for the
        // timeout that default is ABSENCE (`null`, the `max_notional_per_order` shape): the switch
        // is OFF, and `vike-tradehub`'s live mount warns once that it is.
        row("policy.deadman_timeout_ms", "policy", &["deadman_timeout_ms"], &[]),
        row("policy.deadman_action", "policy", &["deadman_action"], &[]),
        // The LINK dead-man's grace (M13). Same empty `env` slice and the same both-directions
        // argument — and one difference an operator reading `config show` has to be able to see:
        // this key's `default` is ARMED, not absent. The serialized leaf is `null` when nothing
        // wrote it, exactly like the timeout above, so the ORIGIN cell reads `default` in both
        // cases while the two defaults mean opposite things.
        row("policy.link_deadman_grace_ms", "policy", &["link_deadman_grace_ms"], &[]),
        // -- config: full chain.
        row("config.store_root", "config", &["store_root"], &[STORE_ROOT_ENV]),
        row("config.log_dir", "config", &["log_dir"], &[LOG_DIR_ENV]),
        row("config.journal_dir", "config", &["journal_dir"], &[JOURNAL_DIR_ENV]),
        row("config.backtest_addr", "config", &["backtest_addr"], &[BACKTEST_ADDR_ENV]),
        row("config.datahub_addr", "config", &["datahub_addr"], &[DATAHUB_ADDR_ENV]),
        row("config.tradehub_addr", "config", &["tradehub_addr"], &[TRADEHUB_ADDR_ENV]),
        row(
            "config.datahub_advertise_addr",
            "config",
            &["datahub_advertise_addr"],
            &[DATAHUB_ADVERTISE_ADDR_ENV],
        ),
        // The daemon's SELF-REPORT override. ⚠ Its blank ENV/row columns in `config show` mean
        // something different from every other address row's: unset is the ordinary state, in
        // which the daemon reports the address it discovered from its own routing table. "Not
        // set" is not "not reported" here.
        row(
            "config.tradehub_advertise_addr",
            "config",
            &["tradehub_advertise_addr"],
            &[TRADEHUB_ADVERTISE_ADDR_ENV],
        ),
        // **The account-admin barrier DECLARATION** (`docs/decisions/0065`), and the one `config`
        // row whose value is not an address, a path or a boolean but a three-valued word: unset /
        // `off` (the default, and byte-identical to a daemon without the verb), `loopback` (the
        // one assertion the process can CHECK, against its own bind, refusing the capability when
        // the bind disagrees) and `contained` (a barrier outside the process, asserted and never
        // verified).
        row(
            "config.tradehub_account_admin",
            "config",
            &["tradehub_account_admin"],
            &[TRADEHUB_ACCOUNT_ADMIN_ENV],
        ),
        // The CLIENT-side dial address `vike-cli backend connect` writes — the one `config` key with
        // an EMPTY env slice, and `crate::config::Config::node_addr` argues why: `--node` is
        // already its per-invocation override, so an environment layer would be a third spelling of
        // one answer.
        row("config.node_addr", "config", &["node_addr"], &[]),
        row("config.instance_origin", "config", &["instance_origin"], &[INSTANCE_ORIGIN_ENV]),
        // -- preferences: full chain.
        row("preferences.log_level", "preferences", &["log_level"], &[RUST_LOG_ENV, LOG_LEVEL_ENV]),
        row(
            "preferences.log_file_level",
            "preferences",
            &["log_file_level"],
            &[LOG_FILE_LEVEL_ENV],
        ),
        row("preferences.chart_style", "preferences", &["chart_style"], &[CHART_STYLE_ENV]),
        row("preferences.sweep_threads", "preferences", &["sweep_threads"], &[SWEEP_THREADS_ENV]),
        // The five APPEARANCE preferences (design system spec §5). The empty `env` slice is the
        // ruling, not an omission: "none of the five gets an environment variable — they are a
        // person's preferences, not deployment knobs" (`config.node_addr`'s shape).
        row("preferences.theme", "preferences", &["theme"], &[]),
        row("preferences.market_colors", "preferences", &["market_colors"], &[]),
        row("preferences.header_gradient", "preferences", &["header_gradient"], &[]),
        row("preferences.density", "preferences", &["density"], &[]),
        row("preferences.text_size", "preferences", &["text_size"], &[]),
    ];

    // -- policy.venues: derived, one row per ROSTER venue. `[venues]`'s keys ARE the venue ids, so
    //    the in-section path is `venues.<id>` and the dotted key adds the section like every other
    //    row. Empty `env`, like every policy row and for the same structural reason.
    out.extend(vike_model::VENUES.iter().copied().map(|venue| SettingKey {
        key: format!("policy.venues.{venue}"),
        section: "policy",
        path: vec!["venues", venue],
        env: Vec::new(),
        env_presence_only: false,
    }));

    // -- policy.accounts: the `[accounts]` table — per-ACCOUNT arming ceilings, `{venue: {LABEL:
    //    mode}}`. Empty `env`, like every policy row and for the same structural reason. ONE row
    //    for the whole table — the opposite of the `policy.venues` decision above — because an
    //    account LABEL is a name the operator chose at runtime rather than a compile-time roster.
    out.push(SettingKey {
        key: "policy.accounts".to_string(),
        section: "policy",
        path: vec!["accounts"],
        env: Vec::new(),
        env_presence_only: false,
    });

    // -- policy.account_exposure: the `[account_exposure]` table — per-ACCOUNT exposure ceilings,
    //    `{venue: {LABEL: figure}}`. ONE row for the whole table, for the reason the `accounts` row
    //    above gives verbatim.
    out.push(SettingKey {
        key: "policy.account_exposure".to_string(),
        section: "policy",
        path: vec!["account_exposure"],
        env: Vec::new(),
        env_presence_only: false,
    });

    // -- flags: derived, one row per registry entry.
    out.extend(FLAG_REGISTRY.iter().map(|meta| SettingKey {
        key: format!("flags.{}", meta.field),
        section: "flags",
        path: vec![meta.field],
        env: if meta.reads_env() { vec![meta.env] } else { Vec::new() },
        env_presence_only: meta.env == POLY_REDEEM_HALT_ENV && meta.reads_env(),
    }));

    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Resolve the settings AND describe where every key's value came from.
///
/// Takes the same two parameters as [`fn@crate::load`], and calls it — so a description can never
/// disagree with the settings the caller is actually running on (a present `<project>/vike.toml`
/// included).
pub fn describe(
    settings_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<Description, ConfigError> {
    describe_with_source(settings_dir, crate::source::StoreLayer::NotConsulted(NO_STORE), env)
}

/// What [`describe`] declares when it resolves no store.
///
/// ⚠ It is a REAL limitation and not a formality: a description built this way cannot report an
/// `Origin::Db`, so it describes a resolution nothing is actually running on when a store carries
/// rows. `vike_config::boot_lines` and `vike-cli`'s `config show`/`config check` all take a
/// [`crate::StoreLayer`] as a required parameter for exactly this reason. This entry point is for
/// TESTS and for a caller that genuinely has no store to offer.
const NO_STORE: &str = "`vike_config::describe` resolves no settings store — a disclosure surface calls \
     `describe_with_source` with the arm the binary read";

/// [`describe`] plus **the settings SOURCE** — the disclosure twin of [`crate::load_with_source`].
///
/// Same rule as the loader: every key the store sets reports [`Origin::Db`]; everything else
/// reports [`Origin::Default`] unless the environment overrides it. `source` is DATA; this crate
/// never opens the store (see [`crate::mirror`]).
///
/// ⚠ **This function must never inherit the load's SEAL refusal, and one `?` here would give it
/// one.** `config show` is the disclosure verb an operator reaches for precisely when a box will
/// not start, so a store that says something illegal has to render a marked table rather than a
/// second copy of the refusal. It gets that for free today — [`crate::load_with_source`] never
/// returns `Err` for a seal/row problem, only for the removed-project-file refusal and a genuinely
/// malformed environment/CLI value, both of which this function is right to propagate.
pub fn describe_with_source(
    settings_dir: Option<&Path>,
    source: crate::source::StoreLayer<'_>,
    env: &HashMap<String, String>,
) -> Result<Description, ConfigError> {
    let settings = crate::load::load_with_source(
        settings_dir,
        source,
        env,
        &crate::layers::CliOverrides::default(),
    )?;
    let store_refusal = settings.store_refusal.clone();

    // The store's own rows, assembled by the SAME call the loader applied — so the presence test
    // below is one lookup over exactly what `apply_rows` deserialized, rather than a second reading
    // of the rows.
    let db_values: HashMap<&'static str, serde_json::Value> = match source.rows() {
        Some(s) => match crate::mirror::section_values(s) {
            Ok(values) => values.into_iter().collect(),
            Err(
                crate::mirror::RowRefusal::Unreadable(_) | crate::mirror::RowRefusal::Illegal(_),
            ) => {
                // The loader already marked this on `settings.seal_refusal`/`warnings` — a
                // disclosure command must not brick over the same finding a second time. Nothing
                // this half can report is trustworthy either, so it reports nothing rather than
                // guessing.
                HashMap::new()
            }
        },
        None => HashMap::new(),
    };

    // The value side: the effective settings and the code defaults, serialized once each.
    let effective = sections(&settings);
    let defaults = sections(&Settings::default());

    let mut rows = Vec::new();
    for spec in setting_keys() {
        let section = spec.key.split('.').next().unwrap_or_default();
        let value = lookup(effective.get(section), &spec.path);
        let default = lookup(defaults.get(section), &spec.path);

        // Precedence, highest first — the mirror of `load_with_source`'s own order.
        let (origin, origin_value) = 'origin: {
            for &var in &spec.env {
                if let Some(raw) = env_value(env, var, spec.env_presence_only) {
                    break 'origin (Origin::Env(var), Some(raw));
                }
            }
            if let Some(v) = json_lookup(db_values.get(spec.section), &spec.path) {
                break 'origin (Origin::Db, Some(v));
            }
            (Origin::Default, None)
        };

        let adjusted =
            matches!(origin, Origin::Db) && !same_value(origin_value.as_deref(), value.as_deref());

        rows.push(ResolvedSetting {
            key: spec.key,
            section: spec.section,
            value,
            default,
            origin,
            origin_value,
            adjusted,
        });
    }

    Ok(Description {
        settings_dir: settings_dir.map(Path::to_path_buf),
        settings,
        rows,
        store_refusal,
    })
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

/// **One key's EFFECTIVE value in a resolved [`Settings`]**, rendered the way `config show` prints
/// it — `None` for a key nothing set.
///
/// Exposed because [`crate::drift`] compares two whole resolutions key by key and must speak this
/// vocabulary rather than a second one. It is the same `sections` + [`lookup`] pair the row loop in
/// [`describe_with_source`] uses, so the two can never render one value two ways.
#[must_use]
pub fn effective_value(settings: &Settings, spec: &SettingKey) -> Option<String> {
    let section = spec.key.split('.').next().unwrap_or_default();
    lookup(sections(settings).get(section), &spec.path)
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

/// [`lookup`]'s twin over the settings STORE's assembled objects, which are `serde_json::Value`
/// since the value column became a JSON scalar (`crate::mirror`).
///
/// ⚠ **The rendering must be the TOML one, not JSON's, and that is a correctness requirement rather
/// than a cosmetic one.** [`describe_with_source`] computes `adjusted` by comparing this string
/// against the EFFECTIVE value, which is rendered by [`render`] out of a `toml::Value` — so a leaf
/// this function spells differently is reported as a value *a later rule moved*, which is a lie
/// `vike-cli config show` prints.
///
/// It bites on exactly one row today: [`setting_keys`] has a single spec whose leaf is not a
/// scalar — `policy.accounts`, the `{venue: {LABEL: mode}}` table — where the TOML rendering is
/// `{ hyperliquid = { ALT = "paper" } }` and `serde_json`'s own `Display` renders
/// `{"hyperliquid":{"ALT":"paper"}}`.
fn json_lookup(object: Option<&serde_json::Value>, path: &[&str]) -> Option<String> {
    let mut value = object?;
    for segment in path {
        value = value.get(*segment)?;
    }
    Some(match toml::Value::try_from(value) {
        Ok(as_toml) => render(&as_toml),
        Err(_) => match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        },
    })
}

/// A variable's value under [`crate::layers`]' own rules: empty is UNSET, except for a
/// presence-armed variable ([`SettingKey::env_presence_only`]), where any value (including empty)
/// counts.
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
        assert_eq!(Origin::Env("X").kind(), "env");
        assert_eq!(Origin::Db.kind(), "db");

        assert_eq!(Origin::Env("ACME_X").label(), "env:ACME_X");
        assert_eq!(Origin::Default.detail(), "");
        assert_eq!(Origin::Db.detail(), "db/vike.db");

        // THREE words, and the two retired ones (`project`, `file`) must never come back as a
        // spelling `--json` emits: the layers they named are refused at load or deleted outright.
        let words: Vec<&str> =
            [Origin::Default, Origin::Env("X"), Origin::Db].iter().map(Origin::kind).collect();
        assert_eq!(words, ["default", "env", "db"]);
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

    /// The derived half must cover the registry exactly — one row per flag, keyed and pathed by the
    /// field name, and the kill switch marked as presence-armed only while its variable is read.
    #[test]
    fn every_registered_flag_has_a_row() {
        let keys = setting_keys();
        for meta in FLAG_REGISTRY {
            let want = format!("flags.{}", meta.field);
            let row = keys.iter().find(|k| k.key == want).unwrap_or_else(|| panic!("{want}"));
            assert_eq!(row.path, vec![meta.field]);
            assert_eq!(row.env, if meta.reads_env() { vec![meta.env] } else { Vec::new() });
            assert_eq!(row.env_presence_only, meta.env == POLY_REDEEM_HALT_ENV && meta.reads_env());
        }
        assert_eq!(keys.iter().filter(|k| k.section == "flags").count(), FLAG_REGISTRY.len());
    }

    /// The venue half must cover the ROSTER exactly — one row per `vike_model::VENUES` id, keyed
    /// `policy.venues.<id>` and pathed into the `[venues]` table.
    #[test]
    fn every_roster_venue_has_a_row() {
        let keys = setting_keys();
        for venue in vike_model::VENUES {
            let want = format!("policy.venues.{venue}");
            let row = keys.iter().find(|k| k.key == want).unwrap_or_else(|| panic!("{want}"));
            assert_eq!(row.section, "policy");
            assert_eq!(row.path, vec!["venues", *venue], "the in-section path carries no prefix");
            assert!(row.env.is_empty(), "a ceiling has no env layer");
        }
        let venue_rows = keys.iter().filter(|k| k.key.starts_with("policy.venues.")).count();
        assert_eq!(venue_rows, vike_model::VENUES.len(), "exactly the roster, no extras");
    }

    /// Policy is db-only BY CONSTRUCTION, and this table must not quietly claim otherwise.
    #[test]
    fn no_policy_row_declares_an_environment_variable() {
        for row in setting_keys().iter().filter(|k| k.section == "policy") {
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

    #[test]
    fn the_precedence_line_names_env_then_db_then_default() {
        let line = precedence_line();
        assert!(line.starts_with("precedence: env > the settings database > default"), "{line}");
        assert!(line.contains("STORE ONLY"), "{line}");
    }
}
