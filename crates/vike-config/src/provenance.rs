//! **Which layer set each typed setting** — the row half of `vike-cli config show`.
//!
//! [`fn@crate::load`] answers *what* the settings resolved to. It cannot answer the question an
//! operator actually asks, which is *did my write take effect?* — so this module answers, per key,
//! whether the settings database or nothing at all set it.
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
//! own order: the settings DATABASE → the code default. There is no environment layer (decision
//! 0111), and deliberately no CLI arm —
//! `vike-cli`'s dispatcher resolves settings through [`fn@crate::load`], so a CLI layer would be a
//! layer this description's caller never applied.
//!
//! **[`Policy`](crate::Policy) rows can only ever report `default` or `db`.** That is not a
//! limitation of this module, it is the taxonomy showing through: `Policy` does not implement the
//! sealed `CliOverride`, so there is no other layer that could have set one.
//!
//! # The key table, part declared and part derived
//!
//! [`setting_keys`] has three halves — the flat policy / config / preferences rows, written out;
//! one derived row per [`crate::FLAG_REGISTRY`] entry; and one derived row per
//! [`vike_model::VENUES`] id for the `policy.venues.*` ceilings. No count is written here, because
//! a count stops matching the table the first time a field is added.
//!
//! # Values come from the types, not from a hand-written getter
//!
//! Each row's effective value is looked up by PATH in `toml::Value::try_from(&settings.<section>)`,
//! and its default by the same path in the same serialization of `<Section>::default()`. So there
//! is no per-field accessor to forget to update, and an `Option::None` field is simply absent from
//! the serialized table — which is exactly "unset", the thing it means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::flags::FLAG_REGISTRY;
use crate::load::Settings;

/// The layer that set an effective value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Nothing set it — the compiled-in default stands.
    Default,
    /// **A row of the settings DATABASE** — `<project>/settings/db/vike.db`'s `setting` table, or
    /// its `venue_arming` table for a `policy.venues.*` / `policy.accounts` row.
    Db,
}

