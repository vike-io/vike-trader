//! The `SETTINGS` rows read by the two daemons, their clients, the mount and alerting.

use super::{venue_map, vike_map};
use crate::settings::{Layer, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    venue_map("OKX_DEMO_API_PASSPHRASE", "vike-mount", ""),
    venue_map("POLY_LIVE_PK", "vike-mount", ""),
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
    // The HEADLESS alerting mount's rules file: the DAEMON's `main.rs` reads it through
    // `const ALERTS_ENV` (Layer::Binary — the correct shape) and passes the resolved path to
    // `persist::load_path`.
    vike_map("VIKE_ALERTS", "vike-tradehub", "<project>/settings/state/alerts.json"),
    vike_map("VIKE_ALERT_TELEGRAM_CHAT_ID", "vike-alerting", ""),
    vike_map("VIKE_ALERT_TELEGRAM_TOKEN", "vike-alerting", ""),
    vike_map("VIKE_ALERT_WEBHOOK_URL", "vike-alerting", ""),
    vike_map("VIKE_DATAHUB_ADDR", "vike-datahub", "127.0.0.1:7878"),
    // The explicit opt-in for a NON-LOOPBACK `VIKE_DATAHUB_ADDR` (the exact string "1"): the
    // datahub protocol authenticates nothing unless node keys are configured, so the bin
    // REFUSES to start on a non-loopback bind without it — `vike_datahub::server`'s
    // `bind_decision`, the tradehub guard's twin. ⚠ Setting it is NECESSARY, not sufficient:
    // that same guard takes the server's `ServerAuth` as a third input and refuses a key-less
    // non-loopback bind (`BindDecision::RefuseUnauthenticated`) even with this set — the
    // variable consents to being REACHABLE, never to serving the store write and the Rhai
    // compiler unauthenticated.
    // Env-only, unlike the tradehub pair: this binary loads no settings files at all.
    vike_map("VIKE_DATAHUB_ALLOW_PUBLIC_BIND", "vike-datahub", ""),
    // Where every ROUTED reader DIALS, since
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` made the wire the default
    // route. Read by `crates/vike-datahub-client/src/route.rs`'s `datahub_addr_for_bin` off
    // the sweep each bin's `main` already owned for `store_root`, through that crate's own
    // `DATAHUB_ADDR_ENV` — a SECOND spelling of `vike_config`'s constant, kept because
    // importing it would resolve the name to `vike-config` and take this read out of the sweep
    // entirely. `datahub_addr_env_matches_the_config_crate` is what pays for it.
    //
    // ⚠ **The `krate` moved from `vike-backtest` on 2026-09-23 and the ROW did not change
    // otherwise** — the read is the same read, in the same shape, one crate lower. It moved
    // because `vike-report` became the fourth reader and cannot name a crate of its own rank;
    // `vike_datahub_client::route`'s module doc carries the argument. This registry is keyed
    // on the PAIR `(name, krate)`, so a move like this is a re-key rather than a new row, and
    // leaving the old key would have failed `every_declared_variable_is_read` rather than
    // passing quietly.
    //
    // ⚠ The default is not that crate's to state: `history_route` falls through to
    // `vike_config::DEFAULT_DATAHUB_ADDR`, so the value below is the DATAHUB row's and is
    // rendered here rather than chosen.
    vike_map("VIKE_DATAHUB_ADDR", "vike-datahub-client", "127.0.0.1:7878"),
    // The datahub's CONTROL-scope node key (`docs/decisions/0025-datahub-remote-posture.md`):
    // the write scope — the `Backfill` store write plus every `Run*` verb, which compile
    // client-supplied Rhai server-side. Read by
    // `vike_node_proto::auth::node_keys_from_vars` out of the CALLER-supplied
    // credential map, so the row is `Injected`/`MapLookup` and the crate that owns the literal
    // is the one holding the shared primitive — the same shape (and the same reasoning) as the
    // `VIKE_TRADEHUB_CONTROL_KEY` / `vike-tradehub-client` row further down. The BINARY owns
    // the store read; that crate reads no environment and opens no file.
    //
    // ⚠ **The `krate` moved from `vike-datahub-client` on 2026-09-23 and the READ did not
    // change** — the module declaring these two literals moved to `vike-node-proto`, below both
    // node protocols, so a client would stop depending on its own peer. This registry is keyed
    // on the PAIR `(name, krate)`, which makes a file crossing a crate boundary a RE-KEY rather
    // than a new row; leaving the old key would have failed `every_declared_variable_is_read`.
    vike_map("VIKE_DATAHUB_CONTROL_KEY", "vike-node-proto", ""),
    // The OBSERVE twin of the row above: the read scope (history + catalog). ⚠ Their joint
    // ABSENCE is the gate — with neither key configured the datahub authenticates nothing and
    // serves exactly as it did before 0025 was adopted, which is the credential-is-the-gate
    // idiom this workspace uses for venues.
    vike_map("VIKE_DATAHUB_OBSERVE_KEY", "vike-node-proto", ""),
    // The ARM for the LIVE MARKET-DATA plane (the exact string "1", the workspace idiom for a
    // switch whose safe state is off). It makes this daemon the single subscriber to each
    // venue's book/depth/tape and pushes them to clients over the `MdSubscribe` stream.
    //
    // ⚠ It spends VENUE-API budget from the box's own IP, shared with the order-signing daemon,
    // which is why arming is an explicit operator act rather than a build fact: a binary
    // carrying `--features live-feeds` with this unset mounts no hub, opens no socket and does
    // not advertise `market_data` at all. Read via `vars.get` out of the ONE `std::env::vars()`
    // sweep `crates/vike-datahub/src/bin/vike-datahub.rs` owns, so the row is
    // `Injected`/`MapLookup` and `LIBRARY_PIN` does not grow.
    vike_map("VIKE_DATAHUB_LIVE", "vike-datahub", ""),
    // Tier R — the RESIDENT market-data set, as comma-separated `venue:symbol:lane` rows. A
    // resident key has a refcount FLOOR of 1 and is never released, so a hot symbol's ladder
    // paints on the first DOM open instead of waiting on a REST re-seed, and the daemon's traded
    // pair stays observable with no desktop attached. UNSET means an EMPTY resident set, which
    // is a real configuration: the hub still serves on-demand keys.
    vike_map("VIKE_DATAHUB_LIVE_RESIDENT", "vike-datahub", ""),
    // The CHART-GAP SEED lane's arming. The exact string "1"; unset is off, and off still
    // ANSWERS the verb (writing nothing) rather than refusing it — which is the leg that lets a
    // WRITE verb be `VerbScope::Read` at all
    // (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`). An armed lane lets an
    // OBSERVE-scope client have this box spend venue-API budget, which is why it is an operator
    // act and not a default.
    vike_map("VIKE_DATAHUB_CHART_SEED", "vike-datahub", ""),
    vike_map("VIKE_DATAHUB_STORE", "vike-datahub", "market_data/hist"),
    vike_map("VIKE_HIST_STORE", "vike-datahub", "market_data/hist"),
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "a temp dir is used when it is unset",
    },
    vike_map("VIKE_JOURNAL_DIR", "vike-mount", ""),
    vike_map("VIKE_LOG_DIR", "vike-mount", ""),
    // ⚠ TOMBSTONE — the `("VIKE_POLY_TICKS", "vike-desktop")` row stood beside its `vike-desktop`
    // neighbour `crates/vike-ops/src/settings/rows/gui.rs`'s `VIKE_POLY_COCKPIT_TOKEN` and is
    // DELETED. Unlike that neighbour (a QA seed the GUI still reads), this one armed
    // the Polymarket startup SUBSCRIBE loop inside the desktop's local market-data plane, which the
    // desktop cut removed outright — the GUI opens no venue socket at all now. It was read THERE and
    // nowhere else in the workspace, so the name has no row left in any crate; the desktop's own
    // tombstone beside the deleted feed block says the same thing from the other side. If the
    // subscribe loop is ever rebuilt behind the daemon, the row comes back keyed to THAT crate.
    vike_map("VIKE_PREFLIGHT_SKIP", "vike-mount", ""),
    vike_map("VIKE_RECONCILE", "vike-tradehub", ""),
    Setting {
        name: "VIKE_RECONCILE_AUDIT_MS",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "mirrors VIKE_RECONCILE_INTERVAL_MS (Some or None)",
    },
    vike_map("VIKE_RECONCILE_BALANCE", "vike-tradehub", ""),
    vike_map("VIKE_RECONCILE_BALANCE_TOL_ABS", "vike-tradehub", "1.0"),
    vike_map("VIKE_RECONCILE_BALANCE_TOL_REL", "vike-tradehub", "1e-4"),
    vike_map("VIKE_RECONCILE_GENERATE_MISSING", "vike-tradehub", ""),
    vike_map("VIKE_RECONCILE_INFLIGHT_MS", "vike-tradehub", ""),
    vike_map("VIKE_RECONCILE_INTERVAL_MS", "vike-tradehub", "60000"),
    vike_map("VIKE_RECONCILE_LOOKBACK_MS", "vike-tradehub", "3600000"),
    Setting {
        // ⚠ TWO functions answer for this name and they answer DIFFERENTLY, so the
        // default states the one an operator actually gets: `quarantine_first_default` folds
        // `quarantine` into the map at BOTH live mounts before `parse_policy` ever sees it, and
        // `parse_policy`'s own `hybrid` fallback is reachable only by a caller that skips the fold
        // or by a SET but unrecognised value (the fold is an `or_insert`: it defaults an ABSENT
        // variable and nothing else).
        // Saying "hybrid" here (as this row did until S2) would tell an operator their unset box
        // auto-applies `PositionDrift`, which is the opposite of what it does.
        // ⚠ THIS ROW IS A MERGE of two, performed 2026-09-23. A SECOND row stood below it,
        // `("VIKE_RECONCILE_POLICY", "vike-tradehub")`, whose own comment said its evidence was
        // WEAKER THAN A PRODUCTION READ: what kept it alive for `every_declared_variable_is_read`
        // was a LITERAL in `tradehub_cli.rs`'s test region
        // (`daemon_reconcile_policy_honors_explicit_override`), the direction-2 looseness
        // `crates/vike-ops/tests/settings_secrets/settings_registry.rs` documents. When `reconcile_config` moved
        // into `vike-tradehub` the two stopped being different claims — the production read and the
        // test literal are one crate's now — and the registry is keyed on the (name, krate) PAIR,
        // so keeping both would have been a duplicate key rather than two pieces of evidence. The
        // STRONGER row survives; the daemon still resolves this variable through
        // `daemon_recon_env` -> `quarantine_first_default`, which is what the weaker row was for.
        name: "VIKE_RECONCILE_POLICY",
        krate: "vike-tradehub",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "quarantine when absent; a set but unrecognised value is hybrid",
    },
    vike_map("VIKE_RECONCILE_STARTUP_DELAY_MS", "vike-tradehub", "2000"),
    Setting {
        name: "VIKE_RECORD_PROPERTIES",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "",
    },
    Setting {
        name: "VIKE_RUN_PROFILE",
        krate: "vike-mount",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        default: "",
    },
    // `vike_tradehub::profile_rows::select_run_profile` — the ENV rung of the run-profile
    // selection, since `docs/decisions/0057`'s question 3 was answered "the row wins". It is
    // `Injected` and not `Library`: the function takes the caller's already-swept map and looks the
    // name up in it, which is the shape `LIBRARY_PIN` ratchets everything else TOWARD, so this row
    // adds nothing to that ratchet. The precedence it sits at the bottom of is the store's active
    // row, then `--profile`, then this.
    vike_map("VIKE_RUN_PROFILE", "vike-tradehub", ""),
    vike_map("VIKE_STATE_ROOT", "vike-tradehub", "<none> → <project>/settings/state"),
    // The ROUTE's wire arm: `open_routed_history` hands this override to
    // `vike_secrets::resolve_node_keys`, so the datahub node pair is looked up under the same
    // project everything else on the box resolves to. A MAP LOOKUP off the sweep the calling
    // binary already owns, never a process read — this is library code, and every routed reader
    // (the compute daemon, the three `cheap_np` bins, the tearsheet) passes the map it had
    // already swept for its store root.
    //
    // ⚠ The READ is not new; its CRATE is. It sat in `crates/vike-backtest/src/backtest_cli.rs`
    // until 2026-09-23, when the route moved down so a second reader of that crate's own layer
    // rank could reach it. This registry is keyed on the PAIR `(name, krate)`, so a read crossing
    // a crate boundary is a NEW row rather than an edited one — and `vike-backtest` KEEPS its own
    // row, because `run_serve` still resolves the pair for the daemon it is about to bind.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-datahub-client",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default: "<none> → the project walk, the same answer every other absent spelling resolves to",
    },
    // Task 3 of decision 0095: `poly_taker_hold_live_smoke.rs`'s own
    // `declare_polymarket_egress_from_the_settings_database` helper reads the override directly
    // (a `std::env::var` call in the TEST BINARY, same shape as the bridge-crate smokes'
    // `VIKE_SETTINGS_DIR` rows in `crates/vike-ops/src/settings/rows/bridges.rs`),
    // ahead of declaring the Polymarket bridge's egress from the settings database it names.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // ⚠ This declared `Naming::Literal` until decision 0088's B5 step moved
    // `ctrader_live_mount_smoke.rs` (whose `std::env::var("VIKE_SETTINGS_DIR")` call was the
    // Literal sighting) out of this crate's `tests/` and into `bridges/ctrader`'s own row (in
    // `crates/vike-ops/src/settings/rows/bridges.rs`'s `VIKE_SETTINGS_DIR` block),
    // which already declared `Naming::Literal` for it independently. The one sighting left here is
    // `account_table_is_reachable.rs`'s `HashMap::insert("VIKE_SETTINGS_DIR", ..)`, a MapLookup
    // shape — the row is re-keyed to match rather than left pointing at a read that moved crates.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-mount",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::MapLookup,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // The TELEGRAM control channel (`vike_tradehub::telegram`). The split between these rows is the
    // whole point: the process-env master flag is read by the daemon BINARY, while the credentials
    // and the two ALLOWLISTS are parsed out of an already-loaded workspace `.env` MAP by the
    // library — the `auth::from_vars` shape, which is the STEP-2 target state, not a violation.
    // The token and a non-empty CHAT allowlist must both be present (plus `VIKE_TRADEHUB_CONTROL=1`)
    // or nothing is constructed; the USER allowlist is the one optional member — absent means
    // chat-only authorization, exactly as before it existed.
    vike_map("VIKE_TELEGRAM_ALLOWED_CHAT_IDS", "vike-tradehub", ""),
    vike_map("VIKE_TELEGRAM_ALLOWED_USER_IDS", "vike-tradehub", ""),
    vike_map("VIKE_TELEGRAM_BOT_TOKEN", "vike-tradehub", ""),
    vike_map("VIKE_TRADEHUB_CONTROL_KEY", "vike-tradehub-client", ""),
    // **The ADMIN-scope node key** (`docs/decisions/0065`): the THIRD key, and the one the
    // account-administration wire verbs authenticate against. A separate name rather than a
    // wider Control grant is the whole authorization half of that record's barrier — the key
    // every desktop carries to place orders is NOT the key that writes key material.
    // Read from the caller-supplied node-key MAP by
    // `vike_tradehub_client::auth::from_vars_with_admin`, which is called ONLY when the
    // daemon's own three-valued declaration armed the capability — so a box that merely holds
    // this key does not thereby arm the surface.
    vike_map("VIKE_TRADEHUB_ADMIN_KEY", "vike-tradehub-client", ""),
    vike_map("VIKE_TRADEHUB_CONTROL_RATE", "vike-tradehub", "20.0"),
    vike_map("VIKE_TRADEHUB_OBSERVE_KEY", "vike-tradehub-client", ""),
];
