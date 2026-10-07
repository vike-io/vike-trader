//! **The LIVE mount** — [`live_mount`] and [`live_mount_with`], the arm `run` takes when
//! `flags.tradehub_live` is on: the wired-market `build_node` core over the resolved mount set, the
//! B11 live-account lock claims over the ARMED set (after the `data_only` withhold, before
//! `build_node` builds a client), the per-venue feed wiring, and the reconcile driver.
//!
//! Cut VERBATIM out of `crates/vike-tradehub/src/tradehub_cli.rs`, whose `//!` block stays the
//! lifecycle contract and whose `use` block this child shares (`use super::*`): the two are one
//! composition root split for size, not two modules with a boundary between them.
//!
//! ⚠ Text gates read this file TOGETHER with its parent, in that order:
//! `crates/vike-ops/tests/wiring/live_lock_claim_order_gate.rs`'s `Root::continued_in` follows the
//! sentinel directory from `run`'s `lock_dir` (parent) through `live_mount`'s and
//! `live_mount_with`'s parameters (here) to `LiveLock::acquire`, BY INDEX — a new parameter goes at
//! the END of either signature. `scripts/run_mutations.sh`'s B1 plants beside the claim here.

use super::*;

/// What a LIVE mount hands back: the core, its teardown handles, `build_node`'s own arming record,
/// and the B11 live-account lock claims.
///
/// ⚠ The fourth element is HELD, never read — its LIFETIME is the claim (see
/// `vike_ops::live_lock`). `main` binds it for the whole session, so a second live process on any
/// of these accounts is refused for exactly as long as this one can trade. Returning it rather than
/// storing it in [`LiveTeardown`] is deliberate: the teardown struct is DESTRUCTURED at the top of
/// the shutdown path, which would release every claim while the core is still joining and its
/// cancel sweep is still running orders.
pub(super) type LiveMount = (
    CoreHandle,
    LiveTeardown,
    std::collections::HashSet<String>,
    Vec<vike_ops::live_lock::LiveLock>,
);

