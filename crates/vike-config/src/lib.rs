//! `vike-config` — the TYPED settings model and its layered loader.
//!
//! Every setting a process can be told lives in one of FOUR types, split by AUTHORITY — who may
//! change a value, and from where — not by subsystem (`docs/decisions/0005-settings-split-by-authority.md`):
//!
//! | type | holds | overridable by |
//! |---|---|---|
//! | [`Policy`] | hard ceilings — leverage, order notional, account exposure, market-slippage band, dead-man, halt admission, **per-venue and per-account arming** | **the settings database only** |
//! | [`Config`] | deployment — store paths, log dir, listen addresses | database, env, CLI |
//! | [`Preferences`] | taste + tuning — log levels, chart style, sweep threads | database, env, CLI |
//! | [`Flags`] | toggles — reconcile, poly exec, control surfaces, recorders; each with an OWNER and a REVIEW DATE ([`FLAG_REGISTRY`]) | database, env, CLI |
//!
//! and one order, applied to all of them. `docs/decisions/0086-settings-live-only-in-the-database.md`:
//! settings live ONLY as rows in `<project>/settings/db/vike.db`, and there are no settings files —
//! not as source, fallback, export or way back:
//!
//! ```text
//! code defaults -> the settings database's rows -> env -> CLI
//! ```
//!
//! A key with no row is not an error: the compiled-in default stands, and
//! `load(None, &HashMap::new())` is the pure code-default answer. [`PRECEDENCE`] is the table
//! `vike-cli config show` prints its header from, and `crates/vike-config/tests/layers_are_reachable.rs`
//! pins it to the layers that exist, and `crates/vike-cli/tests/settings_layers_reachable.rs` proves each
//! one takes effect through the shipped binary.
//!
//! ## What the compiler enforces
//!
//! **[`Policy`] has no `from_env`, no `apply_env` and no `apply_cli` — not on the type, not on any
//! trait it implements.** Overriding a ceiling from the environment is *unrepresentable*, not
//! discouraged: `settings.policy.apply_env(&env)` is a compile error. [`EnvOverride`] and
//! [`CliOverride`] are sealed traits implemented by exactly the other three types, so the table
//! above cannot be widened from outside this crate either.
//!
//! A ceiling you can override with an environment variable is not a ceiling. The failure mode is
//! silent — a stale systemd unit or an inherited shell raises the limit with no row changed, no
//! diff, no review, and a run that looks completely normal — so the class is removed rather than
//! documented. [`mod@layers`] carries the full argument.
//!
//! ## What the loader refuses to be quiet about
//!
//! - **Unknown keys are rejected BY NAME** (`deny_unknown_fields` on every patch struct). A typo'd
//!   setting that silently does nothing is worse than an error — the operator sets a ceiling,
//!   sees no complaint, and does not have it.
//! - ⚠ **…and `deny_unknown_fields` does NOT reach inside a MAP.** It governs a struct's FIELD
//!   names; serde treats a map's keys as data. [`Policy::venues`] is the one map in the model, so
//!   its venue ids are checked by hand against [`vike_model::VENUES`] and an unknown one is refused
//!   with the roster in the message. A value type that is an ENUM gets its refusal from serde for
//!   free; a KEY space never does.
//! - **Every error names where it came from and the KEY.** `"invalid config"` is useless;
//!   ``"settings database (section `policy`): market_slippage = 0.9 exceeds the allowed maximum
//!   0.05"`` is the bar. See [`ConfigError`].
//! - **A truthy typo on a flag is an error**, where `VIKE_RECONCILE=true` would otherwise silently
//!   read false. Same exact-`"1"` idiom, but the operator is told. See [`mod@flags`].
//! - **A key that was REMOVED is refused by name, not ignored** — [`PolicyPatch::max_total_exposure`],
//!   [`PolicyPatch::rate`] and [`PreferencesPatch::rate_utilization`] are tombstones whose errors say
//!   where the concept lives now, [`refuse_removed_env`] is the same rule for variables
//!   ([`REMOVED_ENV`]), and [`REMOVED_PROJECT_FILE`] for the retired `<project>/vike.toml`. An
//!   operator who wrote a limit believes they have it; "unknown field" would tell them only that
//!   the key is wrong.
//! - **A setting nothing reads is worse than an unimplemented feature**, because `config show`
//!   would confirm something false. [`CONSUMPTION`] is the ratchet: one row per
//!   `config`/`preferences`/`flags` key naming the file and the read that consumes it, or a written
//!   admission that nothing does (`crates/vike-config/tests/settings_are_consumed.rs`); `Policy` has its own,
//!   older gate (`crates/vike-config/tests/policy_is_consumed.rs`).
//!
//! ⚠ There is currently NO policy-bounds-preference clamp. The mechanism (policy sets the bound, a
//! preference sets the value inside it) is still the taxonomy's spine, with no instance. See
//! [`mod@preferences`].
//!
//! ## Where things live
//!
//! The `pub mod` roster below is grouped by role; each module's own `//!` is its contract.
//!
//! - **The four authorities** — [`mod@policy`], [`mod@config`], [`mod@preferences`], [`mod@flags`].
//! - **Loading** — [`mod@load`] (the entry points), [`mod@source`] (the settings store as the loader
//!   takes it), [`mod@mirror`] (the database's rows applied as the one layer), [`mod@layers`] (the sealed
//!   override traits), [`mod@error`], [`mod@drift`].
//! - **Arming** — [`mod@venue_mode`] (the per-venue ceiling that only ever refuses), [`mod@venue_arming`]
//!   (one account's row), [`mod@venue_accounts`] (the shared-book rule), [`mod@arming`] (the credential
//!   file may not arm real money).
//! - **Refusals and remedies** — [`mod@removed`], [`mod@remedy`] (how an operator WRITES a key),
//!   [`mod@redact`] (which names never print a value).
//! - **Reading and writing rows** — [`mod@write`] (the write planner), [`mod@provenance`] and
//!   [`mod@show`] (the two halves of `vike-cli config show`), [`mod@boot`] (the startup disclosure).
//! - **Tables a gate holds to the tree** — [`mod@ceilings`], [`mod@consumed`], [`mod@profile_risk`].
//!
//! ## Bounds are imported, never restated
//!
//! [`Policy::market_slippage`] is validated against [`vike_model::market_slippage`]'s
//! `MIN_MARKET_SLIPPAGE`/`MAX_MARKET_SLIPPAGE`. A second copy of a bound is the split-brain that
//! module's own doc warns about, and it is the reason `vike-model` is a dependency of an otherwise
//! self-contained crate.
//!
//! ## I/O ownership
//!
//! The env map and the settings directory are PARAMETERS. This crate never reads `std::env`, never
//! walks for a directory and never expands `~`, and the LOADER never opens the settings database:
//! the BINARY reads it (`vike_secrets::read_settings_in`) and hands it over as a [`StoreLayer`], and a
//! composition root gets all of that from `vike_boot::boot`. The one module that writes rows is
//! [`mod@write`], through the one-row write primitive `vike-secrets` offers. Libraries take configuration as
//! arguments, and only binaries read the process environment. `toml` survives here as the value
//! model the rows are parsed and rendered through, and for a run profile's `[risk]` table
//! ([`mod@profile_risk`]) — not as a settings source.
//!
//! The history of how the layers got here (Phases 1-6 of the settings-unification program) is
//! `docs/superpowers/specs/2026-08-04-settings-unification-design.md`, and decisions 0005, 0057 and
//! 0086.
//!
//! ## Example
//!
//! ```
//! use std::collections::HashMap;
//! use vike_config::load;
//!
//! // No files, no environment: exactly the code defaults — and a warning that says so, because a
//! // process running on defaults it never chose must not be indistinguishable from one that did.
//! let settings = load(None, &HashMap::new()).unwrap();
//! assert_eq!(settings.policy.max_leverage, 1.0);
//! assert!(!settings.flags.reconcile);
//! assert_eq!(settings.warnings, vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()]);
//!
//! // A flag from the environment. Note there is no policy equivalent of this call — by design.
//! let env = HashMap::from([("VIKE_RECONCILE".to_string(), "1".to_string())]);
//! let settings = load(None, &env).unwrap();
//! assert!(settings.flags.reconcile);
//! ```

