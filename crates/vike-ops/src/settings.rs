//! `settings` — the verbatim registry of every environment variable this workspace reads.
//!
//! This module is DATA, not behavior: nothing in the runtime consults it. It exists so the
//! gate in `tests/settings_registry.rs` can assert that the source tree and this table agree
//! in both directions — an undeclared `env::var` fails CI, and a stale row fails CI too.
//!
//! It was STEP 1 of the settings program (the capability-map playbook in `CLAUDE.md`): declare
//! today's reality byte-identically, pinning contradictions rather than fixing them. STEP 2 —
//! moving library-layer reads up into binaries and into `vike_core::RunProfile` — flips
//! [`Layer::Library`] rows one at a time; the table records the outcome, it does not drive it.
//!
//! ⚠ **The `Layer::Library` work-list is RATCHETED**, and this doc is not where its size lives.
//! `LIBRARY_PIN` in `tests/settings_registry.rs` pins the SET of `(krate, name)` pairs as a
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
//! - `VIKE_MAX_ORDER_NOTIONAL` — `vike_app_core::order_entry::OrderLimits::from_env()` became the
//!   pure `from_max_notional(Option<&str>)`; `vike-app`'s `main.rs` did the read. ⚠ **SUPERSEDED by
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
//!   the argument family 3's own ⚠ note records as wrong. The row is `Injected` now.
//! - `VIKE_STUDIO_TAB` + `VIKE_STUDIO_AUTORUN` — `StudioState::new` read both QA capture hooks in a
//!   LIBRARY, so a stale export in a dev shell forced a tool tab and an autorun on any caller. They
//!   are `StudioState::new_with_qa(store, qa_tab, qa_autorun)` parameters now; `vike-app`'s `main.rs`
//!   (the one production caller — the `studio_shot` example poses `right_tab` directly, and every
//!   unit test calls the env-free `new`) does the reads, so both rows moved crate as well as layer.
//!   ⚠ That same-crate `Library` -> `Binary` shape is the one with the leftover-literal trap; it is
//!   safe here only because the constructor takes the raw tab STRING and no variable name is spelled
//!   anywhere in `vike-studio`'s `src/` outside comments (which the scanner strips).
//!
//! ## Settings-unification PHASE 5 — the work-list also SHRINKS by deletion, not by lifting
//!
//! A read can leave the list a third way: the variable stops existing. Phase 5 moved the RISK
//! CEILINGS into `<vike home>/policy.toml` (`vike_config::Policy`) and removed their environment
//! overrides outright — `VIKE_MAX_ORDER_NOTIONAL` (read by `vike-app`'s `main.rs` and by
//! `vike-cli`'s `cmd/verbs.rs`) and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (`vike-tradehub`'s
//! `main.rs`). Three reads, gone; nothing in the workspace reads either name.
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
//! The work-list also GROWS. Settings-unification Phase 2 (the state root, `vike_model::state_path`)
//! added 8 `Library` rows: `vike_app_core::workspace::persist` and `vike_studio::workspace` each
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
//!   `vike-data`) — `vike_model::store_path::user_data_dir`, called from four pasted copies.
//!   FIXED: `vike_model::store_path::user_data_dir_from_vars` takes the map, the four callers pass
//!   their own `std::env::vars()` sweep, twelve rows became three. This is the ONLY consumer of the
//!   platform trio left in the workspace, and the only reason those three rows still exist.
//! - **Settings, credentials and state** all resolve through `<project>/settings/`
//!   (`vike_model::state_path::project_settings_dir` and its zero-dependency twin in
//!   `vike_secrets`), which is a WALK from the working directory, not a home-directory lookup.
//!   Every platform-variable read that served them is deleted, along with the home-directory
//!   precedence they shared.
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
//! Nine reads moved out of `vike-app`'s and `vike-tradehub`'s `main.rs` into `vike_config`'s own
//! `apply_env` over the caller-supplied map, so their rows are now `("vike-config", …)` /
//! `Layer::Injected` / `Naming::MapLookup`: `VIKE_HIST_STORE`, `VIKE_STATE_DIR`, `VIKE_STYLE`,
//! `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`, `VIKE_RECONCILE`, `VIKE_TRADEHUB_ADDR`,
//! `VIKE_TRADEHUB_CONTROL`, `VIKE_TRADEHUB_LIVE`, `VIKE_TRADEHUB_RECORD` and
//! `VIKE_TELEGRAM_CONTROL`. (`VIKE_RECONCILE` also LOST a row: both binaries now read the one
//! resolved flag, so `vike-tradehub` no longer reads the name at all — what remains is one
//! `vike-config` row and one `vike-ops` row, the latter for the rest of the `VIKE_RECONCILE_*`
//! family that `build_recon_config` still parses from a map.)
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
//! per-row comments below for the individual arguments):
//!
//! 1. **Deliberate process-env operator toggles.** The venue `{VENUE}_MAINNET` flags and the
//!    default-OFF settlement pollers (`VIKE_PM_RESOLVE`, `VIKE_HL_OUTCOME`, `POLY_*`) are
//!    documented as reading the REAL process env on purpose — see [`crate::reconcile_config`]'s
//!    module doc for why a shell-exported flag must not come from the credentials `.env` map, and
//!    `vike-mount`'s env-boundary note for why the `_MAINNET` reads stay at their own site.
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
//!    composition root); `vike_app_core::workspace::persist::path` (a whole
//!    load/save/layout family, ~10 call sites in the CI-invisible `vike-app`); `VIKE_HALT_FILE`
//!    (`ExecActor` already has a `with_halt_path` seam, but wiring it means touching every venue
//!    mount); the `VIKE_RECORD_*` recorders; and `vike-log`'s four `init` reads. Each is a
//!    separate PR with its own argument.
//!
//! Mirrors the `vike_model::VENUES` roster precedent: the roster lives in `src`, the
//! exhaustiveness gate lives in `tests`.

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
/// the caller supplies (`vike_ops::reconcile_config`, the venue `config.rs` loaders,
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
    /// The documented default when unset, verbatim. `""` means "unset = feature off".
    pub default: &'static str,
}