/// The LIVE mount (`VIKE_TRADEHUB_LIVE=1`): stand up the wired-market [`vike_mount::build_node`] core
/// with the resolved strategy mounts folded in (split-plane I10: one for the historical
/// single-mount profile, N for a `[[mounts]]` one), then wire each DISTINCT mounted venue's live
/// market feed onto it. Returns the live
/// [`CoreHandle`] plus the [`LiveTeardown`] handles. HARD-errors (never a silent paper fallback) on any
/// venue/symbol the daemon has not wired for live — the daemon's own allow-list (safety gates #4/#5).
/// The PER-VENUE credential gate (#2) and each venue's own network gate (#3, decision 0095: the
/// ceiling for binance/bybit/okx/hyperliquid) still apply INSIDE `build_node`, so a credential-less
/// venue mounts paper even here (no real orders).
///
/// `risk_profile` is the resolved `--profile`/`VIKE_RUN_PROFILE` [`vike_core::RunProfile`]'s `[risk]`
/// table (RunProfile wiring — closing the live gap): threaded straight into [`NodeConfig::risk_profile`],
/// which `build_node` applies uniformly to every wired venue's `RiskLimits` via
/// `vike_mount::make_engine`. `None` (no `--profile`/`VIKE_RUN_PROFILE`, today's only path before this
/// wiring) leaves every venue's `RiskLimits` byte-identical to before — this is the SAME budget
/// `resolve_paper_risk_limits` already arms on the PAPER mount above, now also reaching the LIVE one.
/// The caller (this file's `main`) resolves this through
/// [`vike_core::RunProfile::risk_for_live_venue_mount`] rather than reading `.risk` directly, so by
/// the time it reaches this function it is guaranteed to have come from a `mode = "live"` profile —
/// `make_engine`'s hardcoded `GridSource::VenueFetched` never contradicts the profile it came from.
///
/// `policy` is this MACHINE's `policy` settings, resolved once by [`resolve_settings`] at the
/// top of [`main`] and projected here onto [`vike_mount::MountPolicy`] — the subset a venue mount
/// applies (settings-unification Phase 6c). A DIFFERENT authority from `risk_profile`: per-machine
/// and admin-owned, with no env and no CLI layer at all.
///
/// ⚠ Its `venues` field is the per-venue ARMING CEILING and its default is `paper` EVERYWHERE, so
/// `Policy::default()` (no file) mounts an ALL-PAPER daemon regardless of the credential store —
/// see this module's doc. `market_slippage` is the other binding field, consumed by the hyperliquid
/// bridge's mount as `MountRequest::market_slippage` (this daemon's primary live venue, and the
/// only roster venue with no native market order, so its every market intent and every tripped
/// stop-MARKET is priced at that band); for that one, no file leaves the mount on hyperliquid's own
/// compiled-in literal, byte-identically.
///
/// `flags` is the resolved [`vike_config::Flags`] — this arm consumes `reconcile`,
/// `tradehub_record` and `oco_cancel_sibling_on_dead_exit`, each still overridden by its own
/// variable inside the loader. They arrive as a parameter rather than being read here so that ONE
/// load decides them for the whole process.
///
/// `mounts` is the resolved mount SET (split-plane I10: N strategies / N venues, one row for the
/// historical single-mount profile) — each entry carries its strategy (the A-S maker by default,
/// or whatever that profile row's `[strategy]` table named), its A-S lowering `cfg` (this
/// function's FEED wiring reads it: `cfg.token_id`/`cfg.interval` name what to subscribe, and the
/// Polymarket arm's `TickBarSynthesizer` window is `cfg.interval_ms`) and its strategy-free `spec`
/// projection ([`vike_mount::build_live_multi_strategy_core`] mounts on those). Per entry the two
/// agree by construction — the caller derives each `spec` from its `cfg`.
///
/// The FIRST entry is the PRIMARY mount: its `cfg` supplies the per-venue account seed
/// (`NodeConfig::seed_cash` / `CoreConfig::seed_cash`), exactly as the single-mount daemon always
/// did. Each DISTINCT venue's feed arm is wired exactly once, however many mounts share the venue
/// (subscriptions dedup per series inside the arm); the teardown handle carries one [`LiveFeeds`]
/// entry per venue.
///
/// ⚠ The `allow` matches [`live_mount_with`]'s, which has carried one since it was written, and for
/// the same reason: every parameter here is a FACT THIS FUNCTION MAY NOT RESOLVE ITSELF — the
/// policy, the flags, the sentinel directory, the origin claim, the WAL map — and bundling them
/// into a struct to satisfy a count would hide the one property
/// `crates/vike-ops/tests/wiring/live_lock_claim_order_gate.rs` follows through this signature by INDEX.
#[allow(clippy::too_many_arguments)]
pub(super) fn live_mount(
    mounts: Vec<ResolvedMount>,
    risk_profile: Option<vike_exec::ProfileRisk>,
    profile: Option<vike_core::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    state_dir: &std::path::Path,
    // `config.instance_origin` — see [`live_mount_with`]. LAST, and deliberately not beside
    // `flags`: `crates/vike-ops/tests/wiring/live_lock_claim_order_gate.rs` pins `state_dir`'s POSITION in
    // this signature (it is the live-account sentinel's directory, and the gate follows it by
    // index), so a new parameter goes after it or the pin measures the wrong argument.
    instance_origin: Option<vike_model::InstanceOrigin>,
    // The WAL's env map with `config.journal_dir` folded in — see [`journal_vars`]. A PARAMETER for
    // the same reason `instance_origin` is one: this function resolves no settings of its own, and
    // `main` resolves it ONCE so this core's `journal`, the paper mount's and the startup
    // disclosure name the same directory (the materializer's tail-follow was the second reader
    // until the `materialize` feature was deleted on 2026-09-22). AFTER `state_dir`, like every
    // other addition — see the note above.
    journal_vars: &HashMap<String, String>,
    // Every venue's `venue_setting` rows, read ONCE by `run` from the boot's settings directory
    // (`load_venue_settings_for`), carried on `MountPolicy::venue_settings` and handed to the feed
    // wiring, which reads the mark-stream rows. LAST, like every addition since the live-lock gate
    // pinned `state_dir`'s position.
    venue_settings: std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<LiveMount, String> {
    // The credentials map — the daemon binary owns this I/O (the light client crates must not).
    // Absent per-venue creds keep that venue PAPER even with the gate on (safety gate #2), and that
    // is unchanged by WHICH store supplied them: see [`workspace_credentials`].
    let vars = workspace_credentials();
    live_mount_with(
        mounts,
        risk_profile,
        profile,
        policy,
        flags,
        vars,
        state_dir,
        instance_origin,
        journal_vars,
        &ProdFeedCtors,
        venue_settings,
    )
}

/// [`live_mount`] with its two impurities as PARAMETERS — the credential map (production:
/// [`workspace_credentials`], the store the binary owns) and the feed-construction seam
/// (production: [`ProdFeedCtors`], whose defaults are the arms' own inline expressions). The
/// split exists for the deterministic data-only splice test
/// (`src/tradehub_cli/tests/feed_splice.rs`): a scripted constructor plus a fake-key map is what lets a
/// CI test drive the REAL mount path — plan gate, withhold, `build_node`, feed wiring — with no
/// store, no network and no weekday market.
///
/// The third return is `build_node`'s own `live_venues` record — the per-venue EXEC ARMING STATE
/// (`vike_mount::make_engine` inserts a venue exactly when it constructs a real exec client). It is
/// what the seam test asserts "the data-only declaration kept exec paper" against, and it is also
/// what `main` RENDERS: the ready banner's `mode` and every `WireMountRow::live` are derived from
/// it, so no paper-vs-live report can disagree with the mount that produced it.
///
/// ⚠ [`live_mount`] used to DISCARD it, on the reasoning that "the arming disclosures inside
/// [`wire_venue_feeds`] already read it". Those disclosures are `tracing` lines; the ready banner is
/// the string `docs/ops/tradehub-the CI box.md` and `deploy/vike-tradehub.service` both call the
/// authority on paper-vs-live, and it was being built from the PROFILE instead. MEASURED on the CI box,
/// one startup: nine venues in this set, one venue in that banner. Do not re-narrow the return.
// ⚠ MORE arguments than clippy's default allows, and the shape is the point rather than an
// oversight: this is the daemon's live composition seam and every parameter is an INJECTED
// IMPURITY — the resolved mount set, the operator budget, the run profile, the machine policy, the
// flags, the credential map, the state directory the B11 sentinels are claimed in, this
// deployment's origin claim, and the feed constructors. That list is what lets
// `src/tradehub_cli/tests/feed_splice.rs` drive the REAL mount path with no store, no network and no real
// keys; bundling them into a struct would hide exactly the substitutions those tests exist to
// make. `state_dir` was the argument that crossed clippy's threshold, and it cannot be resolved
// here: only the boot walk knows it.
// ⚠ A NEW parameter goes at the END, after `state_dir` — see that parameter's own note: the
// live-account sentinel gate follows it through this signature by INDEX. This comment used to
// carry a COUNT of the arguments, which is why it now does not.
#[allow(clippy::too_many_arguments)]
pub(super) fn live_mount_with(
    mounts: Vec<ResolvedMount>,
    risk_profile: Option<vike_exec::ProfileRisk>,
    // The SAME run profile `risk_profile` was derived from, carried WHOLE so its `[guards]` and
    // `[sinks]` tables can reach the `CoreConfig` below. Threading it stopped at `[risk]` before:
    // every guard an operator wrote was parsed, validated, announced as ignored — and dropped.
    profile: Option<vike_core::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    mut vars: HashMap<String, String>,
    // Where the B11 live-account sentinels are claimed (`<state_dir>/LIVE-<venue>.lock`) — the
    // daemon's own `booted.state_dir`, a PARAMETER because the claim is made here (after the
    // `data_only` withhold, before `build_node`) and this function resolves no paths of its own.
    state_dir: &std::path::Path,
    // `config.instance_origin` — this deployment's origin claim, stamped into every client order
    // id this core mints so a SECOND instance on the same venue account is recognisable on
    // reconcile rather than anonymous (`vike_model::instance_origin`). A PARAMETER because this
    // function resolves no settings of its own; `None` (the default) is byte-identical to before
    // the key existed.
    //
    // ⚠ AFTER `state_dir`, not beside `flags`, and that is a constraint rather than a preference:
    // `crates/vike-ops/tests/wiring/live_lock_claim_order_gate.rs` follows the sentinel directory through
    // this signature BY INDEX, so a parameter inserted ahead of `state_dir` makes that gate pin
    // the wrong argument. Add new ones at the end.
    instance_origin: Option<vike_model::InstanceOrigin>,
    // `config.journal_dir` folded over the process env — see [`journal_vars`] and [`live_mount`].
    journal_vars: &HashMap<String, String>,
    make: &dyn FeedCtors,
    // Every venue's `venue_setting` rows, read ONCE by `run` from the boot's settings directory
    // (`load_venue_settings_for`): carried on `MountPolicy::venue_settings`, and read by
    // `wire_venue_feeds` for the mark-stream rows — one snapshot for both. LAST, like every
    // addition since the live-lock gate pinned `state_dir`'s position.
    venue_settings: std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<LiveMount, String> {
    // The `VIKE_RECONCILE_*` FAMILY's env map — cadences, lookbacks, the policy name, the balance
    // tolerances, AND the two sibling FLAGS (`reconcile_generate_missing`, `reconcile_balance`),
    // all built from this one map by `reconcile_config::build_recon_config` below.
    //
    // ⚠ This paragraph used to say those two flags were "recorded as unconsumed in
    // `vike_config::CONSUMPTION` rather than half-wired here", and the very next line folds them —
    // a written claim that was false about the code it sat on, which is the exact defect class this
    // branch exists to remove. What is true is the argument BEHIND it: the family still moves as
    // ONE map, because `build_recon_config` reads the whole of it out of one map and nothing was
    // split off. `daemon_recon_env` folds in the QUARANTINE-FIRST policy default and then
    // `or_insert`s the two resolved flags over a base map that IS the process env — so an
    // `Environment=VIKE_RECONCILE_BALANCE=0` line is already present and is never replaced. See
    // [`daemon_recon_env_from`], whose precedence is pinned by
    // `the_process_env_beats_the_resolved_flag_in_the_reconcile_family`.
    //
    // ⚠ The MASTER GATE itself is decided further down, not here, and the move is deliberate: since
    // S2 the default is ON for a mount that arms a live venue account, and that fact is not known
    // until `vars` has been through the `data_only` withhold below and `vike_mount::armed_live_venues`
    // has been asked. Deciding it up here would either read a pre-withhold map (arming reconcile for
    // a venue this daemon deliberately leaves on paper) or force a second probe call that could
    // disagree with the one the lock claim uses.
    let recon_env = daemon_recon_env(flags);

    // ⚠ **FIRST, before anything reads `vars`.** The resolved flags have to be in the map before
    // `venue_feed_plan`, before `armed_live_venues` and a long way before `build_node`, because
    // every one of those asks the map a question a `flags` setting is now allowed to answer.
    // Folding later would give the earlier readers a different map from the later ones, which is
    // the "one flag, two places, disagreeing" failure `vike_config::flags`' module doc names.
    //
    // ⚠ It also runs BEFORE the `data_only` withhold below, which strips `vars` by `{VENUE}_`
    // prefix — so a folded key whose NAME started with an eligible venue's prefix would be counted
    // in that disclosure's `keys_withheld` as though the store had held one more credential. None
    // does: `no_folded_flag_key_collides_with_a_data_only_venue_prefix` holds that across both
    // tables, so a new folded key or a new `DATA_ONLY_VENUES` row cannot introduce one silently.
    fold_flags_into_vars(flags, &mut vars);

    // THE MOUNT'S POLICY, built ONCE and BEFORE the plan loop, because the loop asks the venue
    // registry which NETWORK each venue's accounts mount on (`venue_feed_plan` →
    // `venue_arming::feed_tier`/`cex_mainnet_enabled`) and that answer has to come from the very
    // value the mount is built from — a bridge's `resolve` that reads the `account` table or a
    // `venue_setting` row must see the same snapshot in both places, or the feed's network is a
    // second opinion again. Nothing in it reads `vars`, so building it ahead of the withhold pass
    // moves no answer; it is moved into the `NodeConfig` below, exactly as before.
    let mount_policy = vike_mount::MountPolicy {
        // ⚠ THE `account` TABLE IS READ HERE, in the composition root, and nowhere below it. The
        // mount needs it — dukascopy resolves WHICH LEGAL ENTITY an order reaches from an `account`
        // row's credential-key owner prefix (`crates/bridges/dukascopy/src/mount.rs`) — and so does
        // the arming projection, which must describe exactly what the mount will do. Until 2026-09-15
        // `vike-mount` opened the store itself, from a library file, at a directory taken from a
        // process global: the class `crates/vike-ops/tests/settings/settings_registry.rs`'s
        // `CREDENTIAL_STORE_PIN` ratchets down, and invisible to it because neither account reader
        // was one of `CREDENTIAL_STORE_READERS`' keyed names. Both are keyed now, and this root —
        // already pinned, already the box's one credential reader — performs the read.
        //
        // ONE snapshot, carried on the policy object both seams already receive, so no caller can
        // hand the projection and the mount different tables. Errors are carried VERBATIM rather
        // than swallowed: a store that exists and will not open refuses a labelled account by
        // NAMING the failure, where a swallowed error used to be re-reported as a bad `account` row.
        accounts: vike_bridge_core::account_directory::AccountDirectory::read(
            vike_bridge_core::credentials::load_workspace_accounts_from_env(process_env()),
            vike_bridge_core::credentials::load_workspace_account_keys_from_env(process_env()),
        ),
        // The `venue_setting` snapshot `run` read — cloned rather than moved, so the parameter
        // stays usable after `mount_policy` moves into the `NodeConfig` below.
        venue_settings: venue_settings.clone(),
        ..vike_mount::MountPolicy::from(policy)
    };

    // Safety gates #4/#5 — the daemon's venue+symbol ALLOW-LIST, resolved per MOUNT and BEFORE
    // `vars` is moved into the NodeConfig (split-plane I10: every mount must pass its own venue's
    // gate, and the venue set collects ONE plan per DISTINCT venue, in mount order — the plan is
    // venue+environment-derived, so mounts sharing a venue share its plan). Any unwired venue is a
    // HARD ERROR (never a silent paper fallback). The dispatch itself lives in `venue_feed_plan`
    // (`feeds.rs`), where `live_wired_venues_pin.rs` scans its arms. Safety gate #3 (feed side,
    // decision 0095) is now INSIDE that dispatch: `venue_feed_plan` takes the mount policy and asks
    // each venue's registry row which network its accounts dial.
    let mut venue_plans: Vec<(String, VenuePlan)> = Vec::new();
    for m in &mounts {
        let plan = venue_feed_plan(&m.cfg, &vars, &mount_policy)?;
        if !venue_plans.iter().any(|(v, _)| v == &m.cfg.venue) {
            venue_plans.push((m.cfg.venue.clone(), plan));
        }
    }
    // The DATA-PLANE-ONLY declarations (the `data_only` profile key, validated to the
    // credentialed-data venue set at load): withhold each declared venue's credentials from the
    // map `build_node`'s exec arms will read — AFTER the plan loop above, which is the ordering
    // the seam rests on (each credentialed-data plan CARRIES its resolved config, so the FEED
    // keeps the credentials exec loses; see [`vike_mount::startup::withhold_venue_credentials`]). An undeclared mount
    // leaves `vars` untouched, byte-identically. The startup line is the declaration's own
    // disclosure — the arming banner inside `wire_venue_feeds` then announces the resulting
    // paper exec per venue ([`data_only_arming`]).
    let data_only: std::collections::HashSet<String> = mounts
        .iter()
        .filter(|m| m.row.data_only_effective())
        .map(|m| m.cfg.venue.clone())
        .collect();
    for venue in &data_only {
        let withheld = vike_mount::startup::withhold_venue_credentials(&mut vars, venue);
        tracing::warn!(
            venue = %venue,
            keys_withheld = withheld,
            "DATA-ONLY mount declared for {venue} (`data_only = true` in the profile): its \
             credentials are WITHHELD from the exec mount, so exec stays on the PAPER book by \
             declaration while the credentialed market feed authenticates from the same store"
        );
    }
    // SAFETY GATE #6 — ONE live process per venue ACCOUNT (split-plane B11, the Danger-2
    // tripwire): claim `<state_dir>/LIVE-<venue>.lock` for every venue this mount is about to ARM,
    // BEFORE `build_node` constructs a single exec client. Two properties, and the daemon had
    // NEITHER of them until this call site existed:
    //
    // 1. THE SET. The claims used to be made in `main` over `mount_venues` — the RUN PROFILE's
    //    venues. That is a different question: the profile says which `(venue, symbol)` pairs carry
    //    a STRATEGY, while `build_node` arms an exec client for every `crate::wired_markets::WIRED_MARKETS`
    //    venue the credential store answers for and the ceiling permits. MEASURED on the CI box, one
    //    startup of the shipped daemon, two lines apart — `live_venues={"hyperliquid","deribit",
    //    "okx","bybit","alpaca","aster","binance","ig","oanda"}` beside `mode":"LIVE (venue=bybit)"`
    //    — nine live authenticated sessions behind ONE lock. `vike_mount::armed_live_venues` is the
    //    pre-mount probe that answers the arming question instead, and `build_node`'s own
    //    `refuse_unarmed_live_venues` backstop refuses the node if anything arms outside it.
    // 2. THE MAP IT READS. It is THIS `vars` — the map AFTER the `data_only` withhold above — and
    //    that ordering is load-bearing: a declared data-only venue has had its exec credentials
    //    taken away, so it will not arm, and claiming its account lock would refuse a legitimate
    //    second process for a venue this daemon deliberately leaves on paper. `main` could not have
    //    read this map at all; the withhold happens here.
    //
    // The claims are RETURNED, not dropped: `main` binds them for the whole session (the OS
    // releases them on death, so there is no stale-lock sweep). Wiring only — the mechanism, the
    // refusal message and the tests live in `vike_ops::live_lock`.
    let mut live_account_locks: Vec<vike_ops::live_lock::LiveLock> = Vec::new();
    // ⚠ A ROUTE KEY per ACCOUNT, not a venue id: `binance` for a venue's default account — so no
    // deployed sentinel filename moves — and `binance#ALT` for a second one. `LiveLock::acquire`'s
    // own doc has required exactly this since the route-key split landed, because keying on the
    // canonical venue would make ONE process mounting two accounts of one exchange refuse its own
    // second mount.
    // ⚠ The probe reads the SETTINGS, not this profile's mounts, and that is correct rather than an
    // omission: a second account ARMS on its `policy.accounts.<venue>.<LABEL>` line plus its own
    // credentials, and `make_engine_accounts` mounts it whether or not a `[[mounts]]` row names it.
    // So the set of sentinels to claim is the same set whatever this profile mounts — which is what
    // lets the claim be made HERE, before `build_node` has assembled anything.
    // ⚠ ONE call, TWO consumers. The armed set decides which sentinels to claim AND — since S2 —
    // whether this mount reconciles at all. Asking twice would let the lock claim and the reconcile
    // gate disagree about what this daemon is about to authenticate as, which is the one thing they
    // must never do.
    let armed_live = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &vars,
        &mount_policy,
    );
    for route_key in &armed_live {
        match vike_ops::live_lock::LiveLock::acquire(state_dir, route_key) {
            Ok(l) => live_account_locks.push(l),
            Err(e) => return Err(format!("refusing LIVE mount: {e}")),
        }
    }

    // THE RECONCILE MASTER GATE (S2) — ON by DEFAULT for a mount that arms a live venue account,
    // paired with `quarantine` so it folds nothing. One decision — in `vike-ops`, and called
    // identically by `vike-app`'s `App::new`, until the desktop lost its local core and the gate
    // moved here (2026-09-23) — so the GUI and the daemon could not differ about what a restart does.
    //
    // ⚠ THE CONSEQUENCE, stated where it happens: from here a live mount issues AUTHENTICATED READ
    // calls against every armed account at startup and then every `VIKE_RECONCILE_INTERVAL_MS`
    // (default 60 s), whether or not anybody asked. That is the price of not trading against a
    // belief, and it is bounded (a handful of REST reads per venue per minute, inside every wired
    // venue's published budget) — but aster has no testnet credentials in the store (its testnet
    // exists and is routed; only `ASTER_LIVE_*` is configured), so on a box that arms aster they are
    // MAINNET reads. `reconcile_gate`'s own doc carries the argument and the three ways to refuse.
    let recon_gate =
        reconcile_config::reconcile_gate(flags.reconcile, flags.reconcile_off, armed_live.len());
    // Disclosed at WARN whichever way it went, through the ONE shared emitter: "reconcile is on and
    // nobody asked" and "reconcile is off on a live box" are both things an operator must be able to
    // find in a journal without knowing which flag to grep for, and `vike-app` had to say it the same
    // way while it reconciled. See `reconcile_config::log_reconcile_gate`.
    reconcile_config::log_reconcile_gate(recon_gate, armed_live.len());
    let recon_enabled = recon_gate.enabled();

    // ...and the one CROSS-mount consistency gate, likewise before any core spawns: two polymarket
    // mounts on ONE token must agree on the interval, because they will share one `MakerSink`
    // whose bar synth runs at exactly one window (see `LiveFeeds::Polymarket`).
    #[cfg(feature = "polymarket")]
    check_poly_token_intervals(&mounts)?;
    // ...and its ctrader twin (split-plane I9): every ctrader mount shares one data socket and one
    // bar SYNTHESIZER window, so rows disagreeing on the interval are refused before any core
    // spawns — see `check_ctrader_intervals`.
    check_ctrader_intervals(&mounts.iter().map(|m| &m.cfg).collect::<Vec<_>>())?;

    // Opt-in PIT-`SymbolProperties` recorder (`VIKE_RECORD_PROPERTIES=1`). ⚠ ALWAYS `None` since
    // the daemon stopped writing the store (#2093, 2026-09-22, docs/decisions/0084):
    // `PropertiesRecorder` opens the concrete DataFusion store, which only a `record-feeds`/
    // `materialize` build carried (both enabled `vike-data/hist-datafusion`, and #2093 deleted
    // both); the recorder that survives runs inside the datahub, which opens its own store. So
    // nothing is constructed here, and `VIKE_RECORD_PROPERTIES` is inert in this binary whatever
    // sets it — the flag fold still writes it into `vars`, and no code downstream reads that key.
    //
    // ⚠ This comment described a construction HERE, through `PropertiesRecorder::open_from_vars`
    // over the folded `vars`, until 2026-09-28 — a call this function has not made since #2093.
    // What that call taught survives it, because it is why the key's fold tier is
    // [`FoldTier::Resolved`]: `open_from_env` is a one-line wrapper that calls `open_from_vars` over
    // `PropertiesRecorder::env_snapshot` — a map holding just this one variable, read from the
    // PROCESS ENV — so handing the map half the credential store's map instead REPLACES the
    // environment read rather than widening it, and an `or_insert` fold would let a stale
    // credential-store line beat an exported value in BOTH directions. A root that constructs one again
    // inherits that argument; `a_process_env_value_beats_a_file_value_for_every_wired_key` is the
    // proof and `PropertiesRecorder::gate` the exact-`"1"` grammar.
    let properties_rec: Option<Arc<vike_data::PropertiesRecorder>> = None;

    // The live core config carries the SAFETY KNOBS a live daemon must be capped with (NOT the paper
    // mount's minimal `CoreConfig::default()`). Field names/values mirrored vike-app's fat-build
    // CoreConfig (the desktop builds none since #1610): a 30s submit-ack backstop, a 25%
    // equity-drawdown liquidate-only latch, and the margin-call watchdog (inert at 1× until
    // leverage is raised). `recon_enabled` is the
    // S2 reconcile gate resolved above (a PAPER mount ⇒ `false`, byte-identical to the pre-reconcile
    // daemon: `build_node` builds no reconnect-trigger channel). `strategy`/`extra_mounts` are
    // left unset here — `build_live_multi_strategy_core` folds the resolved mounts into them
    // before `build_node` consumes the config.
    //
    // The PRIMARY (first) mount's `seed_cash` supplies the per-venue account seed, exactly as the
    // single-mount daemon always passed its one mount's — `build_node` applies `NodeConfig::
    // seed_cash` uniformly to every extra venue engine and `core_config.seed_cash` to the primary,
    // and a per-ROW seed would need a per-venue seed table `build_node` does not take (stated
    // rather than silently summed: `validate_for_live`'s seed refusal runs per row either way).
    let primary_seed = mounts[0].cfg.seed_cash;
    // THE ARMING ROWS, computed HERE and nowhere else in this function. The window is narrow and
    // both edges are load-bearing: AFTER `withhold_venue_credentials` above (so a `data_only`
    // venue's exec keys are already gone from `vars`, and the rows say paper for it, which is what
    // this daemon actually does) and BEFORE `vars`/`mount_policy` move into `NodeConfig` below
    // (after which neither exists to read).
    //
    // This is why `vike_mount::journal_venue_mounts` takes ROWS rather than the map. A signature
    // taking `vars` would let the call sit anywhere downstream and be wrong by a comment; taking
    // rows makes the caller pick this moment, and there is only one.
    let arming = vike_mount::venue_arming(crate::registry::REGISTRY, &vars, &mount_policy);

    // THE DEAD-MAN'S ABSENT-KEY WARNING, once, beside the construction it describes (the
    // `deadman:` field of the literal below). Here and not in `deadman_config_from_policy`, because
    // this is the composition root where `policy` is known to be a LIVE mount's — the fold is pure
    // and the gate-off `paper_mount` arm never calls it, so a warning inside the fold would be a
    // warning nobody could count. Fires for an ABSENT key only: `deadman_timeout_ms = 0` is the
    // operator's recorded decision and gets no line. `warn_deadman_absent`'s doc argues the shape.
    warn_deadman_absent(policy);

    // THE LINK DEAD-MAN's venue set and its per-venue report (M13). The set is the DISTINCT venues
    // this daemon mounts, in mount order — `venue_plans` above already collected exactly that, and
    // it is the right set rather than a convenient one: a feed thread exists only for a mounted
    // venue, so a venue outside it can never disclose a `FeedStatus` and arming it would watch a
    // link nothing reports on. The report is emitted here, at INFO, one line per venue, because
    // "which venues does my automatic stop actually cover" is a startup question and the answer is
    // a join of a policy key, a per-venue table and this mount's own venue list — none of which an
    // operator can compute from the file alone.
    // ⚠ The PLAN travels with the venue, not just its name: whether the switch can fire here is a
    // question about the LANES this daemon subscribed, and `mount_link_disclosure` is the reading
    // of the feed wiring that answers it. Without it the mount announced an armed switch on every
    // CEX venue while nothing could reach the latch for any of them.
    let link_venues: Vec<(String, MountLinkDisclosure)> =
        venue_plans.iter().map(|(v, plan)| (v.clone(), mount_link_disclosure(plan))).collect();
    for line in link_deadman_arming_report(policy, &link_venues) {
        tracing::info!("{line}");
    }

    let mut node_cfg = NodeConfig {
        // THE VENUE REGISTRY this daemon names the bridges in (docs/decisions/0096, amended
        // 2026-09-29): `build_node` names none, and is handed it here.
        registry: crate::registry::REGISTRY,
        // THE WIRED MARKETS (docs/decisions/0098): which venues the node mounts, on which symbol,
        // in which orders — the same table `armed_live_venues` above claimed its locks from.
        markets: crate::wired_markets::WIRED_MARKETS,
        vars,
        properties_rec,
        seed_cash: primary_seed,
        recon_enabled,
        risk_profile,
        // The machine's ceilings, projected onto what a venue mount applies. No `policy` rows ⇒
        // `Policy::default()` ⇒ `MountPolicy::default()` ⇒ every venue keeps its compiled-in
        // literal, byte-identical to this daemon before Phase 6c.
        //
        // ⚠ The SAME value the live-account lock claims above were computed from — one projection,
        // so the set that was locked and the set the arms resolve cannot disagree by construction.
        policy: mount_policy,
        core_config: vike_core::CoreConfig {
            seed_cash: primary_seed,
            // The origin claim this daemon's client order ids carry — the parameter above,
            // straight through. Consumed at a FRESH coid session only (a restart resumes its
            // persisted one verbatim): `vike_core::CoreConfig::instance_origin` argues why.
            instance_origin,
            // ⚠ These three are the daemon's DEFAULTS, not its answer: `profile`'s `[guards]` table
            // overwrites whichever of them it names, via `apply_guards_and_sinks` below. Before that
            // call existed they were the only values that could ever apply, and an operator's
            // configured guards were parsed, warned about and discarded.
            submit_ack_timeout: Some(Duration::from_secs(30)),
            max_drawdown: Some(0.25),
            margin_call: Some(vike_exec::MarginCallConfig::default()),
            // THE DEAD-MAN SWITCH, from the `policy` settings and nothing else (M4). OPT-IN: armed only
            // when the operator wrote `deadman_timeout_ms = <n>`; an absent key and an explicit
            // `0` both leave this `None`, and `warn_deadman_absent` above is what tells the two
            // apart. (This comment read "Armed by default — sixty seconds of ingest silence
            // cancels the book" for one morning; the key's doc records the reversal.)
            // `deadman_config_from_policy` is the whole fold and argues each half;
            // `crates/vike-config/tests/policy_is_consumed.rs` names THIS line. ⚠ Not one of the
            // three `apply_guards_and_sinks` overwrites below: a run profile has no `[guards]`
            // field for it, deliberately — a profile may not lower a policy ceiling, and
            // `the_run_profile_cannot_touch_the_deadman` pins that.
            // ⚠ When written, armed by the live GATE, not by any venue arming: this literal is
            // built before `build_node` decides per venue, so an all-paper or `data_only` run
            // under the gate carries the switch too (the key's doc states the shape and the open
            // decision).
            deadman: deadman_config_from_policy(policy),
            // THE CONNECTION-STATE DEAD-MAN (M13) — ARMED BY DEFAULT, unlike the switch above, and
            // the asymmetry is the whole point of the re-ruling: this one observes what the BRIDGE
            // reports about the link, so a market that merely closed cannot trip it, so it may
            // have an armed default. `link_deadman_config_from_policy` folds three things — the
            // policy grace (absent ⇒ armed at `DEFAULT_LINK_DEADMAN_GRACE_MS`, `0` ⇒ off), the
            // per-venue table `vike_model::link_deadman_default`, and the venues this mount
            // actually has — and returns `None` when the grace is off OR no mounted venue is
            // armed, so an FX-only daemon builds nothing. The per-venue INFO lines above say which
            // it was. `crates/vike-config/tests/policy_is_consumed.rs` names THIS line.
            // ⚠ Not reachable from a run profile's `[guards]`, deliberately, exactly like the
            // sibling: a profile may not lower a policy ceiling.
            link_deadman: link_deadman_config_from_policy(policy, &link_venues),
            // Honor the same opt-in write-ahead journal the paper mount does (off by default).
            // ⚠ The RESOLVED run profile answers when there is one; otherwise the map this binary
            // resolved, which is what lets `config.journal_dir` enable the WAL (no setting could
            // do that while this was a `vike-core` library env read, and `VIKE_JOURNAL_DIR` still
            // wins over the row inside [`journal_vars`]). The profile
            // rung is NOT a convenience: `journal_config_from` re-opens the file `VIKE_RUN_PROFILE`
            // names, so with a `run` ROW active and that variable still set the daemon would print
            // "which no longer decides anything" about a variable still deciding this sink. See
            // `crate::profile_rows::journal_config_for`.
            journal: crate::profile_rows::journal_config_for(profile.as_ref(), journal_vars),
            // OPT-IN OCO SIBLING-CANCEL ON A DEAD EXIT (off by default) —
            // `flags.oco_cancel_sibling_on_dead_exit`, still overridden by
            // `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`. When a released bracket exit terminates
            // UNFILLED, the default KEEPS the surviving OCO sibling so the position retains
            // whatever protection it still has; on, the sibling is canceled and the book is left
            // flat. Off ⇒ byte-identical to this daemon before the flag reached it.
            //
            // ⚠ vike-app was this flag's ONLY reader in the tree, so the behaviour existed in the
            // GUI and was UNREACHABLE on the server that runs unattended — the one deployment where
            // "leave the book flat when protection dies" is most likely to be what an operator
            // wants. Both mounts read it now (the paper arm via `PaperMountOpts`), so the rehearsal
            // and the live run cannot disagree about it either.
            oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,
            // OPT-IN SHUTDOWN POLICY (off by default) — `flags.cancel_orders_on_shutdown`, still
            // overridden by `VIKE_CANCEL_ORDERS_ON_SHUTDOWN`. OFF (the default, and the behaviour that has always
            // shipped) leaves every resting order LIVE AT THE VENUE when the daemon stops, with
            // nothing left running to manage it; ON cancels the book during teardown, inside the
            // `[daemon] shutdown_deadline_ms` budget. It never flattens — positions survive either
            // way.
            //
            // ⚠ Reachable on every stop that TEARS DOWN: an interactive `shutdown`/`quit`/Ctrl-D,
            // AND — since 2026-08-07 — SIGTERM, whose handler (installed first thing in `run`)
            // raises the same stop flag. Under `deploy/vike-tradehub.service` stdin is `/dev/null`,
            // so `systemctl stop` is the stop that matters, and it now reaches this sweep inside
            // `TimeoutStopSec=`; a binary older than that date killed the process before any of it
            // ran — see `docs/ops/kill-switches.md` §D, which says so to the operator.
            cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,
            // Runtime strategy mount/unmount (split-plane B5): resolve a wire `MountStrategy`'s
            // `[strategy]`-vocabulary spec through the daemon's own profile machinery
            // (`crate::mount_factory` — same refusals as a profile load). Absent this,
            // the core refuses every runtime mount. Grafted through the I10 rebase: the
            // relocated multi-mount NodeConfig must answer MountStrategy exactly as B5's did.
            strategy_factory: Some(crate::mount_factory::strategy_factory()),
            // Durable strategy state + runtime-mount topology (B5 residual closed): arms the
            // per-mount `<mount_id>.json` sidecars AND the `runtime_mounts.json` topology record
            // the resurrect below replays. `None` (no project, no `$VIKE_STATE_ROOT`) disarms
            // both — byte-identical to this daemon before the residual closed.
            state_dir: strategy_state_dir(),
            ..vike_core::CoreConfig::default()
        },
    };

    // ⚠ THE `[guards]`/`[sinks]` WIRING. Applied HERE — after the daemon's own defaults, before
    // `build_node` — because the profile is the auditable authority and the literals above are the
    // fallback. Five guards (`submit_ack_timeout_ms`, `submit_ack_confirm_grace_ms`,
    // `max_drawdown`, `conditionals_on_ticks`, `margin_call`) plus `sinks.equity_sample_ms` map
    // 1:1 onto `CoreConfig` fields that already existed; the converters already returned the right
    // types. Nothing called them. From #816 (which wired `[risk]`) until now, every one of those
    // keys was parsed, VALIDATED, reported as ignored by a `warn!` and then dropped — a settings
    // key that nothing reads, which this workspace deleted `Policy::max_total_exposure` for.
    //
    // The return's `unwired` is the set that is still unwired, which is DISCLOSED rather than
    // warned about generically: an operator who set `initial_trading_state = "halted"` must be told
    // that exact key did not arm, not handed a sentence about the whole section.
    //
    // ⚠ …and `confirm_grace` is the OTHER half of that return: the stage-1/stage-2 pairing evaluated
    // on the FINAL numbers — this daemon's own `submit_ack_timeout: Some(30s)` literal above, or
    // whatever the profile put in its place, against the grace the profile did or did not name. A
    // profile that RAISES the timeout and says nothing about the grace walks the untouched grace
    // under HARD LOWER BOUND (a) in silence, and nothing below this line could ever notice: an
    // undersized grace is not an error, it is a narrower margin, and it surfaces as a phantom
    // OrderRejected against an order the venue actually holds. It WARNS rather than refusing (see
    // `vike_core::ConfirmGraceHazard`, which argues that against
    // `docs/decisions/0013-degrade-vs-refuse.md`), and the sentence is rendered by the type so no
    // caller can drift from the arithmetic — and this daemon is the only one that applies a run
    // profile's guards (it named "this daemon and `vike-app`" until 2026-09-28).
    if let Some(p) = &profile {
        let vike_core::GuardsReport { unwired, confirm_grace } =
            p.apply_guards_and_sinks(&mut node_cfg.core_config);
        if !unwired.is_empty() {
            tracing::warn!(
                "run profile keys {unwired:?} are SET and reach no CoreConfig — they are parsed \
                 and validated but arm nothing in this daemon (see \
                 `vike_core::RunProfile::apply_guards_and_sinks` for why each one is still \
                 unwired); every other [guards]/[sinks] key IS applied"
            );
        }
        if let Some(hazard) = confirm_grace {
            tracing::warn!("{hazard}");
        }
    }

    // Fold the resolved strategies into `core_config.strategy` + `extra_mounts`, then build the
    // wired-market node. Per-venue exec is credential-gated inside `build_node` (absent creds ⇒ that
    // venue is paper), and `spawn_core_multi` wires applied-fill capture on each mount's own venue
    // engine. `recon_clients`/`recon_trigger` are captured (the reference mount needs them).
    //
    // ⚠ `build_live_multi_strategy_core` with ONE mount IS `build_live_strategy_core` (first mount
    // → `strategy`, an empty rest → `extra_mounts`), so the single-mount daemon builds the same
    // node it always did. `cfgs` is cloned out first — the feed wiring below still needs each
    // mount's A-S lowering after the specs move into the core.
    let cfgs: Vec<MakerMountConfig> = mounts.iter().map(|m| m.cfg.clone()).collect();
    let vike_mount::Node { handle, recon_clients, recon_trigger, live_venues, forwarder_stop } =
        vike_mount::build_live_multi_strategy_core(
            mounts
                .into_iter()
                .map(|m| vike_mount::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
                .collect(),
            node_cfg,
        )
        .map_err(|e| format!("build_live_multi_strategy_core: {e}"))?
        .node;

    // ...and RECORD what each venue was ASKED to be and what it BECAME. The `venue_mounted`
    // channel's writer, called HERE and not from `main` for exactly the reason the lock claim
    // above gives: `vars` is the POST-withhold map. A `data_only` venue's exec credentials are
    // gone from it by now, so journalling from `main` — where `live_venues` is conveniently in
    // scope — would predict against the PRE-withhold map and record a venue this daemon
    // deliberately keeps on paper as one that was refused for a credential reason. Same trap the
    // sentinels had to move in here to avoid.
    //
    // Placed AFTER the node is built because the whole value of the record is the pairing: the
    // prediction (`vike_mount::venue_arming`, which the arming screen renders) against the OUTCOME
    // (`live_venues`). Before this line the second half does not exist.
    //
    // Failures WARN and continue. A journal that cannot be written must never stop a daemon that
    // has already mounted its venues — the ledger is evidence, not a gate.
    for r in vike_mount::journal_venue_mounts(
        Some(state_dir),
        &arming,
        &live_venues,
        env!("CARGO_PKG_VERSION"),
        vike_model::now_ms(),
    ) {
        if let Err(e) = r {
            tracing::warn!(error = %e, "venue mount record not journalled");
        }
    }

    // Resurrect RUNTIME strategy mounts (B5 residual closed): replay the topology sidecar through
    // the SAME lossless command lane a wire MountStrategy is lowered into — AFTER the core
    // spawned, BEFORE `wire_venue_feeds` below arms a single feed. The ingest lane is FIFO, so
    // every resurrected mount (and its `<mount_id>.json` state load, part of the mount arm) folds
    // ahead of the first market message — `resurrect_runtime_mounts`'s ordering contract. Nothing
    // here can fail the mount: a stale record skips with a warn, a corrupt file reads empty.
    if let Some(dir) = strategy_state_dir() {
        let outcome =
            crate::mount_factory::resurrect_runtime_mounts(&dir, |c| handle.send_command(c));
        if outcome.sent + outcome.skipped > 0 {
            tracing::info!(
                sent = outcome.sent,
                skipped = outcome.skipped,
                "runtime-mount resurrect replayed the topology sidecar"
            );
        }
    }

    // Carry the reconcile ingredients across the feed build ONLY when the S2 gate said yes — the
    // `spawn_recon` mount runs AFTER the feeds so a feed-subscribe error returns BEFORE any
    // `vt-core-recon` thread is spawned (no orphaned driver on the error path). OFF (a paper mount): the
    // handles are moved into the tuple and DROPPED right here — matching the pre-reconcile daemon's
    // `..`-destructure drop timing exactly (`recon_trigger` is already `None`, since `build_node` gates
    // the channel on the same flag) — so no ingredients survive to mount a driver. This is the ONLY
    // unconditional new binding, and its OFF-path effect is a byte-identical early drop.
    let recon_ingredients = recon_enabled.then_some((recon_clients, recon_trigger));

    // ⚠ The daemon no longer records its own feed (0084: the store has ONE writer plane, and the
    // recorder that survives runs inside the datahub with its watchdog, silence detection and
    // record profiles — none of which the removed `record-feeds` tee had). `post_feeds` stays as
    // the teardown slot so the sequential TAIL below keeps its shape; it is always `None` now.
    let post_feeds: PostFeeds = None;

    // Identity: nothing is teed off the venue's base sink any more.
    let wrap = |base: Arc<dyn LiveDataSink>| -> Arc<dyn LiveDataSink> { base };

    // Wire the venue's live market feed onto the core's ingest lanes.
    // Wire each mounted venue's live market feed onto the core's ingest lanes — ONE feed arm per
    // distinct venue, however many mounts share it (subscriptions dedup per series INSIDE the
    // arm). The arms live in `wire_venue_feeds`, directly above `live_mount`, where
    // `cex_feed_wiring_pin.rs` scans them.
    let mut feeds: Vec<LiveFeeds> = Vec::with_capacity(venue_plans.len());
    for (venue, plan) in &venue_plans {
        let venue_cfgs: Vec<&MakerMountConfig> =
            cfgs.iter().filter(|c| &c.venue == venue).collect();
        feeds.push(wire_venue_feeds(
            plan,
            &venue_cfgs,
            &handle,
            &live_venues,
            &arming,
            &data_only,
            &wrap,
            make,
            &venue_settings,
        )?);
    }

    // Reconcile driver mount — the reference was `vike-app`'s `App::new` recon_driver
    // block (gone with the desktop's local core). Placed AFTER the feeds are up so a
    // feed-subscribe error above returns before this ever spawns a thread. `recon_ingredients` is
    // `Some` only when `recon_enabled` (the OFF path already dropped the handles above) ⇒ this
    // whole mount is byte-identically absent on a PAPER mount and whenever the operator refused it.
    // `spawn_recon` owns its own `vt-core-recon` thread and respects the single-writer rule (it
    // only blocking-fetches REST reports and enqueues `Command::ReconcileReports` for the fold thread —
    // see that module's doc); it ADOPTS the pre-built `recon_trigger` pair so a venue reconnect poke
    // reaches this same driver.
    let recon_driver = match recon_ingredients {
        None => None,
        // Enabled but every venue is paper (no creds ⇒ no `ReconClient`): inert, no thread — mirrors
        // the `recon_clients.is_empty()` arm vike-app had. The (empty) clients + `recon_trigger`
        // drop here.
        Some((clients, _trigger)) if clients.is_empty() => {
            // ⚠ This arm is REACHABLE now in a way it was not before S2: the armed-live probe is
            // INTENT-based (`vike_mount::armed_live_venues`' own doc says so), so a venue whose
            // synchronous connect fails or whose factory declines a present-but-bad key probes live,
            // turns the default gate on, and then hands back no client. Saying so plainly beats a
            // silent no-op, because "reconcile is on" and "reconcile reached a venue" are different
            // facts and an operator reading the gate's own disclosure above has only the first.
            tracing::warn!(
                "reconcile is ON but no venue produced a ReconClient (every armed venue fell back \
                 to paper); the driver is inert this session and NOTHING is being reconciled"
            );
            None
        }
        Some((clients, recon_trigger)) => {
            // The per-venue feed-status health map, from the feeds THIS mount actually built
            // ([`LiveFeeds::recon_feed_statuses`]). `build_node` builds no market feeds of its own
            // (they stay with the caller — see `crates/vike-mount/src/node.rs`'s module doc), so the mounted venue's
            // own `Feeds::status` handle is the only one that exists, and it gates ONLY that venue:
            // every other reconciled venue is absent from the map, reads `Healthy`, and is never
            // health-blocked (`crates/vike-tradehub/CLAUDE.md`'s health-gate bullet).
            //
            // ⚠ The map was UNCONDITIONALLY EMPTY before the CEX arm, and empty is still the right
            // answer for hyperliquid/polymarket — the gate can only ever SUPPRESS a pass, and a
            // wrongly-suppressed pass can stay suppressed, so a row is added per venue on evidence,
            // never by sweeping up whatever handles are in scope.
            let feed_statuses = recon_feed_statuses_of(&feeds);
            let recon_cfg = reconcile_config::build_recon_config(&recon_env, feed_statuses);
            tracing::warn!(
                // LEGS, not venues: one row per venue ACCOUNT since the per-account producer
                // landed, and equal to the venue count on every box with no `[accounts]` table.
                accounts = clients.len(),
                policy = ?recon_cfg.policy.default,
                "mounting the reconciliation engine (quarantine-first unless VIKE_RECONCILE_POLICY \
                 is set); a restarted live daemon re-adopts open venue orders/positions instead of \
                 a blind fresh core. See the reconcile-gate line above for WHY it is on"
            );
            Some(vike_core::spawn_recon(&handle, clients, recon_cfg, recon_trigger))
        }
    };

    Ok((
        handle,
        LiveTeardown { feeds, forwarder_stop, post_feeds, recon_driver },
        live_venues,
        live_account_locks,
    ))
}