// -- the four authorities -------------------------------------------------------------------------
pub mod config;
pub mod flags;
pub mod policy;
pub mod preferences;

// -- loading: the entry points, the store, the layers, errors ------------------------------------
pub mod drift;
pub mod error;
pub mod layers;
pub mod load;
pub mod mirror;
pub mod source;

// -- arming: per-venue and per-account ceilings ---------------------------------------------------
pub mod arming;
pub mod venue_accounts;
pub mod venue_arming;
pub mod venue_mode;

// -- refusals and remedies ------------------------------------------------------------------------
pub mod redact;
pub mod remedy;
pub mod removed;

// -- reading and writing rows ---------------------------------------------------------------------
pub mod boot;
pub mod provenance;
pub mod show;
pub mod write;

// -- tables a gate holds to the tree --------------------------------------------------------------
pub mod ceilings;
pub mod consumed;
pub mod profile_risk;

pub use arming::{
    ArmingScope, ArmingSetting, CREDENTIAL_FILE_ARMING_REFUSED, LiveArming, LiveArmingVerdict,
    TRADEHUB_LIVE_ARMING, armed_for_live, armed_settings_in, refuse_credential_file_arming,
    refuse_stranded_venue_settings, stranded_venue_settings_report,
};
pub use boot::{BootLineLevel, boot_line_level, boot_lines};
pub use ceilings::{Ceiling, Site as CeilingSite, ceilings_named, shared_names};
// ⚠ `DEFAULT_BACKTEST_ADDR` joins the crate root because BOTH sides of that wire resolve it — the
// `vike-backend backtest --addr` daemon that binds and the `vike-cli backtest`/`sweep`/`walkforward`
// /`study` that dial (rulings 7 and 16 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`).
//
// ⚠ `DEFAULT_DATAHUB_ADDR` joins it for a GUARD rather than a dialler:
// `crates/vike-studio/src/backend/remote.rs`'s `no_offloading_backend_defaults_to_the_datahub`
// asserts that neither offloading backend DEFAULTS to the data daemon. A test that asked the
// question against a local copy of the address would drift with the thing it checks, so it asks
// against this authority, and that is only possible if the authority is nameable from outside.
pub use config::{Config, ConfigPatch, DEFAULT_BACKTEST_ADDR, DEFAULT_DATAHUB_ADDR};
pub use consumed::{
    CONSUMPTION, Consumer, Consumption, Reader, consumer_of, env_verdict, is_consumed,
    unconsumed_keys,
};
pub use drift::DriftedKey;
pub use error::ConfigError;
pub use flags::{
    DEAD_FLAG_KEYS, Disposition, FLAG_REGISTRY, FlagMeta, Flags, FlagsPatch, dead_flag_env_ignored,
    flag_meta,
};
pub use layers::{CliOverride, CliOverrides, EnvOverride};
pub use load::{NO_SETTINGS_DIRECTORY_WARNING, Settings, load, load_with_cli, load_with_source};
pub use mirror::{SETTING_SECTIONS, adoption_integrity, apply_rows, section_values};
pub use remedy::WriteRemedy;
pub use source::StoreLayer;

