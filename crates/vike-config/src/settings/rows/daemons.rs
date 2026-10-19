//! The `SETTINGS` rows read by the two daemons, their clients, the mount and alerting.

use super::{external_cred, vike_cred, vike_env, vike_node};
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    // The three remaining VIKE_ALERT* rows moved from `vike-ops` to `vike-alerting` with the tree
    // that reads them (`delivery::webhook_configs_from_env`) — `krate_of` keys on the source path,
    // so a row left on the old crate would fail `every_declared_variable_is_read`.
    //
    // `VIKE_ALERTS`, the rules-file override, has no reader row: decision 0111 made the path the
    // derived `<settings>/state/alerts.json`, and `vike-config` refuses the variable.
    vike_env("VIKE_ALERT_TELEGRAM_CHAT_ID", "vike-alerting", ""),
    vike_env("VIKE_ALERT_TELEGRAM_TOKEN", "vike-alerting", ""),
    vike_env("VIKE_ALERT_WEBHOOK_URL", "vike-alerting", ""),
    // THE TOOL-FEEDS LANE'S TWO API KEYS (`crates/vike-datahub/src/tool_feeds/mod.rs`'s `FINNHUB_API_KEY` and
    // `FMP_API_KEY`): the data daemon reads each from ITS OWN box's credential store, one scoped
    // read per poll cycle (`StoreKeys`), to fetch earnings (Finnhub) and dividends (FMP) on the
    // server's schedule. They are third-party DATA keys, not venue credentials. `Injected`/`MapLookup`
    // so `LIBRARY_PIN` does not grow, and the rows that make `vike-cli secrets set FINNHUB_API_KEY`
    // legal (`vike_venues::credential_keys::STORE_KEYS_OUTSIDE_THE_GRID`, held to this table in both
    // directions). Absent is the ordinary state: the feed answers `NotConfigured` naming the key.
    // ⚠ The desktop used to read both (`vike-app-core`'s `ToolApiKeys`, from the process environment
    // and then the desktop's store); those rows left with that code — the desktop holds no provider key.
    external_cred("FINNHUB_API_KEY", "vike-datahub", ""),
    external_cred("FMP_API_KEY", "vike-datahub", ""),
    // The data daemon's own settings — its listen address and public-bind opt-in, the live
    // market-data plane, its resident set, the chart-gap seed lane and the store root — have no
    // reader rows: decision 0111 made each a row of the settings database (`config.datahub_*`,
    // `flags.datahub_*`, `config.store_root`), a client's dial address is `config.datahub_addr`,
    // and `vike-config` refuses every variable at startup.
    // The datahub's CONTROL-scope hub key (`docs/decisions/0025-datahub-remote-posture.md`):
    // the write scope — the `Backfill` store write plus every `Run*` verb, which compile
    // client-supplied Rhai server-side. Read by
    // `vike_hub_proto::auth::hub_keys_from_vars` out of the CALLER-supplied
    // credential map, so the row is `Injected`/`MapLookup` and the crate that owns the literal
    // is the one holding the shared primitive — the same shape (and the same reasoning) as the
    // `VIKE_TRADEHUB_CONTROL_KEY` / `vike-tradehub-client` row further down. The BINARY owns
    // the store read; that crate reads no environment and opens no file.
    //
    // ⚠ **The `krate` moved from `vike-datahub-client` on 2026-09-23 and the READ did not
    // change** — the module declaring these two literals moved to `vike-hub-proto`, below both
    // hub protocols, so a client would stop depending on its own peer. This registry is keyed
    // on the PAIR `(name, krate)`, which makes a file crossing a crate boundary a RE-KEY rather
    // than a new row; leaving the old key would have failed `every_declared_variable_is_read`.
    //
    // `Medium::ProcessEnv`, not `HubKeyMap`, although every DAEMON hands this parser its hub-key
    // map: `crates/vike-cli/src/boot.rs`'s `datahub_keyring` hands it the PROCESS environment
    // first, and one root that reads the environment is what the medium column records. A key
    // variable, so decision 0111 (verdict 5) leaves it where it is: `ENV_ALLOWLIST` names it.
    vike_env("VIKE_DATAHUB_CONTROL_KEY", "vike-hub-proto", ""),
    // The OBSERVE twin of the row above: the read scope (history + catalog). ⚠ Their joint
    // ABSENCE is the gate — with neither key configured the datahub authenticates nothing and
    // serves exactly as it did before 0025 was adopted, which is the credential-is-the-gate
    // idiom this workspace uses for venues.
    vike_env("VIKE_DATAHUB_OBSERVE_KEY", "vike-hub-proto", ""),
    Setting {
        name: "VIKE_HOLD_STORE",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "a temp dir is used when it is unset",
    },
    vike_env("VIKE_LOG_DIR", "vike-tradehub-core", ""),
    // ⚠ TOMBSTONE — the `("VIKE_POLY_TICKS", "vike-desktop")` row stood beside its `vike-desktop`
    // neighbour `crates/vike-config/src/settings/rows/gui.rs`'s `VIKE_POLY_COCKPIT_TOKEN` and is
    // DELETED. Unlike that neighbour (a QA seed the GUI still reads), this one armed
    // the Polymarket startup SUBSCRIBE loop inside the desktop's local market-data plane, which the
    // desktop cut removed outright — the GUI opens no venue socket at all now. It was read THERE and
    // nowhere else in the workspace, so the name has no row left in any crate; the desktop's own
    // tombstone beside the deleted feed block says the same thing from the other side. If the
    // subscribe loop is ever rebuilt behind the daemon, the row comes back keyed to THAT crate.
    // The preflight skip, read out of the CREDENTIAL map the trading daemon folds the resolved
    // `flags.preflight_skip` row into (`vike_tradehub_core::preflight::preflight_skipped`); no process
    // environment reaches it (decision 0111). `CREDENTIAL_MAP_FOLDS` names the fold.
    vike_cred("VIKE_PREFLIGHT_SKIP", "vike-tradehub-core", ""),
    // The `VIKE_RECONCILE_*` family has no reader row: decision 0111 made every member a row of the
    // settings database (`vike_tradehub::reconcile_config::ReconSettings` carries them), and
    // `vike-config` refuses each variable at startup.
    // `("VIKE_RECORD_PROPERTIES", "vike-datahub")` — the Polymarket taker-hold live smoke's own skip
    // gate — stood here until decision 0111's P7: the smoke arms its recorder itself, because the
    // production gate is a parameter now and the variable refuses startup.
    // The ROUTE's wire arm: `open_routed_history` hands this override to
    // `vike_secrets::CredentialStore::hub_keys`, so the datahub node pair is looked up under the same
    // project everything else on the box resolves to. A MAP LOOKUP off the sweep the calling
    // binary already owns, never a process read — this is library code, and every routed reader
    // (the backtesthub, the three `cheap_np` bins, the tearsheet) passes the map it had
    // already swept for its store root.
    //
    // ⚠ The READ is not new; its CRATE is. It sat in `crates/vike-backtesthub/src/backtest_cli.rs`
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
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, the same answer every other absent spelling resolves to",
    },
    // Task 3 of decision 0095: `poly_taker_hold_live_smoke.rs`'s own
    // `declare_polymarket_egress_from_the_settings_database` helper reads the override directly
    // (a `std::env::var` call in the TEST BINARY, same shape as the bridge-crate smokes'
    // `VIKE_SETTINGS_DIR` rows in `crates/vike-config/src/settings/rows/bridges.rs`),
    // ahead of declaring the Polymarket bridge's egress from the settings database it names.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-datahub",
        scope: Scope::Vike,
        layer: Layer::TestOnly,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk, i.e. whichever checkout the smoke runs in",
    },
    // ⚠ This declared `Naming::Literal` until decision 0088's B5 step moved
    // `ctrader_live_mount_smoke.rs` (whose `std::env::var("VIKE_SETTINGS_DIR")` call was the
    // Literal sighting) out of this crate's `tests/` and into `bridges/ctrader`'s own row (in
    // `crates/vike-config/src/settings/rows/bridges.rs`'s `VIKE_SETTINGS_DIR` block),
    // which already declared `Naming::Literal` for it independently. The one sighting left here was
    // `account_table_is_reachable.rs`'s `HashMap::insert("VIKE_SETTINGS_DIR", ..)`, a MapLookup
    // shape, and the row was re-keyed to match.
    //
    // ⚠ `Binary`/`Literal` again since decision 0111: the `incident` bin
    // (`crates/vike-tradehub-core/src/bin/incident.rs`'s `active_run_profile`) reads the override with a
    // literal `std::env::var` to find the settings database whose ACTIVE `run` row it captures —
    // it took the run profile from a FILE named by `--profile` / `VIKE_RUN_PROFILE` until then, and
    // that variable's `vike-tradehub-core` row went with it. The binary read outranks the test sighting.
    Setting {
        name: "VIKE_SETTINGS_DIR",
        krate: "vike-tradehub-core",
        scope: Scope::Vike,
        layer: Layer::Binary,
        naming: Naming::Literal,
        medium: Medium::ProcessEnv,
        default: "<none> → the project walk from the working directory",
    },
    // The TELEGRAM control channel (`vike_tradehub::telegram`). The split between these rows is the
    // whole point: the process-env master flag is read by the daemon BINARY, while the credentials
    // and the two ALLOWLISTS are parsed out of an already-loaded workspace `.env` MAP by the
    // library — the `auth::from_vars` shape, which is the STEP-2 target state, not a violation.
    // The token and a non-empty CHAT allowlist must both be present (plus `VIKE_TRADEHUB_CONTROL=1`)
    // or nothing is constructed; the USER allowlist is the one optional member — absent means
    // chat-only authorization, exactly as before it existed.
    vike_cred("VIKE_TELEGRAM_ALLOWED_CHAT_IDS", "vike-tradehub", ""),
    vike_cred("VIKE_TELEGRAM_ALLOWED_USER_IDS", "vike-tradehub", ""),
    vike_cred("VIKE_TELEGRAM_BOT_TOKEN", "vike-tradehub", ""),
    // The TRADEHUB pair's shared parser: its one production caller is the daemon
    // (`crates/vike-tradehub/src/hub_server.rs`'s `start_observe_server`), which hands it the hub-key
    // store's map — hence `Medium::HubKeyMap`. The clients that read the pair from the
    // environment spell the names in their own crates, under their own rows.
    vike_node("VIKE_TRADEHUB_CONTROL_KEY", "vike-tradehub-client", ""),
    // **The ADMIN-scope hub key** (`docs/decisions/0065`): the THIRD key, and the one the
    // account-administration wire verbs authenticate against. A separate name rather than a
    // wider Control grant is the whole authorization half of that record's barrier — the key
    // every desktop carries to place orders is NOT the key that writes key material.
    // Read from the caller-supplied hub-key MAP by
    // `vike_tradehub_client::auth::from_vars_with_admin`, which is called ONLY when the
    // daemon's own three-valued declaration armed the capability — so a box that merely holds
    // this key does not thereby arm the surface.
    vike_node("VIKE_TRADEHUB_ADMIN_KEY", "vike-tradehub-client", ""),
    vike_node("VIKE_TRADEHUB_OBSERVE_KEY", "vike-tradehub-client", ""),
];