impl Origin {
    /// The stable machine word: `default` / `db`. Pinned by test — a tool reading `--json`'s
    /// `origin` field must keep reading the same words.
    ///
    /// ⚠ **Three words are RETIRED**: `project` (the per-project `<project>/vike.toml` layer, refused
    /// at load — see [`crate::removed`]), `file` (`docs/decisions/0086`) and `env` (decision 0111:
    /// no key is read from the process environment). All three are narrowings: a consumer that
    /// matched one sees a branch that can no longer be taken, which is dead code, not a break.
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::Db => "db",
        }
    }

    /// The layer's own name: the store's own artifact, or `""` for a default.
    pub fn detail(&self) -> &'static str {
        match self {
            Origin::Default => "",
            // Not the absolute path: this is a `&'static str`, and the ABSOLUTE location is already
            // disclosed once, by `config show`'s own store line. Here it names the artifact.
            Origin::Db => "db/vike.db",
        }
    }

    /// One human cell: `default` or `db`.
    pub fn label(&self) -> String {
        self.kind().to_string()
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
/// `crates/vike-config/tests/layers_are_reachable.rs` pins this table against [`Origin`]'s variants
/// and against the order [`describe_with_source`] really resolves in, and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` proves every entry actually takes effect
/// through the shipped binary — an advertised layer that no composition root reaches gives the
/// operator positive confirmation of something false.
///
/// There is deliberately no CLI row: [`describe`] takes no [`crate::CliOverrides`], so a CLI layer
/// here would be one this description's caller never applied.
pub const PRECEDENCE: [Layer; 2] = [
    Layer { kind: "db", label: "the settings database" },
    Layer { kind: "default", label: "default" },
];

/// The precedence header `vike-cli config show` prints, rendered from [`PRECEDENCE`] itself.
///
/// Derived rather than written out because the header being FALSE is the defect this whole pair of
/// gates exists to remove: a hand-typed line can name a layer nobody reads. A layer added to the
/// table shows up here automatically and the reachability gates then refuse to go green until a
/// binary really applies it; a layer removed from it disappears from the header in the same commit,
/// with nothing to remember.
#[must_use]
pub fn precedence_line() -> String {
    let layers: Vec<&str> = PRECEDENCE.iter().map(|l| l.label).collect();
    // `Policy` implements neither sealed override trait under any configuration, so this
    // parenthetical is a fact about the type rather than about which source answered.
    format!("precedence: {}   (policy: STORE ONLY)", layers.join(" > "))
}

/// One key this crate can resolve: where it lives in a section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingKey {
    /// The dotted key as an operator names it — `policy.max_notional_per_order`. Its first segment
    /// is the [`Settings`] section, which is also how the value is looked up.
    pub key: String,
    /// The settings SECTION this key belongs to — `"policy"` / `"config"` / `"preferences"` /
    /// `"flags"`.
    pub section: &'static str,
    /// The key's path INSIDE that section — no section prefix (the `policy` section holds
    /// `max_leverage`, not `policy.max_leverage`).
    pub path: Vec<&'static str>,
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
    fn row(key: &str, section: &'static str, path: &[&'static str]) -> SettingKey {
        SettingKey { key: key.to_string(), section, path: path.to_vec() }
    }

    let mut out = vec![
        // -- policy: the database only, by construction (no `CliOverride`, sealed).
        row("policy.max_leverage", "policy", &["max_leverage"]),
        row("policy.max_notional_per_order", "policy", &["max_notional_per_order"]),
        // The ACCOUNT-aggregate exposure ceiling, with the same `null`-when-absent leaf shape as its
        // per-order sibling, so `config show` reports `default` (i.e. UNCAPPED) on a box that never
        // wrote the key.
        row("policy.max_account_exposure", "policy", &["max_account_exposure"]),
        // The ceiling on the equity FIGURE the sizing and admission lanes may see — the cap that
        // decouples this deployment's position sizes from a wallet a third party can move.
        row("policy.max_sizing_equity", "policy", &["max_sizing_equity"]),
        row("policy.market_slippage", "policy", &["market_slippage"]),
        // It governs what a KILL SWITCH lets out, so `config show` must be able to say "the row or
        // default, nothing else" — see `crate::Policy::halt_admit`.
        row("policy.halt_admit", "policy", &["halt_admit"]),
        // The dead-man switch. Both rows report `default` on a box with no such row, and for the
        // timeout that default is ABSENCE (`null`, the `max_notional_per_order` shape): the switch
        // is OFF, and `vike-tradehub`'s live mount warns once that it is.
        row("policy.deadman_timeout_ms", "policy", &["deadman_timeout_ms"]),
        row("policy.deadman_action", "policy", &["deadman_action"]),
        // The LINK dead-man's grace (M13), and one difference an operator reading `config show` has
        // to be able to see: this key's `default` is ARMED, not absent. The serialized leaf is
        // `null` when nothing wrote it, exactly like the timeout above, so the ORIGIN cell reads
        // `default` in both cases while the two defaults mean opposite things.
        row("policy.link_deadman_grace_ms", "policy", &["link_deadman_grace_ms"]),
        // -- config.
        row("config.store_root", "config", &["store_root"]),
        row("config.log_dir", "config", &["log_dir"]),
        row("config.journal_dir", "config", &["journal_dir"]),
        row("config.backtest_addr", "config", &["backtest_addr"]),
        row("config.datahub_addr", "config", &["datahub_addr"]),
        row("config.tradehub_addr", "config", &["tradehub_addr"]),
        row("config.datahub_advertise_addr", "config", &["datahub_advertise_addr"]),
        // The daemon's SELF-REPORT override. ⚠ Its blank row in `config show` means something
        // different from every other address row's: unset is the ordinary state, in which the
        // daemon reports the address it discovered from its own routing table. "Not set" is not
        // "not reported" here.
        row("config.tradehub_advertise_addr", "config", &["tradehub_advertise_addr"]),
        // **The account-admin barrier DECLARATION** (`docs/decisions/0065`), and the one `config`
        // row whose value is not an address, a path or a boolean but a three-valued word: unset /
        // `off` (the default, and byte-identical to a daemon without the verb), `loopback` (the
        // one assertion the process can CHECK, against its own bind, refusing the capability when
        // the bind disagrees) and `contained` (a barrier outside the process, asserted and never
        // verified).
        row("config.tradehub_account_admin", "config", &["tradehub_account_admin"]),
        // The CLIENT-side dial address `vike-cli backend connect` writes; `--node` is its
        // per-invocation override (`crate::config::Config::node_addr`).
        row("config.node_addr", "config", &["node_addr"]),
        row("config.instance_origin", "config", &["instance_origin"]),
        // The rows decision 0111 added for settings that had none.
        row("config.reconcile_policy", "config", &["reconcile_policy"]),
        row("config.reconcile_interval_ms", "config", &["reconcile_interval_ms"]),
        row("config.reconcile_audit_ms", "config", &["reconcile_audit_ms"]),
        row("config.reconcile_lookback_ms", "config", &["reconcile_lookback_ms"]),
        row("config.reconcile_startup_delay_ms", "config", &["reconcile_startup_delay_ms"]),
        row("config.reconcile_balance_tol_abs", "config", &["reconcile_balance_tol_abs"]),
        row("config.reconcile_balance_tol_rel", "config", &["reconcile_balance_tol_rel"]),
        row("config.tradehub_control_rate", "config", &["tradehub_control_rate"]),
        row("config.pin_cores", "config", &["pin_cores"]),
        row("config.datahub_bind_addr", "config", &["datahub_bind_addr"]),
        row("config.datahub_live_resident", "config", &["datahub_live_resident"]),
        row("config.journal_snapshot_every", "config", &["journal_snapshot_every"]),
        // -- preferences.
        row("preferences.log_level", "preferences", &["log_level"]),
        row("preferences.log_file_level", "preferences", &["log_file_level"]),
        row("preferences.chart_style", "preferences", &["chart_style"]),
        row("preferences.sweep_threads", "preferences", &["sweep_threads"]),
        // The five APPEARANCE preferences (design system spec §5) — a person's preferences, not
        // deployment knobs.
        row("preferences.theme", "preferences", &["theme"]),
        row("preferences.market_colors", "preferences", &["market_colors"]),
        row("preferences.header_gradient", "preferences", &["header_gradient"]),
        row("preferences.density", "preferences", &["density"]),
        row("preferences.text_size", "preferences", &["text_size"]),
        // The CLIENT's advisory quantity cap (decision 0111).
        row("preferences.max_order_qty", "preferences", &["max_order_qty"]),
        // The desktop's chart-export directory (decision 0111).
        row("preferences.export_dir", "preferences", &["export_dir"]),
    ];

    // -- policy.venues: derived, one row per ROSTER venue. `[venues]`'s keys ARE the venue ids, so
    //    the in-section path is `venues.<id>` and the dotted key adds the section like every other
    //    row.
    out.extend(vike_model::VENUES.iter().copied().map(|venue| SettingKey {
        key: format!("policy.venues.{venue}"),
        section: "policy",
        path: vec!["venues", venue],
    }));

    // -- policy.accounts: the `[accounts]` table — per-ACCOUNT arming ceilings, `{venue: {LABEL:
    //    mode}}`. ONE row for the whole table — the opposite of the `policy.venues` decision above
    //    — because an account LABEL is a name the operator chose at runtime rather than a
    //    compile-time roster.
    out.push(SettingKey {
        key: "policy.accounts".to_string(),
        section: "policy",
        path: vec!["accounts"],
    });

    // -- policy.account_exposure: the `[account_exposure]` table — per-ACCOUNT exposure ceilings,
    //    `{venue: {LABEL: figure}}`. ONE row for the whole table, for the reason the `accounts` row
    //    above gives verbatim.
    out.push(SettingKey {
        key: "policy.account_exposure".to_string(),
        section: "policy",
        path: vec!["account_exposure"],
    });

    // -- flags: derived, one row per registry entry.
    out.extend(FLAG_REGISTRY.iter().map(|meta| SettingKey {
        key: format!("flags.{}", meta.field),
        section: "flags",
        path: vec![meta.field],
    }));

    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Resolve the settings AND describe where every key's value came from.