// `RatePolicyPatch` survives only as the parse shape of the refused `[rate]`
// tombstone (see `PolicyPatch::rate`), and is exported because `PolicyPatch` names it.
pub use policy::{
    DEADMAN_DISABLED_MS, DEFAULT_LINK_DEADMAN_GRACE_MS, DeadManActionSetting,
    LINK_DEADMAN_DISABLED_MS, MAX_DEADMAN_TIMEOUT_MS, MAX_LINK_DEADMAN_GRACE_MS,
    MIN_DEADMAN_TIMEOUT_MS, MIN_LINK_DEADMAN_GRACE_MS, Policy, PolicyPatch,
    RECOMMENDED_DEADMAN_TIMEOUT_MS, RatePolicyPatch,
};
pub use preferences::{Preferences, PreferencesPatch};
pub use profile_risk::{
    PROFILE_RISK_KEYS, ProfileRiskKey, RISK_TABLE, RiskKeyKind, ceiling_for, missing_keys,
    profile_name_of, profile_risk_key, risk_rows_from_profile, unknown_rows,
};
pub use provenance::{
    Description, Layer, Origin, PRECEDENCE, ResolvedSetting, SettingKey, describe,
    describe_with_source, effective_value, precedence_line,
};

pub use redact::{SECRET_SUFFIXES, is_secret, is_secret_key};
pub use removed::{
    EmptyMeaning, REMOVED_ENV, REMOVED_PROJECT_FILE, RemovedFileProbe, RemovedSetting, ValueMap,
    refuse_removed_env,
};
pub use show::{FileRow, file_rows, resolve_file_row};
pub use venue_accounts::{
    ArmedBook, SHARED_BOOK_REPORT_CAP, SharedBook, SharedBookReport, shared_book_report,
    shared_books,
};
pub use venue_arming::{ArmingBlock, VenueArming, arming_key};
pub use venue_mode::{VenueMode, VenuePolicy, legal_modes, roster_id};
pub use write::{
    LockBudget, RowPlanError, SettingsFile, refuse_credential_key, unknown_file_message,
    write_setting_row,
};
