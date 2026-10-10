//! `settings` — the verbatim registry of every environment variable this workspace reads.
//!
//! This module is DATA, not behavior: nothing in the runtime consults it. It exists so the
//! gate in `tests/settings_secrets/settings_registry.rs` can assert that the source tree and this table agree
//! in both directions — an undeclared `env::var` fails CI, and a stale row fails CI too.
//!
//! It was STEP 1 of the settings program (the capability-map playbook in `CLAUDE.md`): declare
//! today's reality byte-identically, pinning contradictions rather than fixing them. STEP 2 —
//! moving library-layer reads up into binaries and into `vike_core::RunProfile` — flips
//! [`Layer::Library`] rows one at a time; the table records the outcome, it does not drive it.
//!
//! ⚠ **The `Layer::Library` work-list is RATCHETED**, and this doc is not where its size lives.
//! `LIBRARY_PIN` in `tests/settings_secrets/settings_registry.rs` pins the SET of `(krate, name)` pairs as a
//! fixed-size array: growing it fails `library_rows_do_not_grow`, shrinking it fails
//! `library_pin_has_no_stale_rows` until the pin is updated, and the array's declared length is
//! the only machine-true count. Every number quoted in the prose below is a snapshot of the day
//! it was written — the ledger of WHICH reads moved and WHY, which is what prose is good for.
//! The ratchet exists because those numbers went stale in the worst direction: the "60" two
//! paragraphs down was true on 2026-07-29 and was 77 by 2026-08-05, grown by five PRs that each
//! had no reason to notice (#1013 +6, #1040 +1, #1043 +8, #1045 +3, #1055 −1). Adding a
//! `Layer::Library` row now requires naming which of the four families below it belongs to.
//!
//! The ratchet's FIRST shrink is the home-directory unification below: 77 → 67 on 2026-08-05.
//!
//! STEP 2, ROUND 1 flipped three (leaving 60 on the work-list at the time), each chosen because
//! the caller
//! could take the value with no new plumbing and each read had a caller-owned twin already in the
//! tree to copy:
//!
//! - `VIKE_ALERTS` — `vike_alerting::persist::path()` read it in a LIBRARY while
//!   `vike-tradehub`'s `main.rs` already resolved the same override in the BINARY and called
//!   `persist::load_path`. The library read had no caller left; the env-reading
//!   `path`/`load`/`save` family became the path-taking `load_path`/`save_path`, and the
//!   `vike-alerting` row is GONE (one row, not two, now).
//! - `VIKE_MAX_ORDER_NOTIONAL` — `vike_app_core::orders::order_entry::OrderLimits::from_env()` became the
//!   pure `from_max_notional(Option<&str>)`; `vike-desktop`'s `main.rs` (then `vike-app`) did the
//!   read. ⚠ **SUPERSEDED by
//!   settings-unification PHASE 5**, which did not lift this read but DELETED it: see below.
//! - `DATABENTO_API_KEY` — `databento::client::api_key_from_env()` became a `&str` parameter, the
//!   `databento_backfill` bin doing the read. Its sibling premium-vendor adapters in the SAME
//!   crate (`tardis`, `vike-archive`) were already shaped that way; this removed the odd one out.
//!
//! STEP 2, ROUND 2 flipped three more — the first shrink of `LIBRARY_PIN` since the ratchet landed,
//! chosen on the same test (a caller that could take the value with no new plumbing, and a
//! caller-owned twin already in the tree to copy):
//!
//! - `VIKE_PACE_BOOK` — `vike_backfill::cli::pace_book_path` now takes the environment MAP, exactly
//!   as its next-door sibling `cli::store_root` already did; `run_klines_backfill_cli` forwards it
//!   and the five `<venue>_backfill` bins pass `&std::env::vars().collect()`. Its old row claimed
//!   family 3 by citing `VIKE_HIST_STORE`'s row in the same crate — which had ALREADY been lifted on
//!   the argument family 3's own ⚠ note records as wrong. The row was `Injected` from then until
//!   docs/decisions/0094 deleted the bins, the pace file and `pace_book_path` together, and the
//!   row with them.
//! - `VIKE_STUDIO_TAB` + `VIKE_STUDIO_AUTORUN` — `StudioState::new` read both QA capture hooks in a
//!   LIBRARY, so a stale export in a dev shell forced a tool tab and an autorun on any caller. They
//!   are `StudioState::new_with_qa(store, qa_tab, qa_autorun)` parameters now; `vike-desktop`'s
//!   `main.rs`
//!   (the one production caller — the `studio_shot` example poses `right_tab` directly, and every
//!   unit test calls the env-free `new`) does the reads, so both rows moved crate as well as layer.
//!   ⚠ That same-crate `Library` -> `Binary` shape is the one with the leftover-literal trap; it is
//!   safe here only because the constructor takes the raw tab STRING and no variable name is spelled
//!   anywhere in `vike-studio`'s `src/` outside comments (which the scanner strips).
//!
//! ## Settings-unification PHASE 5 — the work-list also SHRINKS by deletion, not by lifting
//!
//! A read can leave the list a third way: the variable stops existing. Phase 5 moved the RISK
//! CEILINGS into `vike_config::Policy` (a `policy.toml` file then, the `policy.*` settings-database
//! rows now) and removed their environment overrides outright — `VIKE_MAX_ORDER_NOTIONAL` (read by
//! `vike-desktop`'s `main.rs` and by `vike-cli`'s `cmd/verbs.rs`) and
//! `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (`vike-tradehub`'s `main.rs`). Three reads, gone; nothing in
//! the workspace reads either name.
//!
//! A ceiling that can be raised from the environment is not a ceiling: a shell export, a stale
//! systemd `Environment=` line, a CI script or an inherited parent widens a live risk limit with no
//! file changed, no diff and no review. `vike_config::Policy` therefore implements neither
//! `EnvOverride` nor `CliOverride` (both sealed), so overriding it is unrepresentable rather than
//! discouraged.
//!
//! ⚠ **Their rows did not vanish — they MOVED to `vike-config`**, as `Layer::Injected` rows with a
//! `default` that says REMOVED. `vike_config::refuse_removed_env` still looks both names up in the
//! caller-supplied env map, because a set-but-ignored ceiling is the worst outcome available: the
//! operator believes a limit is armed and the process trades without one. A binary that finds
//! either variable set REFUSES TO START, naming the file and key that replace it. The rows are the
//! honest record of that — they are read, just not obeyed — and they go the day nothing refuses
//! them.
//!
//! The work-list also GROWS. Settings-unification Phase 2 (the state root, `vike_model::paths::state_path`)
//! added 8 `Library` rows: `vike_app_core::ui::workspace::persist` and `vike_studio::studio::workspace` each
//! read `VIKE_STATE_ROOT` + the platform trio to resolve where `workspace.json` /
//! `studio_workspace.json` live. Both are the SAME deferred family as family 4 below — the
//! `persist::path` load/save family and the Studio's twin of it — so they are honestly declared
//! `Library` rather than lifted mid-phase, and they lift together when that family does.
//!
//! ## The platform-variable unification
//!
//! Rows for the four platform variables (`HOME` / `USERPROFILE` / `XDG_DATA_HOME` /
//! `LOCALAPPDATA`) were spread across six crates, and were not one duplicated computation but
//! several — different fallback chains, different variable sets — each with resolution LOGIC that
//! was already shared and already pure. What was duplicated was only the `std::env::var` READ that
//! fed it. Two of the consumers are gone entirely and the third takes the map:
//!
//! - **The store root** (`XDG_DATA_HOME`→`HOME` unix / `LOCALAPPDATA`→`HOME` windows, leaf
//!   `vike-data`) — `vike_model::paths::store_path::user_data_dir`, called from four pasted copies.
//!   FIXED: `vike_model::paths::store_path::user_data_dir_from_vars` takes the map, the four callers pass
//!   their own `std::env::vars()` sweep, twelve rows became three. This is the ONLY consumer of the
//!   platform trio left in the workspace, and the only reason those three rows still exist.
//! - **Settings, credentials and state** all resolve through `<project>/settings/`
//!   (`vike_model::paths::state_path::project_settings_dir`, which `vike_secrets::store_locator` reaches by
//!   one-line delegation), which is a WALK from the working directory, not a home-directory
//!   lookup. Every platform-variable read that served them is deleted, along with the
//!   home-directory precedence they shared.
//!   ⚠ That read "its no-`vike-*`-dependency twin in `vike_secrets`" until
//!   `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
//!   2026-09-20), and both halves are false: the store crate declares `vike-model` — its property
//!   is a RANK, nothing above the vocabulary floor, machine-checked by
//!   `crates/vike-ops/tests/architecture/layer_gate.rs` — and taking that edge is what collapsed the second
//!   spelling, so there is one implementation rather than a twin.
//!
//! ⚠ **The tempting wrong move, recorded so nobody re-derives it.** Where a library still reads
//! process env, pointing it at a map-taking twin fed by `std::env::vars().collect()` IN PLACE would
//! delete rows and improve no code whatsoever: the library would still read process env, just
//! anonymously, and a sweep names no variable, so the gate could no longer see the violation it
//! exists to track. A row must never be retired by making the read unobservable. `env::vars()` in a
//! LIBRARY is the one shape this table cannot police.
//!
//! ## The settings-FILE wiring (Phase 6d) — nine rows re-keyed to `vike-config`
//!
//! Nine reads moved out of `vike-desktop`'s and `vike-tradehub`'s `main.rs` into `vike_config`'s own
//! `apply_env` over the caller-supplied map, so their rows are now `("vike-config", …)` /
//! `Layer::Injected` / `Naming::MapLookup`: `VIKE_HIST_STORE`, `VIKE_STATE_DIR`, `VIKE_STYLE`,
//! `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`, `VIKE_RECONCILE`, `VIKE_TRADEHUB_ADDR`,
//! `VIKE_TRADEHUB_CONTROL`, `VIKE_TRADEHUB_LIVE`, `VIKE_TRADEHUB_RECORD` and
//! `VIKE_TELEGRAM_CONTROL`. (`VIKE_RECONCILE` also LOST a row: both binaries now read the one
//! resolved flag — what remains is one `vike-config` row and one `vike-tradehub` row, the latter
//! for the rest of the `VIKE_RECONCILE_*` family that `build_recon_config` still parses from a
//! map. ⚠ That second row was `vike-ops`'s until 2026-09-23, when `reconcile_config` moved to the
//! daemon that is its only caller; the family did not change, only which crate performs the read.)
//!
//! ⚠ Worth being precise about why this is the table shrinking in the RIGHT direction, because
//! "the read moved into a settings crate" could describe the wrong move too. The variables are not
//! less readable and not less OBSERVABLE: each is still spelled as a `const *_ENV` in
//! `vike-config`, still harvested by this gate, still resolved `env > file > default`. What changed
//! is that the binaries take the RESOLVED value as a parameter — [`Layer::Injected`]'s target
//! shape — and, the reason the change was forced, that the FILE layer finally does something. Those
//! files validated, were accepted by `deny_unknown_fields`, and were reported by
//! `vike-cli config show` as the ORIGIN of an effective value while being read by nothing.
//! `vike_config::CONSUMPTION` and `crates/vike-config/tests/settings_are_consumed.rs` are the gate
//! that makes the next one un-shippable.
//!
//! Four families were deliberately NOT flipped (see `CLAUDE.md`'s Settings section and the
//! per-row comments in the `rows/` family files for the individual arguments):
//!
//! 1. **Deliberate process-env operator toggles.** ⚠ The default-OFF settlement pollers
//!    (`VIKE_PM_RESOLVE`, `VIKE_HL_OUTCOME`, and the `POLY_*` auto-redeem, halt and heartbeat
//!    switches) were documented here as reading the REAL process env on purpose, and they have
//!    LEFT this family: decision 0095 retired their variables (D4 — code nothing starts takes its
//!    values as PARAMETERS), so no crate reads them and their rows, in the config family file, are
//!    `vike-config`'s `Layer::Injected` refusal rows, `default` reading "REMOVED — refused at
//!    startup". The venue
//!    `{VENUE}_MAINNET` flags used to be named here too; the same decision deleted them outright
//!    rather than leaving them a toggle, so they left this family entirely rather than moving to
//!    another — and `vike-mount`'s own module doc now states the opposite of what this bullet used
//!    to cite it for: no `{VENUE}_MAINNET` variable is read anywhere in that tree any more.
//!    ⚠ **"Deliberate operator toggle" is a claim about WHERE the read is, never about whether
//!    anything runs it.** Before decision 0095 retired them, the five settlement-poller variables
//!    were each read inside a poller no composition root constructs, so exporting one changed
//!    nothing — `vike_config::CONSUMPTION` still carries the gated row per `flags.*` key
//!    (`Reader::Uncalled`, naming the entry point that must stay uncalled) and `vike-cli config
//!    show` prints the verdict. `VIKE_RECORD_CHAINS` is the same shape one family down (a recorder
//!    constructor with no call site at all), and its variable is still read there.
//!    ⚠ `VIKE_RECONCILE`'s exact-`"1"` master gate and `VIKE_TRADEHUB_CONTROL` were in this family
//!    and have LEFT it. The argument that put them here is intact — both are still read from the
//!    REAL process env, because the binary hands `std::env::vars()` to `vike_config::load`, never
//!    the credentials map — and what changed is only that the environment is no longer the sole
//!    layer able to set them.
//! 2. **Process-wide resource knobs consulted deep inside a fold** — `VIKE_PIN_CORES`
//!    (`vike_exec::affinity::pin_current_thread`, called from ~15 thread spawns across the bridge
//!    crates), `VIKE_SWEEP_THREADS` (rayon pool construction inside `map_bounded`). Lifting these
//!    needs a caller-owned process-wide handle (an `init(spec)` + `OnceLock`, or a config threaded
//!    through every spawn), which is a design decision, not a mechanical move.
//! 3. **Shared BIN glue that merely lives outside `main.rs`** — `vike-cli`'s `src/cmd/*`
//!    subcommand bodies. [`Layer`] is computed from the FILE PATH, so a bin-only helper in a
//!    bin-heavy crate scores `Library` even though every caller is a binary. These rows are honest
//!    about WHERE the read is; they are not honest about whether it is a violation, and that is a
//!    gate limitation, not a code one.
//!
//!    ⚠ **This family used to name `binutil::store_root` (vike-backtest) and `cli::store_root`
//!    (vike-backfill) too, on an argument that turned out to be wrong.** The argument was that
//!    "fixing" them would duplicate the `VIKE_HIST_STORE` read across a dozen bins — precisely the
//!    drift `cli.rs`'s module doc says it was created to remove. It assumed the only way to lift a
//!    read is to move the *variable lookup* to each caller. The third shape is to move the
//!    *environment* instead: the bins pass `&std::env::vars().collect()` — one expression, no
//!    variable named — and `store_root` keeps the whole precedence, including which key it looks
//!    up. Nothing duplicated, the drift fix intact, and both helpers are now pure
//!    (`Layer::Injected`), which also removed the process-env MUTATION their unit tests needed. See
//!    the `HOME`/`XDG_DATA_HOME`/`LOCALAPPDATA` rows for the other half of the same lift.
//! 4. **Reads whose lift needs a value threaded through several layers** —
//!    `vike_core::journal_config_from_env` (6 call sites across 4 crates, one of them a library
//!    composition root — deleted by decision 0111, its last callers handing in the resolved
//!    journal instead); `vike_app_core::ui::workspace::persist::path` (a whole
//!    load/save/layout family, ~10 call sites in the CI-invisible `vike-desktop`); the
//!    `VIKE_RECORD_*` recorders; and `vike-log`'s four `init` reads. Each is a separate PR with its
//!    own argument. (`VIKE_HALT_FILE` stood here too, argued as "wiring `ExecActor`'s
//!    `with_halt_path` seam means touching every venue mount" — decision 0099 did exactly that
//!    wiring, and then retired the variable outright rather than lifting it: it is a REMOVED row
//!    now.)
//!
//! ## Decision 0111 — the environment configures nothing but where the database is
//!
//! `docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md` makes every setting
//! a database row. Its end state is a gate, and this table carries the column that gate needs:
//! [`Medium`], WHERE a row's value comes from. `Layer` cannot answer that (a `Layer::Injected` map
//! lookup may be handed the process environment or the credential map), so it is declared per row.
//!
//! The policy half lives beside `LIBRARY_PIN`, in
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`, for the reason that pin gives (this
//! table is data, a pin is a judgement): `ENV_ALLOWLIST` names what may stay in the environment
//! (decision 0111's verdict 2), and `PROCESS_ENV_PIN` holds every `Scope::Vike` process-environment
//! row that is neither allowlisted nor refused yet. That pin may shrink and never grow; each phase of
//! 0111 shrinks it, and the last one deletes it.
//!
//! Mirrors the `vike_model::VENUES` roster precedent: the roster lives in `src`, the
//! exhaustiveness gate lives in `tests`.

mod grid;
mod rows;

/// Who owns the variable's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `VIKE_*` — our own knob.
    Vike,
    /// A venue gate or credential (`POLY_*`, `BINANCE_*`, `DUKASCOPY_*`, …).
    Venue,
    /// Third-party or OS-provided (`RUST_LOG`, `JAVA_HOME`, `HOME`, `CARGO_*`).
    External,
}

/// WHERE the read happens today, and therefore whether it is already correct.
///
/// This is the column the whole table exists for. `Library` rows are the STEP-2 work-list —
/// a library calling `env::var` reads global state its caller cannot see or override.
/// `Injected` rows are the STEP-2 TARGET STATE, already achieved: a pure parser over a map
/// the caller supplies (`vike_tradehub::reconcile_config`, the venue `config.rs` loaders,
/// `vike_bridge_core::key_permissions`). `Binary` rows are the third correct shape — a
/// `main.rs` or `src/bin/*.rs` reading process env directly, which is where env reads belong.
///
/// A variable can hold rows at several layers at once, one per reading crate (the table is
/// keyed on `(name, krate)`). No row is implied by another: a `std::env::vars()` sweep names
/// no variable, so it never produces a row — see the design note below `Naming`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// `main.rs` or `src/bin/*.rs` — the correct place to read process env.
    Binary,
    /// A pure parser reading a caller-supplied map (`vars.get("NAME")`), never process env.
    /// Already correct; nothing to do in STEP 2.
    Injected,
    /// A direct `env::var` under `src/` outside a binary — the STEP-2 work-list.
    Library,
    /// `tests/` or a `#[cfg(test)]` block — smoke-test gates, fine as-is.
    TestOnly,
    /// `build.rs` — compile-time only.
    BuildScript,
}

/// HOW the variable's name reaches the code that consumes it, so the gate can locate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    /// `env::var("VIKE_THING")` — the name is a literal at the call site.
    Literal,
    /// `env::var(THING_ENV)` — names the `const THING_ENV: &str` it resolves through.
    Konst(&'static str),
    /// `vars.get("VIKE_THING")` on a caller-supplied map — pairs with [`Layer::Injected`].
    MapLookup,
    /// The name is computed (`format!`) or a parameter; requires an allowlist entry.
    Dynamic,
}

/// WHERE the value a row names COMES FROM: the column decision 0111
/// (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`, verdict 7) needs
/// to tell a process-environment read from every other kind.
///
/// It is orthogonal to [`Layer`] and [`Naming`]. Those two say where the READ sits and how its name
/// is spelled, and the scanner measures both. Neither can say which MAP a `vars.get("NAME")`
/// consults: the same pure parser can be handed the process environment by one root and the
/// credential map by another. This column is that answer, declared per row.
///
/// The one rule for a row whose reader is fed from more than one place: if ANY production path
/// hands it a process-environment value, the row is [`Medium::ProcessEnv`]. The question the gate
/// asks is "can the environment configure this?", and one root saying yes is enough.
///
/// What holds each answer to something other than the row itself is
/// `crates/vike-ops/tests/settings_secrets/settings_registry/medium.rs`: a direct `env::var` read
/// is `ProcessEnv` by definition, a `Refused` row is exactly a `vike_config::REMOVED_ENV` row, a
/// `NodeKeyMap` row names one of `vike_model::credential_keys::PLATFORM_KEYS`, a `CompileTime` row
/// is baked by its crate's build script, and a `VIKE_` name may claim `CredentialMap` only as a
/// settable store key or a declared flag fold. What no check can prove is WHICH map a `MapLookup`
/// site is handed at run time; a row that says `CredentialMap` while its caller passes the process
/// sweep is a review catch, the same limit as `MAP_LOOKUP_PROVEN`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    /// The process environment: a direct `env::var`, or a key of the `std::env::vars()` sweep a
    /// root hands down. Also the build script's own environment for a `Layer::BuildScript` read.
    ProcessEnv,
    /// A key of the CREDENTIAL map (the settings database's `credential` table, as
    /// `vike_bridge_core::credentials` and `vike_secrets::resolve_project*` return it), including
    /// a flag a daemon folds into that map under this name.
    CredentialMap,
    /// A key of the NODE-KEY map `vike_secrets::resolve_node_keys` returns (the `node_key` table).
    NodeKeyMap,
    /// Baked in at compile time: a build script's `cargo:rustc-env`, read back with `env!()`. No
    /// running process reads it.
    CompileTime,
    /// Looked up ONLY to refuse it: a `vike_config::REMOVED_ENV` name, which
    /// `vike_config::refuse_removed_env` refuses at startup.
    Refused,
    /// No value is read under this name at all: an HTTP header the code WRITES, a variable it puts
    /// into a child process's environment, or a fixture name planted to prove it stays unread. The
    /// scanner sees the literal, so the registry still declares it; this says it configures nothing.
    NotRead,
}

// DESIGN NOTE (settled across Task 1's four review rounds): a row records where a variable is
// NAMED. Two consequences, both learned from real counterexamples in this tree:
//
// 1. A `std::env::vars()` sweep names NOTHING — it copies the environment anonymously — so a
//    sweep can never justify a row. An earlier `Naming::WholeEnv` variant was added and then
//    deleted for exactly this reason; do not re-add it. The binaries that sweep are documented
//    in CLAUDE.md prose instead.
// 2. One variable CAN be named at both kinds of site. `poly_reconcile_enabled`
//    (bridges/polymarket/src/recon_client.rs) reads `std::env::var(POLY_RECONCILE_ENV)` and
//    falls back to `vars.get(POLY_RECONCILE_ENV)` — the layered process-env-else-`.env` idiom.
//    TIE-BREAK: `naming` records the DIRECT read (`Literal`/`Konst`), because that is the one
//    the caller cannot override, and `layer` records `Library` for the same reason. The
//    map-lookup half is a fallback, not a second row.

/// One declared environment variable.
#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// The variable name as it appears in the process environment.
    pub name: &'static str,
    /// The crate that reads it, e.g. `"vike-core"` or `"bridges/polymarket"`.
    pub krate: &'static str,
    pub scope: Scope,
    pub layer: Layer,
    pub naming: Naming,
    /// Where the value comes from (process environment, credential map, …): see [`Medium`].
    pub medium: Medium,
    /// The documented default when unset, verbatim. `""` means "unset = feature off".
    pub default: &'static str,
}

/// Every environment variable the workspace reads, one row per `(name, krate)` pair — EXCEPT the
/// generated credential key grid, which is the registry's second table
/// (`crates/vike-ops/src/settings/grid.rs`'s `GENERATED_KEY_GRID`). ⚠ **Iterate [`all_settings`],
/// never this table alone**: it chains the two, and a reader of `SETTINGS` by itself has silently
/// dropped every composed `Scope::Venue` credential key.
///
/// Generated from the real `tests/settings_secrets/settings_registry.rs` walk (see
/// `docs/superpowers/plans/2026-07-27-settings-registry.md` for the generation method and
/// design history), not hand-transcribed: the walk found every `env::var`/`env::var_os` call
/// site and every `vars.get("NAME")` map lookup across the whole `crates/` tree, resolved each
/// through the crate-wide `const` table, and this table declares the result.
///
/// `crates/vike-model/src/scan.rs` and this very file are excluded from the gate's OBSERVATION step
/// (`tests/settings_secrets/settings_registry.rs`'s `LITERAL_HARVEST_EXCLUDED`) — neither contains a real env
/// read, but both are walked like any other `.rs` file, and without the exclusion the scanner
/// would report its own search-pattern strings, `#[cfg(test)]` fixtures, and (for this file) every
/// row's own `name` field as spurious observed reads. Every row in the family files is a real one;
/// none exists only to satisfy the scanner observing itself.
///
/// Keyed on `(name, krate)`, NOT on `name`. Several variables are read from more than one
/// crate with different fallbacks — `VIKE_HIST_STORE` alone is read in `vike-config`,
/// `vike-backtest`, `vike-backfill`, `vike-datahub` and `vike-studio` with different
/// default chains. Each reading crate gets its own row so the per-crate default and evidence
/// survive; collapsing them would hide a real inconsistency behind a single made-up default. A
/// handful of purely self-referential/fixture-noise sightings (the scanner or a redaction unit
/// test mentioning a name that is genuinely read only by SOME OTHER crate) are omitted rather
/// than given a misleading row — see the plan doc's drop list.
///
/// Three limitations of the gate this table is checked against, kept as the history of what each one
/// was and numbered as `crates/vike-ops/CLAUDE.md` cites them (so the numbers stay put). ⚠ **(1) and
/// (2) are CLOSED** — the ⚠ paragraphs below the list say how, and what residue each left; only (3)
/// and (2)'s residue, which that page calls (2b), are still open. Read it before trusting any
/// direction's silence too far:
///
/// 1. **Per-crate exhaustiveness is one-directional.** The gate proves "every OBSERVED read is
///    declared for SOME crate" (direction 1, name-level) and "every DECLARED row's crate genuinely
///    reads it" (`every_declared_variable_is_read`, per-row) — but nothing fails when a crate that
///    does NOT yet have a row for name `X` starts reading `X` for the first time, as long as `X`
///    is already declared under a DIFFERENT krate (direction 1 is satisfied by the pre-existing
///    row, so the gate stays green). Rows can therefore drift out of date (a new reading crate
///    silently uncovered) without any test failing; this table's accuracy for the "one row per
///    crate" invariant rests on the generation method described above, not on a standing machine
///    check of it.
/// 2. **Computed map keys are an undeclared blind spot, and unlike a computed `env::var` argument
///    there is NO allowlist for them.** `vike_bridge_core::credentials::load_credentials_from`
///    builds every credential key with `format!("{prefix}_API_KEY")` / `_API_SECRET` /
///    `_API_PASSPHRASE` and then `vars.get(name)` — the scanner sees only literals and resolvable
///    `const`s, so most of the `{VENUE}_{TIER}_API_*` grid (e.g. `OKX_DEMO_API_SECRET`) is read
///    but never declared here. The same is true of `attribution_code_from`'s
///    `format!("{VENUE}_BROKER_CODE")` / `_BUILDER_CODE` keys — `OKX_BROKER_CODE`/
///    `POLYMARKET_BUILDER_CODE`/`DERIBIT_BROKER_CODE` happen to have rows ONLY because a
///    `#[cfg(test)]` fixture in `credentials.rs` also spells them as bare literals (an incidental,
///    not a structural, source of observability); `BYBIT_BROKER_CODE`/`BINANCE_BROKER_CODE`/
///    `HYPERLIQUID_BUILDER_CODE`/`ASTER_BUILDER_CODE` have no such fixture and so have no row at
///    all, undetectably. `DYNAMIC_ALLOWLIST` cannot help here — it allowlists a call site the
///    scanner FOUND but could not resolve; a computed `.get(key)` on a map is never even
///    recognised as a candidate site to begin with. This family is a genuine, currently
///    unenumerable gap in the registry — declared here rather than silently implied.
/// 3. **Naming/Layer conventions can look inconsistent for a row observed only via a test
///    fixture.** The attribution-code rows (`*_BROKER_CODE` / `*_BUILDER_CODE`, spelled in the
///    generated key grid) are `Layer::Injected` (matching how `attribution_code_from` reads them
///    in real code) even though the only site the SCANNER
///    actually resolved them from is a `#[cfg(test)]` module in the SAME file — `layer_for` has
///    no notion of "this specific literal sighting sits inside a test block" for the raw
///    literal-sweep path (only `SRC_TEST_MODULE_OVERRIDES`, and only for DIRECT `env::var` reads,
///    covers that). The declared `Layer::Injected` is still the truthful answer for how the
///    variable is ACTUALLY read in production; it just isn't provable from this one incidental
///    sighting alone.
///
/// ⚠ Limitation 1 above is CLOSED for a PROVEN read. `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `every_read_variable_is_declared` runs in two tiers now: a direct `env::var` call site, a resolved
/// `.get(KEY)` site or the generated key grid is PROOF and demands a row from THAT crate, keyed on
/// `(name, krate)`; only a name known from the loose literal sweep alone is still judged at name
/// level — a declared, tested exemption (`the_per_crate_demand_rests_on_proof` measures it), not a
/// silent gap.
///
/// ⚠ Limitation 2 above is the one that CHANGED. Read it as history: the `{VENUE}_{TIER}_API_*` and
/// `{VENUE}_{BROKER,BUILDER}_CODE` families are no longer undeclared. They are enumerable data now —
/// `vike_model::credential_keys`' `lookup_keys` folds the suffix/tier tables over
/// `vike_model::VENUES` — and THE GENERATED KEY GRID, the second table (`all_settings` chains it after
/// this one), declares every one of them, gated by
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `every_generated_key_is_declared`
/// in both directions. What survives of limitation 2 is the narrower residue it always contained:
/// the loose literal sweep still cannot PROVE an ordinary `MapLookup` row's read, which
/// `MAP_LOOKUP_PROVEN` measures rather than assumes.
///
/// ⚠ Limitation 3's EXAMPLE changed with it; the limitation did not. There is no longer a trio of
/// attribution-code rows resting on one `#[cfg(test)]` fixture: the two MECHANISED ones
/// (`OKX_BROKER_CODE`, `POLYMARKET_BUILDER_CODE`) are enumerated by the grid and no longer depend
/// on the fixture at all, and the third — `DERIBIT_BROKER_CODE` — is GONE. Deribit is
/// `AttributionMechanic::None`, so `attribution_code_from` returns before it builds a key and that
/// name provably cannot be read; the row existed only because the fixture spelled the literal,
/// which made `vike-cli config show` report an unread key as a real setting with a real source.
/// The fixture composes the name now (`crates/vike-bridge-core/src/credentials/attribution_code_tests.rs`'s
/// `absent_or_invalid_or_unmechanized_is_none`) and the row is deleted, which is also what keeps
/// this table consistent with the other seven unmechanised venues, none of which ever had one. The
/// CLASS in limitation 3 is unchanged and still populated — a bridge crate's own
/// `tests/config_env.rs` fixture is the only sighting many per-venue rows have.
pub const SETTINGS: &[&[Setting]] = &[
    rows::bridges::ROWS,
    rows::research::ROWS,
    rows::config::ROWS,
    rows::connections::ROWS,
    rows::daemons::ROWS,
    rows::gui::ROWS,
    rows::platform::ROWS,
];

/// Every row of the registry, in one pass: [`SETTINGS`] (the hand-written rows) and then THE
/// GENERATED KEY GRID (`crates/vike-ops/src/settings/grid.rs`'s `GENERATED_KEY_GRID`), both
/// `(name, krate)`-keyed with the same element type.
///
/// ⚠ **This is how the registry is read, and the only way.** The grid is every composed
/// `Scope::Venue` credential and attribution-code key, so a consumer that walks `SETTINGS` alone
/// compiles, passes and silently loses all of them. `vike-cli config show`, the Connections
/// editor's completeness gate and every check in `crates/vike-ops/tests/settings_secrets/settings_registry.rs` read
/// this iterator for that reason; a new consumer does the same.
pub fn all_settings() -> impl Iterator<Item = &'static Setting> {
    SETTINGS.iter().flat_map(|rows| rows.iter()).chain(grid::GENERATED_KEY_GRID.iter())
}

#[cfg(test)]
mod tests;