/// Every environment variable the workspace reads, one row per `(name, krate)` pair.
///
/// Generated from the real `tests/settings_registry.rs` walk (see
/// `docs/superpowers/plans/2026-07-27-settings-registry.md` for the generation method and
/// design history), not hand-transcribed: the walk found every `env::var`/`env::var_os` call
/// site and every `vars.get("NAME")` map lookup across the whole `crates/` tree, resolved each
/// through the crate-wide `const` table, and this table declares the result.
///
/// `scan.rs` and this very file are excluded from the gate's OBSERVATION step
/// (`tests/settings_registry.rs`'s `LITERAL_HARVEST_EXCLUDED`) — neither contains a real env
/// read, but both are walked like any other `.rs` file, and without the exclusion the scanner
/// would report its own search-pattern strings, `#[cfg(test)]` fixtures, and (for this file) every
/// row's own `name` field as spurious observed reads. Every row below is a real one; none exists
/// only to satisfy the scanner observing itself.
///
/// Keyed on `(name, krate)`, NOT on `name`. Several variables are read from more than one
/// crate with different fallbacks — `VIKE_HIST_STORE` alone is read in `vike-app`,
/// `vike-backtest`, `vike-backfill`, `vike-datahub` and `vike-studio` with different
/// default chains. Each reading crate gets its own row so the per-crate default and evidence
/// survive; collapsing them would hide a real inconsistency behind a single made-up default. A
/// handful of purely self-referential/fixture-noise sightings (the scanner or a redaction unit
/// test mentioning a name that is genuinely read only by SOME OTHER crate) are omitted rather
/// than given a misleading row — see the plan doc's drop list.
///
/// Three known, DELIBERATELY UNFIXED limitations of the gate this table is checked against —
/// read this before trusting any direction's silence too far:
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
///    fixture.** The three attribution-code rows above are `Layer::Injected` (matching how
///    `attribution_code_from` reads them in real code) even though the only site the SCANNER
///    actually resolved them from is a `#[cfg(test)]` module in the SAME file — `layer_for` has
///    no notion of "this specific literal sighting sits inside a test block" for the raw
///    literal-sweep path (only `SRC_TEST_MODULE_OVERRIDES`, and only for DIRECT `env::var` reads,
///    covers that). The declared `Layer::Injected` is still the truthful answer for how the
///    variable is ACTUALLY read in production; it just isn't provable from this one incidental
///    sighting alone.
///
/// ⚠ Limitation 2 above is the one that CHANGED. Read it as history: the `{VENUE}_{TIER}_API_*` and
/// `{VENUE}_{BROKER,BUILDER}_CODE` families are no longer undeclared. They are enumerable data now —
/// `vike_model::credential_keys`' `lookup_keys` folds the suffix/tier tables over
/// `vike_model::VENUES` — and THE GENERATED KEY GRID at the bottom of this table declares every one
/// of them, gated by `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared`
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
/// The fixture composes the name now (`crates/vike-bridge-core/src/credentials.rs`'s
/// `absent_or_invalid_or_unmechanized_is_none`) and the row is deleted, which is also what keeps
/// this table consistent with the other seven unmechanised venues, none of which ever had one. The
/// CLASS in limitation 3 is unchanged and still populated — a bridge crate's own
/// `tests/config_env.rs` fixture is the only sighting many per-venue rows have.
pub const SETTINGS: &[Setting] = &[
    Setting {
        name: "ALPACA_SANDBOX_ACCOUNT_ID",
        krate: "bridges/alpaca",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_SANDBOX_CLIENT_ID",
        krate: "bridges/alpaca",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_SANDBOX_CLIENT_SECRET",
        krate: "bridges/alpaca",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The `api` model driver's key, read in BOTH of this crate's binaries — the evaluation
        // harness's `main.rs` and the unattended runner's `src/bin/vike-agent-run.rs`, each with its
        // own copy of the constant below (an IMPORTED const scans as a dynamic read, so the
        // duplication is what keeps this row visible to the gate). One row covers both: a row is
        // keyed on (name, crate).
        // Unset is not a quiet no-op: either binary REFUSES to start when a real model was asked
        // for, because a run that measured nothing and exited 0 is the failure the live-smoke lane's
        // skip-honesty step exists to prevent. The value is a secret, so nothing formats it — the
        // refusal names the VARIABLE and never the value.
        name: "ANTHROPIC_API_KEY",
        krate: "vike-agent-eval",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Konst("ANTHROPIC_API_KEY_ENV"),
        default: "<none> → `vike-agent-eval run` (unless --scripted) and `vike-agent-run run \
                  --driver api` both refuse to start",
    },
    Setting {
        name: "ASTER_BUILDER_FEE_RATE",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "0",
    },
    Setting {
        name: "ASTER_LIVE_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_LIVE_SIGNER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_LIVE_USER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SIM_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SIM_USER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SMOKE_ORDER",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "ASTER_TESTNET_PRIVATE_KEY",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_TESTNET_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_TESTNET_SIGNER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_TESTNET_USER",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_TESTNET_USER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_DEMO_API_KEY",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_DEMO_API_SECRET",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_PASSPHRASE",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_SECRET",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_MAINNET",
        krate: "bridges/binance",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("MAINNET_ENV"),
        default: "false",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "BINANCE_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "false — REFUSED in secrets.env; set it in the process environment",
    },
    Setting {
        name: "BYBIT_DEMO_API_KEY",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_DEMO_API_SECRET",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_MAINNET",
        krate: "bridges/bybit",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("MAINNET_ENV"),
        default: "false",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "BYBIT_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "false — REFUSED in secrets.env; set it in the process environment",
    },
    Setting {
        name: "CARGO_CFG_TARGET_OS",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "CARGO_FEATURE_FXCM",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // Where the fxcm build script writes `libfcshim.so` — the shared object that carries the C++
    // boundary to the ForexConnect SDK, and which `crates/bridges/fxcm/src/loader.rs` opens at
    // RUNTIME. New on 2026-09-09: before that the shim was a static archive linked into the binary
    // through `cc::Build::compile`, which owns `OUT_DIR` internally, so this script never named it.
    //
    // ⚠ It is also used to DERIVE the cargo profile directory (three `ancestors()` hops), because
    // cargo declares no variable for it and the loader's dev rung looks for the shim beside the
    // executable. That derivation is best-effort by construction — a failure is a `cargo:warning`,
    // not a build failure — since the installed and container shapes do not depend on it. `expect`
    // on the variable itself, though: cargo always sets it for a build script, and one that cannot
    // find its own output directory has nowhere to write.
    Setting {
        name: "OUT_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The compiled-STUDY host's build script, the twin of the row above: same fixed hop to the
    // workspace root, same fixed hop to its own committed fixture tree.
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-user-research",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The third build script that reads it, and the row that was MISSING until direction 1 started
    // demanding a row per `(name, krate)` rather than per name — the two rows above spelled the
    // name, so the gate was satisfied while this crate's read had nothing recorded for it.
    // `vike-buildinfo`'s `build.rs` hops from the manifest directory to the repo root to shell out
    // to `git`, so an absent value is a hard `expect` rather than a fallback: cargo always sets it,
    // and a build script that could not locate its own crate has nothing to fall back TO.
    Setting {
        name: "CARGO_MANIFEST_DIR",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The C compiler `crates/bridges/fxcm/tests/fcsdk_rpath_tag.rs` drives to LINK a probe binary
    // and read back which rpath tag came out (`DT_RPATH` vs `DT_RUNPATH` — the distinction that
    // made the venue's packaged install unloadable). Honouring `$CC` is the ordinary convention and
    // is what lets the test follow a runner that does not put its compiler at `cc`; unset falls
    // back to `cc`, and a box with neither skips the test loudly rather than failing it.
    Setting {
        name: "CC",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "cc",
    },
    // The same convention, one crate over: `crates/vike-ops/tests/release_fxcm_artifact_gate.rs`
    // links its own probes to kill each of the release script's `verify` assertions — a correct
    // one, and three each missing one property — for the same reason the fxcm row above exists,
    // that a rule keyed on a file MENTIONING a flag survives the emission of it being deleted.
    // Unset falls back to `cc`; a box with neither compiler skips the test loudly.
    Setting {
        name: "CC",
        krate: "vike-ops",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "cc",
    },
    Setting {
        // The agent-eval harness's SECOND model credential, read in its `main.rs` beside the API
        // key and read nowhere else. It is the long-lived subscription token `claude setup-token`
        // mints, and the Claude Code CLI reads it from the variable of the same name — so this
        // harness's whole job with it is to move it from its own environment into the one child
        // that needs it, while `SCRUB_FROM_CHILDREN` removes it from the two children that must not
        // have it (the MCP server and the paper node). Unset is not a quiet no-op: `run --driver
        // claude-cli` REFUSES to start, in the same words as the API key's refusal, because a lane
        // that measured nothing and exited 0 is the failure this harness exists to prevent. The
        // value is a secret, so nothing formats it — the refusal names the VARIABLE.
        name: "CLAUDE_CODE_OAUTH_TOKEN",
        krate: "vike-agent-eval",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Konst("CLAUDE_CODE_OAUTH_TOKEN_ENV"),
        default: "<none> → `vike-agent-eval run --driver claude-cli` and `vike-agent-run run \
                  --driver claude-cli` both refuse to start",
    },
    Setting {
        name: "CTRADER_CLIENT_ID",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_CLIENT_SECRET",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_ACCESS_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        // `CtraderConfig::from_vars` reads this through a COMPUTED key
        // (`format!("CTRADER_{tier}_ACCESS_TOKEN")`) — an injected-map read the scanner cannot
        // resolve, so the row read `TestOnly` while the only literal sightings sat under `tests/`.
        // The catalog's injected-map test now spells it in `src`, making the observation match the
        // layer the variable has always really had.
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_ACCOUNT_ID",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_REFRESH_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        // Same computed-key shape as `CTRADER_DEMO_ACCESS_TOKEN` above — see that row's note.
        // (`CTRADER_DEMO_ACCOUNT_ID` stays `TestOnly`: it is read the same injected way but is
        // still only literal-sighted under `tests/`, so the scanner cannot observe it here.)
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_ACCESS_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_REFRESH_TOKEN",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_REDIRECT_URI",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "http://localhost:5033/",
    },
    Setting {
        name: "CTRADER_SCOPE",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "trading",
    },
    Setting {
        // Where `ctrader_authorize` writes the OAuth access+refresh token. It had NO resolver at
        // all before this: a bare `std::fs::write("token.json", …)` into the process WORKING
        // DIRECTORY, i.e. a live credential in whatever directory the binary was started from.
        // Now the explicit rung of `$CTRADER_TOKEN_FILE` → `<project>/settings/state/
        // ctrader_token.json` → the legacy `./token.json` (reached only with no project above the
        // CWD). `Layer::Binary` and read as a bare literal, exactly like its two siblings above —
        // `src/bin/ctrader_authorize.rs` is the only file that names it, so nothing in this
        // crate's library `src/` can score it `Injected` and outrank the row.
        name: "CTRADER_TOKEN_FILE",
        krate: "bridges/ctrader",
        scope: Scope::Venue,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "<project>/settings/state/ctrader_token.json, else ./token.json",
    },
    Setting {
        // STEP 2 flipped this Library -> Binary: `databento::client::api_key_from_env()` read the
        // workspace `.env` (and then process env) inside the ADAPTER; the `databento_backfill` bin
        // now does that read and passes the key in as a `&str`, which is what the sibling
        // `tardis`/`vike-archive` adapters in this same crate already did. The `.env` half is a
        // map lookup and the process-env fallback a direct read, so the DIRECT read wins the
        // `Naming` tie-break (the registry's documented rule) and it is `Literal` in the bin.
        name: "DATABENTO_API_KEY",
        krate: "vike-backfill",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "DERIBIT_DEMO_API_KEY",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_DEMO_API_SECRET",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1_LOGIN",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1_LOGIN",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1_PASSWORD",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1_PASSWORD",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO1_SERVER",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2_LOGIN",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2_LOGIN",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2_PASSWORD",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2_PASSWORD",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO2_SERVER",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_JNLP",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_LOGIN",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_PASSWORD",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_SMOKE_ACCOUNT",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "EOD_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "FCSDK_DIR",
        krate: "bridges/fxcm",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_PASSWORD",
        krate: "bridges/fxcm",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_PASSWORD",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_USER",
        krate: "bridges/fxcm",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_USER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_PASSWORD",
        krate: "bridges/fxcm",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_PASSWORD",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_USER",
        krate: "bridges/fxcm",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_USER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_BUILDER_FEE_TENTHS_BP",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "0",
    },
    Setting {
        name: "HYPERLIQUID_DEMO",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_ACCOUNT_ADDRESS",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_PRIVATE_KEY",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_HIP3",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("HIP3_ENV"),
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    // The idle-cadence soak (#873) resolves the account address it watches from the `.env` map,
    // falling back to the LIVE key when no demo one is set — a `vars.get` in `tests/`, so
    // TestOnly + MapLookup.
    Setting {
        name: "HYPERLIQUID_LIVE_ACCOUNT_ADDRESS",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE_PRIVATE_KEY",
        krate: "bridges/hyperliquid",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "HYPERLIQUID_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "false — REFUSED in secrets.env; set it in the process environment",
    },
    Setting {
        name: "HYPERLIQUID_MAINNET",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "false",
    },
    Setting {
        name: "HYPERLIQUID_MAINNET",
        krate: "vike-tradehub",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "false",
    },
    Setting {
        name: "HYPERLIQUID_SIM_PRIVATE_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_ACCOUNT",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_ACCOUNT",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_BACKEND",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_BACKEND",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_CLIENT_ID",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_CPAPI_URL",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_CPAPI_URL",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_DATA_CLIENT_ID",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_HOST",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_MKTDATA_TYPE",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_PORT",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_PORT",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_LIVE_ACCOUNT",
        krate: "bridges/vike-ibkr",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_API_KEY",
        krate: "bridges/ig",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_API_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_IDENTIFIER",
        krate: "bridges/ig",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_IDENTIFIER",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_PASSWORD",
        krate: "bridges/ig",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_PASSWORD",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "JAVA_HOME",
        krate: "bridges/dukascopy",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/bin/jre/*/bin/java, else `java` from PATH",
    },
    Setting {
        name: "JFOREX_BRIDGE_JAR",
        krate: "bridges/dukascopy",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/bin/jforex/jforex-bridge.jar",
    },
    Setting {
        name: "OANDA_DEMO_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_ACCOUNT_ID",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_API_KEY",
        krate: "vike-connections",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_ACCOUNT_ID",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_API_KEY",
        krate: "bridges/oanda",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_DEMO_API_PASSPHRASE",
        krate: "vike-run",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_DEMO_API_SECRET",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_MAINNET",
        krate: "bridges/okx",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("MAINNET_ENV"),
        default: "false",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "OKX_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "false — REFUSED in secrets.env; set it in the process environment",
    },
    // -- vike-buildinfo's build script: the four cargo-supplied facts it stamps into the BUILD
    //    IDENTITY every shipped binary reports. None is an operator knob and none can be set from a
    //    settings file — cargo supplies all four to a build script, and by the time a binary runs
    //    they are compile-time constants. They have rows because `find_env_reads` resolves a direct
    //    `env::var` argument WITHOUT the prefix filter (deliberately — see `scan::ENV_PREFIXES`), so
    //    an undeclared one fails `every_read_variable_is_declared`. `BuildScript` is what the file
    //    path implies and what `declared_layer_matches_the_path` measures.
    Setting {
        name: "OUT_DIR",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "OUT_DIR",
        krate: "vike-user-strategies",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The compiled-STUDY host's twin: its build script WRITES two generated registries here and its
    // lib/integration tests `include!` them back out.
    Setting {
        name: "OUT_DIR",
        krate: "vike-user-research",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    // The OS search path, read to PREPEND a directory rather than to configure anything:
    // `crates/vike-ops/tests/release_fxcm_artifact_gate.rs` puts a fake `cargo` in front of the
    // real one so it can drive `scripts/release_fxcm_artifact.sh`'s whole `build` composition —
    // including a shim that CLOBBERS the default artifact, which is the only way to prove the
    // byte-identity guard is wired in rather than merely defined. The inherited value is kept as
    // the tail, so `bash`/`readelf`/`sha256sum` still resolve; unset degrades to the shim dir
    // alone, and the test then skips on the missing toolchain rather than lying.
    Setting {
        name: "PATH",
        krate: "vike-ops",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<inherited> → the shim directory is prepended to it",
    },
    Setting {
        name: "PMXT_SMOKE",
        krate: "vike-backfill",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_ADDRESS",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_API_KEY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_AUTO_REDEEM",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_CANCEL_ORDER_ID",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_FROM",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_MAX_SPAN",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "45",
    },
    Setting {
        name: "POLY_CHAIN_PROXY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_RPC_URL",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "https://polygon.drpc.org",
    },
    Setting {
        name: "POLY_CHAIN_TO",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_CHAIN_WATCH",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "",
    },
    Setting {
        name: "POLY_EGRESS_PROBE_URL",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "https://ipinfo.io/json",
    },
    Setting {
        name: "POLY_EXEC",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("POLY_EXEC_ENV"),
        default: "",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "POLY_EXEC",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "REFUSED in secrets.env; set it in the process environment",
    },
    Setting {
        name: "POLY_EXEC_MARKETS",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("POLY_EXEC_MARKETS_ENV"),
        default: "",
    },
    Setting {
        name: "POLY_EXPECT_EGRESS_COUNTRY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_FILL_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_FUNDER",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_GAMMA_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // The escape hatch on the venue's own order-placement geoblock pre-flight
        // (`crates/bridges/polymarket/src/mount.rs`'s `geoblock_override_enabled`). ⚠ `Injected`,
        // NOT `Library` like its three `POLY_EXEC*` siblings, and deliberately so: the STEP-2
        // target shape is a caller-supplied map, and the `Layer::Library` work-list is a ratchet
        // that may shrink and never grow. So this flag is set in the credential store beside the
        // venue's other keys, never exported into the process env. It arms nothing — `POLY_EXEC=1`
        // is still required, and a `POLY_EXEC` line in that same store is still refused by
        // `crates/vike-config/src/arming.rs`'s `refuse_credential_file_arming`.
        name: "POLY_GEOBLOCK_OVERRIDE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_HEARTBEAT",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("HEARTBEAT_ENV"),
        default: "",
    },
    Setting {
        name: "POLY_LIVE_PK",
        krate: "vike-run",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_MAINNET_ADDRESS",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_MAINNET_PRIVATE_KEY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_MAINNET_SECRET",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_NONCE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_PASSPHRASE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_PRESUBMIT_REGISTER",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("POLY_PRESUBMIT_REGISTER_ENV"),
        default: "",
    },
    Setting {
        name: "POLY_PRIVATE_KEY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_PRIVATE_KEY",
        krate: "vike-mount",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_PROXY_ENABLED",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "true",
    },
    Setting {
        name: "POLY_PROXY_HOST",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "127.0.0.1",
    },
    Setting {
        name: "POLY_PROXY_PORT",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "1080",
    },
    Setting {
        name: "POLY_RATE_GATE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("POLY_RATE_GATE_ENV"),
        default: "",
    },
    Setting {
        name: "POLY_RECONCILE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("POLY_RECONCILE_ENV"),
        default: "",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. The arming RULE
        // is unchanged — `vike_bridge_core::mainnet` still reads the process env OR the map,
        // deliberately (its STEP-2 convergence) — only the credential file has stopped being a
        // place it can be armed from. See `vike_config::arming`.
        name: "POLY_RECONCILE",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "REFUSED in secrets.env; set it in the process environment",
    },
    Setting {
        name: "POLY_REDEEM_HALT",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_REDEEM_SMOKE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "POLY_RELAYER_API_KEY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_RELAYER_API_KEY_ADDRESS",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_REWARD_WEIGHT",
        krate: "vike-run",
        scope: Scope::Venue,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "0.0",
    },
    Setting {
        name: "POLY_SIGNATURE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_SIGNATURE_TYPE",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "Poly1271",
    },
    Setting {
        name: "POLY_SOCKS_PROXY",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "",
    },
    Setting {
        name: "POLY_TIMESTAMP",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLY_WS_PROXY_ENABLED",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Dynamic,
        default: "",
    },
    Setting {
        name: "POLY_WS_TOKENS_PER_SOCKET",
        krate: "bridges/polymarket",
        scope: Scope::Venue,
        layer: Layer::Library,
        naming: Naming::Konst("TOKENS_PER_SOCKET_ENV"),
        default: "50",
    },
    // See the `OUT_DIR` note above: cargo-supplied build-script facts, not operator knobs.
    Setting {
        name: "PROFILE",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // Cargo's own `rustc`, which is not necessarily the one on `PATH` — the whole reason the
        // build script asks rather than shelling out to a bare `rustc`.
        name: "RUSTC",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "rustc",
    },
    Setting {
        name: "RUST_LOG",
        krate: "vike-log",
        scope: Scope::External,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "info",
    },
    Setting {
        name: "TARDIS_API_KEY",
        krate: "vike-backfill",
        scope: Scope::External,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    // The target triple `vike_buildinfo::TARGET` reports — see the `OUT_DIR` note above.
    Setting {
        name: "TARGET",
        krate: "vike-buildinfo",
        scope: Scope::External,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // ⚠ NOT a read. The only place this workspace spells the name is
        // `crates/vike-bridge-core/tests/credential_chain_roots.rs`, which plants a populated
        // credential file under a temp directory, names that directory with this variable, and
        // asserts the credentials come from the PROJECT store anyway. The row exists because the
        // literal sweep observes it there and `every_read_variable_is_declared` keys on the NAME, so
        // without one the gate reports an undeclared variable; `TestOnly` is what the file path
        // implies and what `declared_layer_matches_the_path` measures.
        //
        // The `vike-cli` sighting (`cmd/config.rs`'s redaction NEGATIVE table: an OS path is not a
        // `_USER` credential) is subtracted by `NON_READ_LITERAL_MENTIONS` and is not a row.
        //
        // `MapLookup`, not the `Literal` this row carried until `declared_naming_matches_the_call_site`
        // learned to check the field: the test names the variable as a KEY it inserts into the
        // caller-supplied map it then hands to `load_workspace_secrets_from_env`. Nothing in
        // `vike-bridge-core` reads it from process env at all, so `Literal` claimed a direct-read
        // spelling that exists nowhere in the crate — and since #1102 that claim is operator-facing,
        // where it would have read as "this comes from the environment" about the one variable this
        // test exists to prove INERT. Its three platform siblings (`HOME`/`XDG_DATA_HOME`/
        // `LOCALAPPDATA`, under `vike-model`) are `MapLookup` too.
        name: "USERPROFILE",
        krate: "vike-bridge-core",
        scope: Scope::External,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "<never read — asserted inert>",
    },
    // The three remaining VIKE_ALERT* rows moved from `vike-ops` to `vike-alerting` with the tree
    // that reads them (`delivery::webhook_configs_from_env`) — `krate_of` keys on the source path,
    // so a row left on the old crate would fail `every_declared_variable_is_read`.
    //
    // STEP 2 DELETED the fourth, `vike-alerting`'s own `VIKE_ALERTS` row: `persist::path()` read
    // the override inside a LIBRARY while `vike-tradehub`'s `main.rs` already resolved the same
    // override in the binary and called `persist::load_path`, so the library read had no caller
    // left. The whole env-reading `path`/`load`/`save` family was replaced by the path-taking
    // `load_path`/`save_path`, leaving the row below as the workspace's ONLY reader of this
    // variable.
    Setting {
        // The HEADLESS alerting mount's rules file: the DAEMON's `main.rs` reads it through
        // `const ALERTS_ENV` (Layer::Binary — the correct shape) and passes the resolved path to
        // `persist::load_path`.
        name: "VIKE_ALERTS",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/settings/state/alerts.json",
    },
    Setting {
        name: "VIKE_ALERT_TELEGRAM_CHAT_ID",
        krate: "vike-alerting",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_ALERT_TELEGRAM_TOKEN",
        krate: "vike-alerting",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_ALERT_WEBHOOK_URL",
        krate: "vike-alerting",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_ALLOW_WITHDRAW_KEYS",
        krate: "vike-bridge-core",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_APPMAX",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // The cohort API root (`data.vike.io`), read by the `vikedata_backfill` bin
        // (`crates/vike-backfill/src/bin/vikedata_backfill.rs`'s `API_BASE_ENV`), which pulls it
        // out of its one `std::env::vars()` sweep and threads it into
        // `crates/vike-backfill/src/vikedata/client.rs` as a plain `&str`. `--api-base` beats it at
        // the call site.
        //
        // ⚠ Rows are keyed on `(name, krate)`, and this name had TWO of them until the research
        // crate dissolved: the study's own binary read the same variable with its own default and
        // its own flag precedence, and neither row could have declared the other's read. That is
        // the keying argument, and it survives the sibling's deletion — a SECOND crate reading
        // `VIKE_API_BASE` tomorrow needs a row of its own, not a mention in this one.
        //
        // ⚠ `MapLookup`, not `Konst`, and the distinction is the one the registry exists to make.
        // The binary takes ONE `std::env::vars()` sweep at the top of `main` and asks the resulting
        // map — `env.get(API_BASE_ENV)` — so there is no direct process-env read to spell.
        //
        // A BLANK value is IGNORED rather than honoured: `VIKE_API_BASE=` in a
        // systemd unit is an unset variable spelled clumsily, and taking it as a base URL fails
        // every fetch with a message about the empty string instead of about the missing config.
        name: "VIKE_API_BASE",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "https://data.vike.io/v1",
    },
    Setting {
        // The cohort API credential, taken from the CREDENTIAL STORE's map (never process env) by
        // the `vikedata_backfill` bin (`crates/vike-backfill/src/bin/vikedata_backfill.rs`'s
        // `API_KEY_ENV`), which passes it into `crates/vike-backfill/src/vikedata/client.rs` as a
        // parameter; that library reads no environment and opens no store. Same
        // `creds.get(NAME)`-in-a-bin idiom `VIKE_ARCHIVE_API_KEY` uses above, and the reason this
        // row is `Layer::Binary` / `Naming::MapLookup` rather than `Injected`.
        //
        // ⚠ It needs a row DESPITE being a credential. THE GENERATED KEY GRID below enumerates the
        // `{VENUE}_{TIER}_API_*` family that `vike_model::credential_keys` folds out of
        // `vike_model::VENUES`; `VIKE_API_KEY` names no venue and no tier, so the grid cannot
        // produce it, `every_read_variable_is_declared` sees the literal in the bin, and without
        // this row the gate is red.
        //
        // ⚠ This name also carried TWO rows until the research crate dissolved — the study's own
        // binary read the same credential and refused before its first HTTP call. Rows are keyed on
        // `(name, krate)`, so neither row ever declared the other's read; the deletion of the
        // sibling narrows the keying argument to one row, it does not retire it.
        //
        // ABSENT is a REFUSAL, not a fallback, and for a sharper reason than in the study: an
        // unauthenticated fetch 403s part-way through a window, and this binary WRITES — a partial
        // batch lands under a commit key claiming the whole window, after which the honest fetch of
        // that window is a silent no-op forever.
        name: "VIKE_API_KEY",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // data.vike.io archive backfill (vike-backfill's `vike-archive` feature): the
        // `vike_archive_backfill` bin reads this via `dotenv.get("VIKE_ARCHIVE_API_KEY")` (the
        // MapLookup idiom `TARDIS_API_KEY` also uses) and passes it into `vike_archive::
        // ArchiveClient::new` as a plain parameter — the library itself never reads env/`.env`.
        name: "VIKE_ARCHIVE_API_KEY",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // events_api.rs's #[ignore]d live smokes (self-skip without a real token id) read this
        // purely to name a real token_id for the network round trip — the same TestOnly idiom as
        // ASTER_SMOKE_ORDER above. Never read outside that trailing #[cfg(test)] module.
        name: "VIKE_EVENTS_API_SMOKE_TOKEN",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_ARRANGE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "Grid",
    },
    Setting {
        name: "VIKE_BINANCE_TRADE_LITE_FILL",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("TRADE_LITE_FILL_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_BYBIT_FAST_EXEC",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_CAL_PAGE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "0",
    },
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_CAPTURE_FIXTURES",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // QA capture hook: draw the two `vike_app_core::capture_seed::trendline_overlay` drawings
        // on every chart whose series has bars. PRESENT-ness is the whole value (`is_ok()`), the
        // `VIKE_DOM_TESTORDER` idiom — hence the empty default, which is "not set ⇒ nothing drawn
        // and every `ChartState::overlays` map stays empty".
        name: "VIKE_CHART_DRAW",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("CHART_DRAW_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_COUNTERS_FILE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "<exe_dir>/counters.vmc",
    },
    Setting {
        name: "VIKE_COUNTERS_FILE",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_DATAHUB_ADDR",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "127.0.0.1:7878",
    },
    Setting {
        // The env layer over `config.toml`'s `datahub_advertise_addr` (split-plane REQ-2): the
        // datahub dial address the vike-tradehub daemon ADVERTISES in `Welcome.features`
        // (`datahub=<addr>`) so a connected client with no explicit `datahub_addr` of its own
        // dials the datahub there. Read once by `vike_config::Config::apply_env` from the
        // caller-supplied map — the same wiring as its `VIKE_TRADEHUB_ADDR` sibling below.
        name: "VIKE_DATAHUB_ADVERTISE_ADDR",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The explicit opt-in for a NON-LOOPBACK `VIKE_DATAHUB_ADDR` (the exact string "1"): the
        // datahub protocol authenticates nothing unless node keys are configured, so the bin
        // REFUSES to start on a non-loopback bind without it — `vike_datahub::server`'s
        // `bind_decision`, the tradehub guard's twin. ⚠ Setting it is NECESSARY, not sufficient:
        // that same guard takes the server's `ServerAuth` as a third input and refuses a key-less
        // non-loopback bind (`BindDecision::RefuseUnauthenticated`) even with this set — the
        // variable consents to being REACHABLE, never to serving the store write and the Rhai
        // compiler unauthenticated.
        // Env-only, unlike the tradehub pair: this binary loads no settings files at all.
        name: "VIKE_DATAHUB_ALLOW_PUBLIC_BIND",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The datahub's CONTROL-scope node key (`docs/decisions/0025-datahub-remote-posture.md`):
        // the write scope — the `Backfill` store write plus every `Run*` verb, which compile
        // client-supplied Rhai server-side. Read by
        // `vike_datahub_client::node_auth::node_keys_from_vars` out of the CALLER-supplied
        // credential map, so the row is `Injected`/`MapLookup` and the crate that owns the literal
        // is the LIGHT client crate — the same shape (and the same reasoning) as the
        // `VIKE_TRADEHUB_CONTROL_KEY` / `vike-tradehub-client` row further down. The BINARY owns
        // the store read; this crate reads no environment and opens no file.
        name: "VIKE_DATAHUB_CONTROL_KEY",
        krate: "vike-datahub-client",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // The OBSERVE twin of the row above: the read scope (history + catalog). ⚠ Their joint
        // ABSENCE is the gate — with neither key configured the datahub authenticates nothing and
        // serves exactly as it did before 0025 was adopted, which is the credential-is-the-gate
        // idiom this workspace uses for venues.
        name: "VIKE_DATAHUB_OBSERVE_KEY",
        krate: "vike-datahub-client",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_DATAHUB_STORE",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "market_data/hist",
    },
    Setting {
        name: "VIKE_DOM_GROUP",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_DOM_MODE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_DOM_TESTORDER",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_DOM_VENUE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_EXPORT_DIR",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "<exe_dir>/exports",
    },
    // A MEASUREMENT hook, not a feature: log the achieved frame rate once a second. It exists
    // because the same idle chart costs 10.9–12.5% CPU natively on a GPU and 124–275% under
    // lavapipe in a container, and varying the resolution ruled fill rate out — the cost is
    // per-FRAME, so the open question is how many frames each environment draws. Read ONCE into a
    // `OnceLock` rather than per frame: this is the hottest loop in the binary.
    Setting {
        name: "VIKE_FRAME_LOG",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("FRAME_LOG_ENV"),
        default: "unset (off) — the EXACT string \"1\" enables it",
    },
    // The out-of-band kill switch. ⚠ Its DEFAULT moved off the exe directory, which is READ-ONLY
    // under the shipped units' `ProtectSystem=strict` — a unit that set no override had a kill
    // switch that could not be armed and no error anywhere.
    // `crates/vike-bridge-core/src/halt.rs`'s `resolve_halt_path` owns the precedence;
    // `halt_path_arming_error` is why an unusable one is now loud instead of silent.
    Setting {
        name: "VIKE_HALT_FILE",
        krate: "vike-bridge-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("HALT_FILE_ENV"),
        default: "<project>/settings/state/HALT, else <exe_dir>/HALT",
    },
    // ...and the TEST that arms it. `crates/vike-paper/tests/paper_halt_process_wide.rs` sets the
    // variable and reads it back, because the mutation it pins (a `halt_engaged` fallback onto the
    // process-wide sentinel) only reddens where the resolved path EXISTS — so the test supplies the
    // environment instead of hoping the box has one. It re-declares the name as its own `const`
    // rather than importing `vike_bridge_core::halt`'s: vike-paper must not depend on that crate at
    // all, which is the split the test is protecting. A row of its own because the registry is keyed
    // on `(name, krate)` and the vike-bridge-core row above declares a DIFFERENT crate's read.
    Setting {
        name: "VIKE_HALT_FILE",
        krate: "vike-paper",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("HALT_FILE_ENV"),
        default: "<none> → the test writes the file it points at",
    },
    // The per-user store fallback (`vike_model::store_path`). Read wherever `VIKE_HIST_STORE` is,
    // and ONLY as the last step of the same precedence: they answer "where does a store go when this
    // machine is not the checkout that built the binary", which is a fresh INSTALL. Not `Scope::Vike`
    // — these are the platform's own variables, so the naming convention does not apply.
    //
    // ⚠ THREE rows where there used to be TWELVE. Each of vike-app, vike-datahub,
    // `vike_backtest::binutil` and `vike_backfill::cli` pasted the SAME three `std::env::var` lines
    // to build `user_data_dir`'s arguments — one identical computation, four chances to diverge,
    // and in the two bin-glue crates the paste sat in a LIBRARY file (six of the seventy-seven
    // `Layer::Library` rows). `vike_model::store_path::user_data_dir_from_vars` takes the
    // already-collected environment MAP instead, so the trio is spelled in exactly ONE file and the
    // four callers pass `&std::env::vars().collect()` from their own binaries. The four crates'
    // rows are gone because those crates genuinely no longer name these variables — `krate` is
    // where the READ lives, and it lives here now.
    //
    // ⚠ `Layer::Injected`, not `Library`: `vike-model` never touches `std::env`. It is a pure
    // parser over a caller-supplied map — the STEP-2 TARGET state, which is why the six library
    // rows this replaces are retired rather than relocated.
    Setting {
        name: "XDG_DATA_HOME",
        krate: "vike-model",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → $HOME/.local/share/vike-data",
    },
    Setting {
        name: "HOME",
        krate: "vike-model",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the repo default",
    },
    Setting {
        name: "LOCALAPPDATA",
        krate: "vike-model",
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → $HOME/vike-data (Windows only)",
    },
    Setting {
        name: "VIKE_HIST_STORE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<repo>/market_data/hist",
    },
    Setting {
        // `vike_backtest::binutil::store_root` is now PURE — it reads this key out of the map its
        // four in-crate bins pass (`&std::env::vars().collect()`), so the read moved from a library
        // file to the binaries that always owned it. The lift came with the platform trio above: a
        // function cannot half-read the environment, and all four of this one's variables were on
        // the STEP-2 work-list together.
        name: "VIKE_HIST_STORE",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<repo>/market_data/hist",
    },
    // `backtest --fetch-starter` resolves `<project>/tmp` for the scratch its download passes
    // through, by the SAME walk that answers settings, user_data, data and bin — so one project has
    // one answer and this variable relocates all of them together. Read as a MAP LOOKUP from the
    // sweep the binary already owns, never from the process: this is library code under a
    // composition root that has the map.
    //
    // ⚠ The scratch may NOT go in the operating system's temp directory
    // (`crates/vike-ops/tests/system_temp_gate.rs` refuses it): inside the container that path is
    // not the host's, does not survive a restart, and resolves somewhere else on an operator's box
    // — silently. That refusal is what put this read here.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "the project walk from the working directory — <project>/tmp, else the store root's parent",
    },
    Setting {
        // The twin of the row above: `vike_backfill::cli::store_root`, pure, over the map its
        // sixteen in-crate bin call sites pass.
        name: "VIKE_HIST_STORE",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<repo>/market_data/hist",
    },
    Setting {
        name: "VIKE_HIST_STORE",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "market_data/hist",
    },
    Setting {
        name: "VIKE_HIST_STORE",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "market_data/hist",
    },
    Setting {
        name: "VIKE_HL_OUTCOME",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "a temp dir is used when it is unset",
    },
    Setting {
        name: "VIKE_HOLD_TOKENS",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // The env layer over `config.toml`'s `instance_origin`: this deployment's 1–4 character
        // origin tag, stamped into the client order id of every order it places so a SECOND
        // instance sharing the venue API key is recognisable on reconcile instead of anonymous
        // (`vike_model::instance_origin`). The ENV layer is the primary one here by design —
        // two containers from one image differ by exactly this variable. Read once by
        // `vike_config::Config::apply_env` from the caller-supplied map; a malformed value is a
        // hard startup error, because an instance silently running untagged is indistinguishable
        // from one that was never configured.
        name: "VIKE_INSTANCE_ORIGIN",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    // ⚠ `Injected`, not `Binary`, since the multicall merge: the read moved out of
    // `src/bin/tearsheet.rs` into `crates/vike-report/src/tearsheet_cli.rs`'s `run`, which takes a
    // caller-supplied map. That is the direction this registry asks for — *libraries take
    // configuration as parameters; only binaries read the process environment* — so the move is the
    // registry's target state reached, not the LIBRARY_PIN ratchet worked around. The bin still
    // exists and still sweeps the environment; it is now the only thing in that crate that does.
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-report",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_JOURNAL_DIR",
        krate: "vike-run",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_JOURNAL_SNAPSHOT_EVERY",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_LOG",
        krate: "vike-log",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "info",
    },
    Setting {
        name: "VIKE_LOG_DIR",
        krate: "vike-log",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        // `LogConfig::project_dir` (the binary's `<project>/settings/state/logs`) is what every
        // shipped bin now passes; `<exe_dir>/logs` remains the last resort for a binary with no
        // project above it. See `vike_log::resolve_log_dir`'s four-layer table.
        default: "<none> → <project>/settings/state/logs, else <exe_dir>/logs",
    },
    Setting {
        name: "VIKE_LOG_DIR",
        krate: "vike-run",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_LOG_FILE_LEVEL",
        krate: "vike-log",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "trace",
    },
    Setting {
        name: "VIKE_MARK_STREAMS",
        krate: "vike-bridge-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "ON (only the exact \"0\" disables)",
    },
    Setting {
        name: "VIKE_MAX",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // ⚠ REMOVED, not read — settings unification PHASE 5. This was the per-order notional
        // ceiling, read by `vike-app`'s `main.rs` (into `OrderLimits`) and by `vike-cli`'s
        // `cmd/verbs.rs` (the advisory preview guardrail). A ceiling any exported variable can
        // raise is not a ceiling, so BOTH reads were deleted and the value moved to
        // `max_notional_per_order` in `<vike home>/policy.toml` (`vike_config::Policy`), which has
        // no env layer at all — `Policy` implements neither `EnvOverride` nor `CliOverride`.
        //
        // The row survives, in the crate that now REFUSES the variable: silently ignoring a
        // ceiling its operator believes is active is worse than either keeping it or erroring, so
        // `vike_config::refuse_removed_env` looks the name up in the caller-supplied env map
        // (hence `Injected`/`MapLookup`) and fails startup naming the file and key. The day
        // nothing refuses it any more, this row goes too.
        name: "VIKE_MAX_ORDER_NOTIONAL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "REMOVED — refused at startup; use policy.toml's max_notional_per_order",
    },
    Setting {
        // The qty cap STAYS an environment variable, and is the reason the notional one no longer
        // is: it has no policy field and no enforcing counterpart on any node — a local
        // typo-catcher for a human at the `trade` REPL, not a risk ceiling. `Layer::Library`
        // because the read sits in a bin crate's `src/cmd/` tree rather than in `main.rs` (see
        // this module's STEP-2 note on path-based layering of bin-crate glue). `Literal` since
        // Phase 5: the shared name-parameterised `cap` closure went with the notional read, so the
        // remaining site is a plain `env::var("VIKE_MAX_ORDER_QTY")` and `cmd/verbs.rs` no longer
        // needs its DYNAMIC_ALLOWLIST entry.
        name: "VIKE_MAX_ORDER_QTY",
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_MIN",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // Where the backfill bins read/write the MEASURED pace record (`vike_backfill::pace_book`):
        // the per-request wall clock and per-request weight a run observed, so the next run opens on
        // last run's numbers instead of the pacer's pessimistic constant. Default is the PROJECT's
        // state directory (`<project>/settings/state/pace.json`, `vike_model::state_path`), the
        // one folder every program-written file lives in — `<store_root>/pace.json` survives only
        // as the last row, for a binary run with no project above its working directory. The
        // override exists for a read-only store mount or several jobs deliberately sharing one
        // record, and still wins over both. The file is a CACHE, never an input: absent or
        // unreadable means "no record", and every pacer starts where it always did — which is also
        // why the repoint needs no dual read, at the cost of ONE re-measuring run.
        //
        // ⚠ STEP 2 flipped this row `Library` -> `Injected`, and the comment it replaces is the
        // reason it was worth doing: it claimed family 3 ("shared bin glue that merely lives
        // outside a `main.rs`") by pointing at `VIKE_HIST_STORE`'s row in the same crate — which
        // had ALREADY been lifted, on the argument that family-3's ⚠ note now records as wrong. So
        // this row cited a sibling that no longer agreed with it. `cli::pace_book_path` takes the
        // environment map exactly as `cli::store_root` does, `run_klines_backfill_cli` forwards it,
        // and the five `<venue>_backfill` bins pass `&std::env::vars().collect()` — one expression,
        // no variable name duplicated across them, the whole precedence still in `cli.rs`.
        //
        // ⚠ The repoint's `std::env::current_dir()` is NOT what this row describes and does not
        // contradict `Injected`: the working directory names no variable, so the scanner sees
        // nothing and no operator can export it. `Layer` records how the VALUE of this variable
        // reaches the resolver, and that is now a caller-supplied map.
        name: "VIKE_PACE_BOOK",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::Injected,
        // `MapLookup`, not `Konst("PACE_BOOK_VAR")`, even though the key IS spelled through a
        // const: `Naming` records HOW the value is read (a caller-supplied map), and
        // `layer_and_naming_agree_on_every_row` requires every `Injected` row to say so. Same as
        // `VIKE_HIST_STORE`'s row in this crate, whose `HIST_STORE_VAR` const is the same shape.
        naming: Naming::MapLookup,
        default: "<project>/settings/state/pace.json, else <store_root>/pace.json",
    },
    Setting {
        name: "VIKE_PIN_CORES",
        krate: "vike-exec",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("PIN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_PM_RESOLVE",
        krate: "bridges/polymarket",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_POLY_COCKPIT_TOKEN",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "POLY-DEMO",
    },
    Setting {
        name: "VIKE_POLY_TICKS",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_PREFLIGHT_SKIP",
        krate: "vike-mount",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE_AUDIT_MS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "mirrors VIKE_RECONCILE_INTERVAL_MS (Some or None)",
    },
    Setting {
        name: "VIKE_RECONCILE_BALANCE",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE_BALANCE_TOL_ABS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "1.0",
    },
    Setting {
        name: "VIKE_RECONCILE_BALANCE_TOL_REL",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "1e-4",
    },
    Setting {
        name: "VIKE_RECONCILE_GENERATE_MISSING",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE_INFLIGHT_MS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_RECONCILE_INTERVAL_MS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "60000",
    },
    Setting {
        name: "VIKE_RECONCILE_LOOKBACK_MS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "3600000",
    },
    Setting {
        // ⚠ SAFETY OVERRIDE — the REFUSAL of the S2 default-on live reconcile. `1` means "do not
        // ask my venues what they hold", which is why it has a row of its own rather than being
        // read as the negation of `VIKE_RECONCILE`: an operator looking for the off switch must
        // find it under its own name. `vike_config::flags`' `RECONCILE_OFF_ENV` is the reader.
        name: "VIKE_RECONCILE_OFF",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // ⚠ TWO functions in this crate answer for this name and they answer DIFFERENTLY, so the
        // default states the one an operator actually gets: `quarantine_first_default` folds
        // `quarantine` into the map at BOTH live mounts before `parse_policy` ever sees it, and
        // `parse_policy`'s own `hybrid` fallback is reachable only by a caller that skips the fold.
        // Saying "hybrid" here (as this row did until S2) would tell an operator their unset box
        // auto-applies `PositionDrift`, which is the opposite of what it does.
        name: "VIKE_RECONCILE_POLICY",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "quarantine (hybrid only for a caller that skips quarantine_first_default)",
    },
    Setting {
        // ⚠ THIS ROW'S EVIDENCE IS WEAKER THAN A PRODUCTION READ, and saying so is the point of the
        // comment. The daemon's own fold (`with_quarantine_first_default`) MOVED into vike-ops on
        // 2026-09-06 — the row above is the production read now — so what keeps this one alive for
        // `every_declared_variable_is_read` is a LITERAL in `tradehub_cli.rs`'s test region
        // (`daemon_reconcile_policy_honors_explicit_override`), which is the direction-2 looseness
        // `crates/vike-ops/tests/settings_registry.rs` documents.
        //
        // It is KEPT rather than deleted because the claim is still true where it matters: this
        // daemon DOES resolve that variable, through `daemon_recon_env` -> `quarantine_first_default`,
        // and an operator asking "what does the tradehub do with VIKE_RECONCILE_POLICY" deserves a
        // row. What a future editor must know is the coupling: deleting or renaming those two tests
        // strands this row, and the fix then is to delete the row, not to re-add a literal.
        name: "VIKE_RECONCILE_POLICY",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "quarantine (folded in by daemon_recon_env before parse_policy sees the map)",
    },
    Setting {
        name: "VIKE_RECONCILE_STARTUP_DELAY_MS",
        krate: "vike-ops",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "2000",
    },
    Setting {
        name: "VIKE_RECORD_CHAINS",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_CHAINS_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_RECORD_CHAINS_CADENCE_MS",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_CHAINS_CADENCE_ENV"),
        default: "60000",
    },
    Setting {
        // Re-keyed `bridges/deribit`/`Library` -> `vike-config`/`Injected` when the DVOL feed was
        // finally MOUNTED. `vike_deribit::DvolRecorder` used to read this variable itself, from a
        // constructor nothing outside its own test module called — so the toggle had two potential
        // authorities and no consumer at all. The recorder now takes
        // `vike_config::Flags::record_dvol` as a parameter, and the `apply_env` lookup here is the
        // one remaining read: the `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT` shape exactly.
        name: "VIKE_RECORD_DVOL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // Still `bridges/deribit`/`Library`, deliberately: only the ENABLE gate moved. This is the
        // idempotency-bucket width, resolved inside `DvolRecorder::from_flag` beside the store it
        // buckets for — the `VIKE_RECORD_CHAINS_CADENCE_MS` shape, and the same STEP-2 work item.
        name: "VIKE_RECORD_DVOL_CADENCE_MS",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_DVOL_CADENCE_ENV"),
        default: "60000",
    },
    Setting {
        name: "VIKE_RECORD_PROPERTIES",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_RECORD_PROPERTIES",
        krate: "vike-data",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("RECORD_PROPERTIES_ENV"),
        default: "",
    },
    Setting {
        // Regeneration switch for the tessellated-frame text goldens
        // (`crates/vike-chart/tests/tessellation_goldens.rs`). Unset = compare against the
        // committed `tests/goldens/*.txt`; "1" = rewrite them, which must be justified in the
        // commit message. Same shape as `VIKE_REGEN_WINDOW_PIN` below, deliberately: one idiom for
        // "rewrite the committed fixture", so an operator who has met one has met both.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-chart",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        // The SAME regeneration switch, read from the Studio shell's goldens twin
        // (`crates/vike-studio/tests/tessellation_goldens.rs`). Rows are keyed on the
        // (name, krate) pair, so vike-chart's row above cannot declare this crate's read — one
        // more row, same `Konst` spelling, deliberately the same variable so one idiom rewrites
        // every frame-golden suite in the tree.
        name: "VIKE_REGEN_FRAME_GOLDENS",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        // Regeneration switch for the var/zscore characterization pin
        // (`crates/vike-indicators/tests/window_pin.rs`). Unset = compare against the committed
        // fixture; "1" = rewrite it, which must be justified in the commit message.
        name: "VIKE_REGEN_WINDOW_PIN",
        krate: "vike-indicators",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Konst("REGEN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_RUN_PROFILE",
        krate: "vike-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_RUN_PROFILE",
        krate: "vike-run",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_SCALE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_SHOT_FRAME",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "180",
    },
    Setting {
        name: "VIKE_SHOT_WIN",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // ⚠ REMOVED, not read. Nothing in this workspace consumes it: credentials are read from
        // `<project>/settings/secrets.env`, in plaintext, and there is no second store and nothing
        // to unseal.
        //
        // The row survives, in the crate that now REFUSES the variable, for the same reason
        // `VIKE_MAX_ORDER_NOTIONAL`'s does: an operator who set it believes it governs how
        // credentials are opened, and starting anyway would leave that belief silently false. So
        // `vike_config::refuse_removed_env` looks the name up in the caller-supplied env map (hence
        // `Injected`/`MapLookup`) and fails startup. It refuses WITHOUT printing the value — the
        // value is itself a secret and the refusal lands on stderr, which every service manager
        // captures (`RemovedSetting::echo_value`). The day nothing refuses it any more, this row
        // goes too.
        name: "VIKE_SECRETS_PASSPHRASE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "REMOVED — refused at startup; credentials are plaintext in the settings directory",
    },
    // The user-data idle-cadence soaks (#868/#873/#875) — one `#[ignore]`d, credential-gated
    // long-read measurement per venue, each reading the soak duration straight off process env.
    // Six rows because the table is keyed on (name, krate) and six bridge crates read it; the
    // default differs per venue (the two binance soaks want a window longer than the ~180s server
    // ping period, the rest settle for 300s).
    Setting {
        // Safety interlock for the `#[ignore]`d aster user-data soak. ⚠ This comment used to say
        // "aster is MAINNET-ONLY (no testnet endpoint)". That is FALSE and was corrected across the
        // tree on 2026-07-29 — this third copy was missed, which is exactly the rot the correction
        // record on `crates/bridges/aster/CLAUDE.md` exists to stop. The testnet endpoints are real
        // and routed to; only the TESTNET CREDENTIALS are unconfigured, so the soak would otherwise
        // fall through to the live account — hence the interlock.
        // Named through `const ALLOW_MAINNET_ENV` in `tests/aster_userdata_soak.rs`.
        name: "ASTER_SOAK_ALLOW_MAINNET",
        krate: "bridges/aster",
        scope: Scope::Venue,
        layer: Layer::TestOnly,
        naming: Naming::Konst("ALLOW_MAINNET_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/aster",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "900",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        name: "VIKE_SOAK_SECS",
        krate: "bridges/okx",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "300",
    },
    Setting {
        // ⚠ NOT the state ROOT below, despite the name: this is `vike-app`'s strategy-state
        // SIDECAR directory (the per-mount `strategy_state::write_json_atomic` blobs). It predates
        // settings-unification Phase 2, which is exactly why that phase's root had to be called
        // `VIKE_STATE_ROOT` — one variable cannot mean both without an operator silently dumping
        // every strategy sidecar into the state root (or relocating the window layout while
        // pointing the sidecars somewhere). ⚠ Folding this under the state root WAS called "the
        // obvious follow-up" here, and it has since LANDED without needing a second variable:
        // `crates/vike-app/src/main.rs`'s `state_dir_path` joins `strategy-state` onto the boot's
        // own already-resolved state directory, so `<exe_dir>` is the no-project last resort rather
        // than the default this row used to name.
        name: "VIKE_STATE_DIR",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/settings/state/strategy-state, else <exe_dir>/strategy-state",
    },
    // The STATE ROOT override (settings-unification Phase 2): one directory for every file the
    // program writes and no human edits — `workspace.json`, `studio_workspace.json`,
    // `alerts.json`. `vike_model::state_path` is the pure resolver; these three rows are its
    // env-reading callers (see the platform-trio block above for the Layer argument). `pace.json`
    // shares the DEFAULT (`<project>/settings/state`) but not this override — it answers to
    // `VIKE_PACE_BOOK` instead, its row above.
    Setting {
        name: "VIKE_STATE_ROOT",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<none> → <project>/settings/state",
    },
    Setting {
        name: "VIKE_STATE_ROOT",
        krate: "vike-studio",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<none> → <project>/settings/state",
    },
    Setting {
        name: "VIKE_STATE_ROOT",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/settings/state",
    },
    // The SETTINGS DIRECTORY override — `<project>/settings` named outright, skipping the walk.
    //
    // The walk (`vike_model::state_path::project_settings_dir`, and its zero-`vike-*`-dependency
    // twin in `vike_secrets::dotenv`) knows TWO project markers: a checkout's WORKSPACE ROOT (the
    // outermost `Cargo.toml` that declares a `[workspace]` table — neither the nearest manifest nor
    // the outermost one, both of which shipped and were bugs), else a DEPLOYMENT's own `settings/`
    // directory. The second marker exists because
    // all three shipped units run `WorkingDirectory=<project>`, where the install recipe puts a
    // BINARY and no source tree — a `Cargo.toml`-only walk returned `None` there, so a production
    // daemon loaded no policy, no config and NO CREDENTIALS, every venue silently on paper. This
    // variable is the escape hatch above both markers, for a layout neither describes.
    //
    // One row per crate that NAMES it, in three different shapes:
    //   - `vike-model` / `vike-secrets` DECLARE it (`pub const SETTINGS_DIR_ENV`) and read nothing:
    //     both resolvers take the value as a PARAMETER, which is why neither joins `LIBRARY_PIN`.
    //     The raw-literal sweep still observes the constant, hence `Injected`.
    //   - `vike-bridge-core` pulls it out of the caller-supplied process-env map in
    //     `credentials::load_workspace_secrets_from_env` — the ONE lookup that gives all seven
    //     composition roots the hatch for their CREDENTIALS without any of them changing a line.
    //     Spelled as a LITERAL on purpose: `scan`'s map-lookup sweep resolves constants crate-wide,
    //     so importing `vike_secrets::SETTINGS_DIR_ENV` would make this read invisible here.
    //   - `vike-app` / `vike-tradehub` / `vike-cli` / `vike-recorder` look it up in their own
    //     `std::env::vars()` sweep. The first two place the POLICY/config layer with it and read it
    //     in `main.rs` (`Binary`); `vike-cli`'s dispatcher lives in `src/lib.rs`, so its identical
    //     read scores `Injected` — the bin-adjacent-glue shape the registry's module doc already
    //     lists. `vike-recorder` loads no policy at all (it takes an explicit `--profile`) and uses
    //     it for the other two things the directory answers: where the rolling trace log goes, and
    //     which credential store its silent-series pager reads. ⚠ Until 2026-08-08 that daemon read
    //     the variable NOWHERE — `strings` on the shipped binary found it zero times — while its
    //     unit set it and both runbooks claimed it made the answer independent of the working
    //     directory. It did not; resolution was 100% `WorkingDirectory=`.
    //
    // Unset (the default) changes nothing: the walk answers, exactly as it did before. A BLANK
    // value is ignored rather than honoured — it would otherwise resolve settings to `""` and read
    // credentials out of the working directory.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-model",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk: a checkout's workspace root, else a deployment's own settings/",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-secrets",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk: a checkout's workspace root, else a deployment's own settings/",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-bridge-core",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk (the credential chain's project slot)",
    },
    // ⚠ FOUR composition-root rows stood here — `vike-app`, `vike-tradehub`, `vike-cli`,
    // `vike-recorder` — and they are now ONE, under `vike-boot`. None of those binaries reads the
    // variable any more: each hands its `std::env::vars()` sweep to `vike_boot::boot`, which owns
    // the startup sequence and performs the project walk ONCE per process. The consolidation is the
    // point rather than a side effect — the walk happening in five places is what made the CI box's "no
    // policy, no credentials, every venue silently paper" expensive to fix, and what let
    // `vike-app`'s rolling log file land under a different project from the block describing it.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-boot",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk; the ONE walk per process, feeding the settings load, the log home, the credential store and the startup disclosure",
    },
    // …plus one row per crate whose TESTS name it. A live venue smoke resolves the credential store
    // through `vike_secrets::load_workspace_dotenv_from`, whose override is a PARAMETER, so the
    // read happens in the TEST BINARY — itself a `main`, hence `Layer::TestOnly` rather than the
    // `Layer::Library` a loader-side read would have added to `LIBRARY_PIN`'s may-only-shrink
    // work-list.
    //
    // ⚠ These rows exist because the smokes were UNRUNNABLE without them, not for symmetry.
    // `settings/` is gitignored, so a git worktree or a the CI box verification lane checks out
    // `settings/*.toml` and never `secrets.env`; every smoke called the override-blind loader,
    // resolved the empty `settings/` beside it and self-SKIPPED in silence — "no creds → stay
    // paper" is a legitimate state, so nothing was logged and nothing went red. Alpaca is the case
    // that forced it: its hosts are unreachable from the Windows dev box, so its reconcile smoke
    // can ONLY ever be proven from a lane, and until 2026-08-19 it could not be.
    //
    // The ROWS BELOW are the roster — one per crate whose `tests/` name it, and no count is written
    // here or anywhere else. `every_declared_variable_is_read` fails on a row whose crate has
    // stopped naming it, which is what keeps the roster honest in the direction that rots.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/alpaca",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/aster",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/binance",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/bybit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/ctrader",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/deribit",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/dukascopy",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/fxcm",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/hyperliquid",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/ig",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/oanda",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/okx",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/polymarket",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "bridges/vike-ibkr",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // ⚠ `vike-backfill` needs a `NON_READ_LITERAL_MENTIONS` entry to score `TestOnly` here: its
    // `src/cli.rs` spells the name in a fixture inside a trailing `#[cfg(test)]` module, which the
    // raw literal sweep reads as an `Injected` sighting and which would then outrank the real reads
    // in `tests/`. That entry says the fixture is a mention, not a read.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-backfill",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-mount",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // `<project>/user_data` — the USER-CONTENT directory (strategies, run profiles, backtest
    // results, notebooks), and the SIBLING of the settings directory above rather than a child of
    // it. `crates/vike-model/src/state_path.rs`'s `PROJECT_USER_DATA_DIR` argues the split: half of
    // `settings/` is machine-read and half machine-written, while a strategy somebody WROTE is
    // neither — and `settings/secrets.env` holds live venue keys, which must not ride along when a
    // user copies or commits their strategy library.
    //
    // A SEPARATE variable from `VIKE_SETTINGS_DIR`, deliberately: an operator relocating settings
    // for a deployment is answering a different question from a user pointing the app at a strategy
    // library on another disk, and one variable for both would force them to move together.
    //
    // The SHAPES (the rows below are the roster — no count is written here):
    //   - `vike-model` DECLARES it (`pub const USER_DATA_DIR_ENV`) and reads nothing — the resolver
    //     takes the value as a PARAMETER, which is why it does not join `LIBRARY_PIN`. The
    //     raw-literal sweep still observes the constant, hence `Injected`.
    //   - every COMPOSITION ROOT that can compile a Rhai script looks it up in the
    //     `std::env::vars()` sweep it already performs, to find `user_data/indicators/` — the user's
    //     OWN indicators, which each root installs process-wide at startup so a script can call
    //     them. `vike-cli` additionally hands the directory to `cmd::init` to scaffold. Each is
    //     spelled as a LITERAL for the reason the `vike-bridge-core` row above gives: the map-lookup
    //     sweep resolves constants CRATE-wide, so importing
    //     `vike_model::state_path::USER_DATA_DIR_ENV` would make the read invisible here. `Layer` is
    //     computed from the FILE PATH, which is why `vike-cli`'s is `Injected` (its dispatcher lives
    //     in `src/lib.rs`; `main.rs` is a one-line shim) while `vike-app`'s and `vike-datahub`'s —
    //     read in `main.rs` and `src/bin/` — are `Binary`.
    //
    // Unset (the default) changes nothing — the walk answers. A BLANK value is ignored rather than
    // honoured, or user content would resolve to `""` and strategies would be read out of the
    // working directory.
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-model",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data, off the same project walk as settings/",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data (the directory `vike-cli init` scaffolds)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::MapLookup,
        // ONE read, feeding BOTH consumers of the user's indicator library — the chart's ƒx picker
        // in any build, and the Studio's strategy bindings in a `fat` one. Deliberately one read:
        // two resolutions could disagree, and a study callable from a strategy but absent from the
        // picker is indistinguishable from a file that failed to compile.
        default: "<none> → <project>/user_data (the user's own indicators: ƒx-picker chart studies \
                  in any build, plus the Studio's strategy bindings under `fat`)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data (the user indicators a served RunSlice may call)",
    },
    // The `backtest` bin's own read, and a row that was MISSING until direction 1 started demanding
    // one per `(name, krate)`: four crates above already spelled the name, so the gate was satisfied
    // while this binary's read carried no `Layer`, no `Naming` and no default anywhere. It spells
    // the key as a LITERAL rather than importing `vike_model::state_path::USER_DATA_DIR_ENV` on
    // purpose — the map-lookup sweep resolves constants CRATE-wide, so the import would make the
    // read invisible to the gate; the same trade `vike-cli`'s dispatcher makes for this variable.
    // ⚠ `Injected` since the multicall merge — the read moved from `src/bin/backtest.rs` into
    // `crates/vike-backtest/src/backtest_cli.rs`'s `run`, which takes the swept map as a parameter.
    // `Layer` is COMPUTED FROM THE PATH here, so this field follows the file rather than describing
    // an intent; `declared_layer_matches_the_path` is what says so. The read itself is unchanged —
    // it was already a map lookup, which is why only this one field moves.
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → <project>/user_data (the user indicators a profile's script may call)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-user-strategies",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        // The BUILD-TIME read: which user_data tree the compiled-strategy scan bakes into the
        // binary (`<workspace-root>/user_data` when unset — a fixed checkout hop, deliberately
        // NOT the runtime marker walk, because build scripts only ever run in a checkout).
        default: "<none> → <workspace-root>/user_data at BUILD time (compiled-strategy scan)",
    },
    Setting {
        name: "VIKE_USER_DATA_DIR",
        krate: "vike-user-research",
        scope: Scope::Vike,
        layer: Layer::BuildScript,
        naming: Naming::Literal,
        // The compiled-STUDY twin of the row above, and a SEPARATE row because these are keyed on
        // the `(name, krate)` PAIR: two build scripts read the same variable, each baking a
        // different tier of `user_data/` into the binary, and one row could only describe one of
        // them. Same resolution, deliberately — an operator who has learned the override for
        // strategies has learned it for studies.
        default: "<none> → <workspace-root>/user_data at BUILD time (compiled-study scan)",
    },
    Setting {
        // Both VIKE_STUDIO_* rows moved crate AND layer in STEP 2: `StudioState::new` read them
        // inside `vike-studio`'s library, so a caller could neither see nor override the tab and
        // autorun a stale dev-shell export forced on it. They are `new_with_qa` PARAMETERS now, and
        // the ONE caller that wants them — `vike-app`'s `main.rs`, which mounts the Studio tool —
        // does the reads. Nothing else in the workspace wants them: the `studio_shot` capture
        // example poses `right_tab` on the struct directly, and every unit test calls the env-free
        // `new`. `krate_of` keys on the source path, so the rows had to move with the read.
        name: "VIKE_STUDIO_AUTORUN",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("STUDIO_AUTORUN_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_STUDIO_TAB",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("STUDIO_TAB_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_STYLE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_SWEEP_SEQUENTIAL",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("SWEEP_SEQUENTIAL_ENV"),
        default: "Parallel",
    },
    Setting {
        name: "VIKE_SWEEP_THREADS",
        krate: "vike-backtest",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Konst("SWEEP_THREADS_ENV"),
        default: "min(4, available_parallelism())",
    },
    // The TELEGRAM control channel (`vike_tradehub::telegram`). The split between these rows is the
    // whole point: the process-env master flag is read by the daemon BINARY, while the credentials
    // and the two ALLOWLISTS are parsed out of an already-loaded workspace `.env` MAP by the
    // library — the `auth::from_vars` shape, which is the STEP-2 target state, not a violation.
    // The token and a non-empty CHAT allowlist must both be present (plus `VIKE_TRADEHUB_CONTROL=1`)
    // or nothing is constructed; the USER allowlist is the one optional member — absent means
    // chat-only authorization, exactly as before it existed.
    Setting {
        name: "VIKE_TELEGRAM_ALLOWED_CHAT_IDS",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TELEGRAM_ALLOWED_USER_IDS",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TELEGRAM_BOT_TOKEN",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TELEGRAM_CONTROL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    // ⚠ Both rows' DEFAULT is now a ladder, not a path, and the ladder is ONE implementation:
    // `vike_model::tick_store_path::resolve_tick_store_root`. The two binaries used to carry
    // byte-identical copies of a two-rung `env else <exe_dir>` resolution, which is why the two
    // rows below could be kept in step by hand; they are two rows rather than one because the READ
    // still happens in each binary (`Layer::Binary`, and this registry is keyed on `(name, krate)`).
    // Each binary logs the resolved root and the rung that chose it once at startup.
    Setting {
        name: "VIKE_TICK_STORE",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "<project>/market_data/ticks, else <exe_dir>/market_data/ticks",
    },
    Setting {
        name: "VIKE_TICK_STORE",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<project>/market_data/ticks, else <exe_dir>/market_data/ticks",
    },
    Setting {
        name: "VIKE_TOOL",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_TOOLS",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        // QA capture hook: seed the Trade panel with one resting PAPER limit order and one open
        // PAPER position. PRESENT-ness is the value (`is_ok()`), the `VIKE_DOM_TESTORDER` idiom.
        //
        // ⚠ ONE row for TWO reads, and that is the registry's own rule rather than an omission:
        // rows are keyed on the PAIR `(name, krate)`, and both reads are in `vike-app`'s `main.rs`
        // — one arms `App::trade_seed_pending` (the frame loop submits the orders), one fills
        // `startup::StartupEnv::trade_seed` (the layout opens the bar feed that clocks the paper
        // fill). Two rows here would be a duplicate, not extra coverage.
        name: "VIKE_TRADE_SEED",
        krate: "vike-app",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Konst("TRADE_SEED_ENV"),
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_ADDR",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_ALLOW_PUBLIC_BIND",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_CONTROL",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_CONTROL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // Split-plane B1: the `--observe` observer's key NAMES moved out of `vike-app`'s
        // `main.rs` into `vike-app-core`'s `backend_conn::cli_observe_record` (the synthetic CLI
        // backend record), so the read is now `backend_registry::resolve_keys` over the
        // caller-supplied credentials map — the Injected shape. The binary still owns loading
        // that map.
        name: "VIKE_TRADEHUB_CONTROL_KEY",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // STEP 2, done: `cmd/trade.rs` and `cmd/mcp.rs` each read this with `std::env::var` — two
        // `Layer::Library` rows — and that was not merely a layering smell, it was the DEFECT. The
        // `vike-tradehub` daemon on the other end of the socket takes the same key out of the
        // CREDENTIAL STORE (`<project>/settings/secrets.env`), which is never exported to process
        // env, so a correctly-configured box got "not set in the environment" and an exit. The
        // dispatcher now owns both reads and `cmd/nodekeys.rs` resolves them from a caller-supplied
        // map: process env first, store second.
        name: "VIKE_TRADEHUB_CONTROL_KEY",
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_CONTROL_KEY",
        krate: "vike-tradehub-client",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_CONTROL_RATE",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "20.0",
    },
    Setting {
        name: "VIKE_TRADEHUB_LIVE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // ⚠ REMOVED, not read — settings unification PHASE 5, the daemon twin of
        // `VIKE_MAX_ORDER_NOTIONAL` above (same idea, two names). It was the vike-tradehub
        // server-edge per-order ceiling (`ControlLimitsConfig::max_notional`); that value now comes
        // from `max_notional_per_order` in `<vike home>/policy.toml`. This one mattered most: on a
        // production node the variable lives in a systemd unit, where a stale `Environment=` line
        // raises a live risk limit with no diff and no review.
        //
        // Row kept in the crate that REFUSES it — see the `VIKE_MAX_ORDER_NOTIONAL` row's comment.
        name: "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "REMOVED — refused at startup; use policy.toml's max_notional_per_order",
    },
    Setting {
        // Split-plane B1 — the observe twin of the `VIKE_TRADEHUB_CONTROL_KEY` `vike-app-core`
        // row above: the name now lives in `backend_conn::cli_observe_record`, resolved by
        // `backend_registry::resolve_keys` over the caller-supplied map.
        name: "VIKE_TRADEHUB_OBSERVE_KEY",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        // STEP 2, done — the observe twin of the `VIKE_TRADEHUB_CONTROL_KEY` row above; same two
        // library reads retired into one injected resolver (`cmd/nodekeys.rs`), same defect.
        name: "VIKE_TRADEHUB_OBSERVE_KEY",
        krate: "vike-cli",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_OBSERVE_KEY",
        krate: "vike-tradehub-client",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_TRADEHUB_RECORD",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_CANCEL_ORDERS_ON_SHUTDOWN",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "VIKE_WORKSPACE",
        krate: "vike-app-core",
        scope: Scope::Vike,
        layer: Layer::Library,
        naming: Naming::Literal,
        default: "<project>/settings/state/workspace.json",
    },
    // -----------------------------------------------------------------------------------------
    // THE GENERATED KEY GRID — the family this table structurally could not see.
    //
    // Every row below is a key `vike_bridge_core::credentials` BUILDS with a `format!` and then
    // asks the credential map for. None of them appears as a string literal at its read site, so
    // `crates/vike-ops/src/scan.rs`'s `find_map_lookups` could not observe one and this table could
    // not declare one: `OKX_LIVE_API_SECRET` was read on every credential probe with neither a row
    // nor a sighting, while its `OKX_DEMO_API_SECRET` sibling had a row only because a test fixture
    // happened to spell it. That was the last STRUCTURAL hole in the registry, and it sat on the
    // credential path. `DYNAMIC_ALLOWLIST` could never have covered it — that table allowlists a
    // call site the scanner FOUND and could not resolve, and a computed map `get` is not recognised
    // as a candidate site at all.
    //
    // These rows are HAND-WRITTEN DATA, deliberately, and the gate that checks them derives its
    // expectation from somewhere else entirely: `vike_model::credential_keys`' `lookup_keys` folds
    // the suffix/tier tables over `vike_model::VENUES`, and
    // `crates/vike-ops/tests/settings_registry.rs`'s `every_generated_key_is_declared` demands a row
    // here for each. GENERATING these rows from that enumeration would make the gate compare the
    // table to itself — the `roster_matches_the_bridge_crates` failure this repo has already shipped
    // once, green through a mutation that added a bridge crate. So adding a venue to the roster, a
    // tier, or a suffix reddens CI until this block is extended, which is the whole point.
    //
    // ⚠ `just new-venue NAME` DOES extend it, from the new-venue ROW MARKER at the bottom of this
    // block (spelling that token here would make this paragraph one), and that is NOT the
    // self-comparison the paragraph above refuses. The marker is
    // hand-written text in THIS file that spells four tiers and three suffixes literally; it cannot
    // see `credential_keys` and does not grow when that module does. Add a suffix or a tier there
    // and `every_generated_key_is_declared` goes red exactly as before — for every venue at once,
    // scaffolded or not. What the marker removes is only the case it can actually answer: a venue
    // joining the roster, whose twelve rows are pure mechanical restatement of the grid, and which
    // before this went red on the FIRST scaffold run with no generated row and no note anywhere —
    // the scavenger hunt `crates/vike-ops/tests/new_venue_gate.rs` exists to end.
    //
    // Kept as ONE contiguous sorted block rather than interleaved into the alphabetical body above:
    // it is a family with one shared argument, and scattering it would bury that argument in three
    // hundred unrelated rows. `vike-cli config show` sorts its own output, so the operator view is
    // unaffected by the position.
    //
    // Every row is spelled in full rather than through a shared constructor, which is a deliberate
    // cost: one `const fn` would collapse each row to a single line, and it would also stop the row
    // carrying the `Layer` line that `CLAUDE.md`'s documented count command greps for — a command
    // `crates/vike-ops/tests/unrun_command_gate.rs`'s `CHECKED` keeps runnable precisely so the
    // number is never written down by hand. A registry that cannot be counted by the command its
    // own docs name is worse than a long block.
    //
    // ⚠ The grid OVER-approximates over the roster, and `vike_model::credential_keys`' module doc
    // argues why (which venues reach the generic loader is a `match` arm in
    // `crates/vike-connections/src/status.rs`'s `venue_env_configured`, not enumerable data) and
    // what it deliberately excludes (the BESPOKE per-venue shapes — `FXCM_{TIER}_USER`,
    // `DUKASCOPY_DEMO1_LOGIN`, the `POLY_*` trio — which are literals their own bridge's `config.rs`
    // spells, so the scanner always saw them and they always had rows).
    //
    // ⚠ `krate` is `vike-bridge-core` because that is where the map `get` happens, whatever crate
    // calls the loader — and the gate derives the SAME crate from the path of the file that composes
    // the keys. If that file ever moves crates, `every_generated_key_is_declared` and
    // `every_declared_variable_is_read` go red TOGETHER: that signature means RE-KEY these rows, not
    // delete them.
    //
    // ⚠ `default: ""` is not a placeholder. Unset means the venue stays PAPER; that absence IS the
    // live gate, and it is the most load-bearing behaviour in `credentials.rs`.
    // -----------------------------------------------------------------------------------------
    Setting {
        name: "ALPACA_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ALPACA_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "ASTER_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BINANCE_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "BYBIT_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "CTRADER_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DERIBIT_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "DUKASCOPY_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "FXCM_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "HYPERLIQUID_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IBKR_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "IG_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OANDA_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "OKX_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_BROKER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_BUILDER_CODE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_DEMO_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_DEMO_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_DEMO_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_LIVE_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_LIVE_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_LIVE_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_MAINNET_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_MAINNET_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_MAINNET_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_SIM_API_KEY",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_SIM_API_PASSPHRASE",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    Setting {
        name: "POLYMARKET_SIM_API_SECRET",
        krate: "vike-bridge-core",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "",
    },
    // vike:new-venue:row // TODO(new-venue: {venue}): the twelve rows below are this venue's
    // vike:new-venue:row // WHOLE generated grid (`vike_model::credential_keys`' four tiers x
    // vike:new-venue:row // three suffixes). They need no decision — but they were APPENDED,
    // vike:new-venue:row // not merged: move them to this block's alphabetical position, then
    // vike:new-venue:row // delete this comment. `{VENUE}_BROKER_CODE`/`{VENUE}_BUILDER_CODE`
    // vike:new-venue:row // join them if this venue's `vike_model::attribution` arm ever stops
    // vike:new-venue:row // being `AttributionMechanic::None` — until then nothing looks them up.
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_DEMO_API_KEY",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_DEMO_API_PASSPHRASE",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_DEMO_API_SECRET",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_LIVE_API_KEY",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_LIVE_API_PASSPHRASE",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_LIVE_API_SECRET",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_MAINNET_API_KEY",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_MAINNET_API_PASSPHRASE",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_MAINNET_API_SECRET",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_SIM_API_KEY",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_SIM_API_PASSPHRASE",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
    // vike:new-venue:row Setting {
    // vike:new-venue:row     name: "{VENUE}_SIM_API_SECRET",
    // vike:new-venue:row     krate: "vike-bridge-core",
    // vike:new-venue:row     scope: Scope::Venue,
    // vike:new-venue:row     layer: Layer::Injected,
    // vike:new-venue:row     naming: Naming::MapLookup,
    // vike:new-venue:row     default: "",
    // vike:new-venue:row },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry is keyed on `(name, krate)`, not `name`: a variable read by several crates
    /// gets one row per crate, because each carries its own default and its own evidence.
    /// A duplicated PAIR means two rows disagree about the same read site.
    #[test]
    fn registry_rows_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for s in SETTINGS {
            assert!(
                seen.insert((s.name, s.krate)),
                "duplicate registry row for {} in {}",
                s.name,
                s.krate
            );
        }
    }

    /// `Scope` must agree with the name in both directions: a `VIKE_`-prefixed name is always
    /// `Scope::Vike`, and — the half the earlier version of this test could not catch — a row
    /// claiming `Scope::Vike` must actually carry that prefix.
    #[test]
    fn scope_matches_the_name_prefix() {
        for s in SETTINGS {
            let prefixed = s.name.starts_with("VIKE_");
            assert_eq!(
                prefixed,
                s.scope == Scope::Vike,
                "{} has VIKE_ prefix = {prefixed} but declares {:?}",
                s.name,
                s.scope
            );
        }
    }

    /// `Naming::Konst` names the Rust constant the value reaches `env::var` through; an
    /// empty identifier would make the Task-5 cross-check vacuous.
    #[test]
    fn konst_naming_carries_an_identifier() {
        for s in SETTINGS {
            if let Naming::Konst(ident) = s.naming {
                assert!(!ident.is_empty(), "{} declares Konst with an empty ident", s.name);
            }
        }
    }

    /// Layer and Naming must agree on every row: an `Injected` row reads a caller-supplied map,
    /// and a `Library` direct-read row never does — `layer_for` only ever classifies a
    /// non-bin/test/build file as `Library` when the read was a direct `env::var` (its
    /// `injected` branch returns `Injected` instead), so `Library` + `MapLookup` can never occur
    /// honestly. Checks EVERY row — a `.find()` spot-check would stop catching mistakes as soon
    /// as the table grows.
    ///
    /// `Binary` is deliberately NOT constrained here (unlike an earlier version of this test):
    /// a `main.rs`/`src/bin/*.rs` can legitimately read its own locally-loaded credentials map
    /// (`load_workspace_dotenv().get("NAME")`) instead of `env::var` — e.g.
    /// `vike-backfill/src/bin/tardis_backfill.rs`'s `TARDIS_API_KEY` — which is Naming::MapLookup
    /// at Layer::Binary, a real and correct shape, not a STEP-2 violation.
    #[test]
    fn layer_and_naming_agree_on_every_row() {
        for s in SETTINGS {
            match s.layer {
                Layer::Injected => assert_eq!(
                    s.naming,
                    Naming::MapLookup,
                    "{} is Layer::Injected so it must be read via a map lookup",
                    s.name
                ),
                Layer::Library => assert_ne!(
                    s.naming,
                    Naming::MapLookup,
                    "{} is Layer::Library (a direct read) but declares Naming::MapLookup",
                    s.name
                ),
                Layer::Binary | Layer::TestOnly | Layer::BuildScript => {}
            }
        }
    }

    /// A row's `default` is that CRATE's fallback, so two crates reading one variable may
    /// legitimately disagree. Guard the invariant that actually matters: a default is either
    /// empty (unset means "off") or non-blank — never whitespace, which would silently read as
    /// a real value in the operator table.
    #[test]
    fn defaults_are_empty_or_meaningful() {
        for s in SETTINGS {
            assert!(
                s.default.is_empty() || !s.default.trim().is_empty(),
                "{} in {} declares a whitespace-only default",
                s.name,
                s.krate
            );
        }
    }
}