///
/// Takes the same parameter as [`fn@crate::load`], and calls it — so a description can never
/// disagree with the settings the caller is actually running on (a present `<project>/vike.toml`
/// included).
pub fn describe(settings_dir: Option<&Path>) -> Result<Description, ConfigError> {
    describe_with_source(settings_dir, crate::source::StoreLayer::NotConsulted(NO_STORE))
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
/// reports [`Origin::Default`]. `source` is DATA; this crate
/// never opens the store (see [`crate::mirror`]).
///
/// ⚠ **This function must never inherit the load's SEAL refusal, and one `?` here would give it
/// one.** `config show` is the disclosure verb an operator reaches for precisely when a box will
/// not start, so a store that says something illegal has to render a marked table rather than a
/// second copy of the refusal. It gets that for free today — [`crate::load_with_source`] never
/// returns `Err` for a seal/row problem, only for the removed-project-file refusal and a genuinely
/// malformed CLI value, both of which this function is right to propagate.
pub fn describe_with_source(
    settings_dir: Option<&Path>,
    source: crate::source::StoreLayer<'_>,
) -> Result<Description, ConfigError> {
    let settings = crate::load::load_with_source(
        settings_dir,
        source,
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
        let (origin, origin_value) = match json_lookup(db_values.get(spec.section), &spec.path) {
            Some(v) => (Origin::Db, Some(v)),
            None => (Origin::Default, None),
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

#[path = "provenance_tests.rs"]
#[cfg(test)]
mod provenance_tests;
