//! `vike-config` — the TYPED settings model and its layered loader.
//!
//! Phases 1, 4 and 5 of the settings-unification program
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`).
//!
//! Phase 1 built the four types and the layered [`load`]. Phase 4 made [`Flags`] cover the
//! workspace's real operator toggles and gave every one of them an OWNER and a REVIEW DATE
//! ([`FLAG_REGISTRY`], gated by `tests/flag_registry.rs`) — a flag with no owner is a flag nobody
//! will ever delete. Neither phase rewired a call site.
//!
//! **Phase 5 is where that changed, and it is the program's one BREAKING phase.**
//! [`Policy::max_notional_per_order`] is now the ONLY source of the per-order notional ceiling:
//! `VIKE_MAX_ORDER_NOTIONAL` (`vike-app`, `vike-cli`) and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL`
//! (`vike-tradehub`) are no longer read by anything, and a process that finds either one SET
//! refuses to start, naming the file and key that replace it — see [`refuse_removed_env`].
//!
//! **Phase 6 is where the FILES started doing something.** Until it, a `config.toml` /
//! `preferences.toml` / `flags.toml` validated, was accepted by `deny_unknown_fields`, and was
//! reported by `vike-cli config show` as the ORIGIN of an effective value while being read by
//! nothing at all — positive confirmation of something false, which is strictly worse than an
//! unimplemented feature. [`CONSUMPTION`] is the answer and the ratchet: one row per
//! `config`/`preferences`/`flags` key naming the file and the read that consumes it, or a written
//! admission that nothing does, gated by `crates/vike-config/tests/settings_are_consumed.rs` and
//! printed per key by `config show`'s `READ` column. The keys whose readers still live in a venue
//! adapter or a recorder are honestly marked unread; they move a flag at a time, together with the
//! env read they replace.
//!
//! ## The problem, in one sentence
//!
//! Settings live in eight mechanisms with no root, no precedence order, and no way to ask where a
//! value came from — and the worst consequence is not the sprawl, it is that **`max_leverage` is
//! exactly as env-overridable as a chart colour**.
//!
//! ## The answer: split by AUTHORITY, not by subsystem
//!
//! Four types, one per level of who is allowed to change a value and from where:
//!
//! | type | holds | who writes it | overridable by |
//! |---|---|---|---|
//! | [`Policy`] | hard ceilings — leverage, order notional, market-slippage band, halt admission, **per-venue arming** | org/admin | **file only** |
//! | [`Config`] | deployment — store paths, log dir, listen addresses | deployment | file, env, CLI |
//! | [`Preferences`] | taste + tuning — log levels, chart style, sweep threads | user | file, env, CLI |
//! | [`Flags`] | toggles — reconcile, poly exec, control surfaces, recorders; each with an owner + review date | operator | file, env, CLI |
//!
//! and one order, applied to all of them:
//!
//! ```text
//! code defaults -> <project>/settings/*.toml -> env -> CLI
//! ```
//!
//! ⚠ **FOUR files, one authority each, all inside `<project>/settings/` — there is no fifth.** A
//! `<project>/vike.toml` override layer sat between the settings files and the environment, and it
//! is REMOVED: a file one level ABOVE the settings directory, overriding two of the four files
//! inside it, reintroduces the "which file won?" question that consolidating everything into one
//! directory exists to answer. A present one is REFUSED at startup rather than ignored — see
//! [`REMOVED_PROJECT_FILE`] and [`crate::removed`], which own the message. [`PRECEDENCE`] carries
//! the two gates that hold every layer this crate still advertises to a proof of reach.
//!
//! ## What is enforced by the compiler
//!
//! **[`Policy`] has no `from_env`, no `apply_env` and no `apply_cli` — not on the type, not on any
//! trait it implements.** Overriding a ceiling from the environment is *unrepresentable*, not
//! discouraged: `settings.policy.apply_env(&env)` is a compile error. [`EnvOverride`] and
//! [`CliOverride`] are sealed traits implemented by exactly the other three types, so the table
//! above cannot be widened from outside this crate either.
//!
//! A ceiling you can override with an environment variable is not a ceiling. The failure mode is
//! silent — a stale systemd unit or an inherited shell raises the limit with no file changed, no
//! diff, no review, and a run that looks completely normal — so it is worth removing the class
//! rather than documenting it. [`layers`] carries the full argument.
//!
//! ## What else the loader refuses to be quiet about
//!
//! - **Unknown keys are rejected BY NAME** (`deny_unknown_fields` on every file struct). A typo'd
//!   setting that silently does nothing is worse than an error — the operator sets a ceiling,
//!   sees no complaint, and does not have it.
//! - ⚠ **…and `deny_unknown_fields` does NOT reach inside a MAP.** It governs a struct's FIELD
//!   names; serde treats a map's keys as data. [`Policy::venues`] is the one map in the model, so
//!   its venue ids are checked by hand against [`vike_model::VENUES`] in [`Policy::apply`] and an
//!   unknown one is refused with the roster in the message. The rule the split draws for the next
//!   map-shaped key: a value type that is an ENUM gets its refusal from serde for free; a KEY space
//!   never does.
//! - **Every error names the FILE and the KEY.** `"invalid config"` is useless;
//!   `"policy.toml: market_slippage = 0.9 exceeds the allowed maximum 0.05"` is the bar. See
//!   [`ConfigError`].
//! - **A truthy typo on a flag is an error**, where today `VIKE_RECONCILE=true` silently reads
//!   false. Same exact-`"1"` idiom, but the operator is told. See [`flags`].
//! - **A key that was REMOVED is refused by name, not ignored** — [`PolicyPatch::max_total_exposure`],
//!   [`PolicyPatch::rate`] and [`PreferencesPatch::rate_utilization`] are all tombstones whose
//!   errors say where the concept lives now. An operator who wrote a limit believes they have it;
//!   letting `deny_unknown_fields` answer "unknown field" tells them only that the key is wrong.
//!   [`refuse_removed_env`] is the same rule for variables.
//!
//! ⚠ There is currently NO policy-bounds-preference clamp. The mechanism (policy sets the bound, a
//! preference sets the value inside it) is still the taxonomy's spine, but its only instance was
//! `rate.max_utilization` over `rate_utilization` — a ceiling on a value nothing read — and both
//! are gone. See [`preferences`]' module doc.
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
//! The env map and both file roots are PARAMETERS. This crate never reads `std::env`, never walks
//! for a directory and never expands `~` — libraries take configuration as arguments, and only
//! binaries read the process environment (CLAUDE.md's settings rule, and the `vike_tradehub_client::auth::from_vars`
//! precedent). Reading the TOML files themselves IS this crate's job, since the "which file was
//! that?" bookkeeping is exactly what [`ConfigError`] exists to get right.
//!
//! ## Why TOML
//!
//! Because these settings need comments carrying their rationale ("150 ms because the fapi
//! ceiling is 2400"), because it is typed, and because the workspace already speaks it.
//! **Not YAML** — implicit typing (`NO` becomes false, `1.10` becomes `1.1`) is a hazard in a file
//! that configures order routing. Program-written state stays JSON and lives under the settings
//! directory's own `state/`; it is not this crate's concern.
//!
//! ## Example
//!
//! ```
//! use std::collections::HashMap;
//! use vike_config::load;
//!
//! // No files, no environment: exactly the code defaults.
//! let settings = load(None, &HashMap::new()).unwrap();
//! assert_eq!(settings.policy.max_leverage, 1.0);
//! assert!(!settings.flags.reconcile);
//! assert!(settings.warnings.is_empty());
//!
//! // A flag from the environment. Note there is no policy equivalent of this call — by design.
//! let env = HashMap::from([("VIKE_RECONCILE".to_string(), "1".to_string())]);
//! let settings = load(None, &env).unwrap();
//! assert!(settings.flags.reconcile);
//! ```

pub mod arming;
pub mod boot;
pub mod config;
pub mod consumed;
pub mod error;
pub mod flags;
pub mod layers;
pub mod load;
pub mod policy;
pub mod preferences;
pub mod provenance;
pub mod redact;
pub mod removed;
pub mod show;
pub mod venue_accounts;
pub mod venue_arming;
pub mod venue_mode;
pub mod write;

pub use arming::{
    ArmingScope, ArmingSetting, CREDENTIAL_FILE_ARMING_REFUSED, LiveArming, LiveArmingVerdict,
    TRADEHUB_LIVE_ARMING, armed_for_live, armed_settings_in, refuse_credential_file_arming,
};
pub use boot::boot_lines;
pub use config::{Config, ConfigPatch};
pub use consumed::{CONSUMPTION, Consumer, Consumption, consumer_of, is_consumed, unconsumed_keys};
pub use error::ConfigError;
pub use flags::{Disposition, FLAG_REGISTRY, FlagMeta, Flags, FlagsPatch, flag_meta};
pub use layers::{CliOverride, CliOverrides, EnvOverride};
pub use load::{Settings, load, load_with_cli, settings_files};
// `RatePolicy` is GONE; `RatePolicyPatch` survives only as the parse shape of the refused `[rate]`
// tombstone (see `PolicyPatch::rate`), and is exported because `PolicyPatch` names it.
pub use policy::{
    DEADMAN_DISABLED_MS, DeadManActionSetting, MAX_DEADMAN_TIMEOUT_MS, MIN_DEADMAN_TIMEOUT_MS,
    Policy, PolicyPatch, RECOMMENDED_DEADMAN_TIMEOUT_MS, RatePolicyPatch,
};
pub use preferences::{Preferences, PreferencesPatch};
pub use provenance::{
    Description, FileStatus, Layer, Origin, PRECEDENCE, ResolvedSetting, SettingKey, describe,
    precedence_line,
};
pub use redact::{SECRET_SUFFIXES, is_secret, is_secret_key};
pub use removed::{
    REMOVED_ENV, REMOVED_PROJECT_FILE, RemovedFileProbe, RemovedSetting, refuse_removed_env,
};
pub use show::{FileRow, file_rows, resolve_file_row};
pub use venue_accounts::{ArmedBook, SharedBook, shared_books};
pub use venue_arming::{ArmingBlock, VenueArming, arming_key};
pub use venue_mode::{VenueMode, VenuePolicy, legal_modes, roster_id};
pub use write::{
    SettingsFile, SettingsWrite, SettingsWriteError, set_setting, unknown_file_message,
    validate_settings_text,
};
