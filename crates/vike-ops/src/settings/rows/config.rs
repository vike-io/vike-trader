//! The `SETTINGS` rows read by `vike-config`.

use super::vike_env;
use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(crate) const ROWS: &[Setting] = &[
    Setting {
        // The row for this RETIRED switch (decision 0095: the arming ceiling alone chooses the
        // network now), refused in TWO places, both in this crate. A SET process-env value is a
        // hard startup refusal (`vike_config::refuse_removed_env` — the switch is RETIRED, not
        // merely unread) and a value in the CREDENTIAL FILE is separately refused
        // (`vike_config::refuse_credential_file_arming`, looked up in the caller-supplied
        // credential map, hence `Injected`/`MapLookup`) for the reason every row in that table
        // exists: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Both refusals
        // point the operator at the same remedy — `policy.venues.binance` — because the ceiling is
        // the only thing that chooses the network now. See `vike_config::removed` and
        // `vike_config::arming`.
        name: "BINANCE_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095): `policy.venues.binance` chooses \
                  the network; also refused as a credential row",
    },
    Setting {
        // The row for this RETIRED switch (decision 0095: the arming ceiling alone chooses the
        // network now), refused in TWO places, both in this crate. A SET process-env value is a
        // hard startup refusal (`vike_config::refuse_removed_env` — the switch is RETIRED, not
        // merely unread) and a value in the CREDENTIAL FILE is separately refused
        // (`vike_config::refuse_credential_file_arming`, looked up in the caller-supplied
        // credential map, hence `Injected`/`MapLookup`) for the reason every row in that table
        // exists: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Both refusals
        // point the operator at the same remedy — `policy.venues.bybit` — because the ceiling is
        // the only thing that chooses the network now. See `vike_config::removed` and
        // `vike_config::arming`.
        name: "BYBIT_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095): `policy.venues.bybit` chooses the \
                  network; also refused as a credential row",
    },
    Setting {
        // The PROCESS variable of that name, and only it: decision 0095 moved `ctrader_authorize`
        // onto the credential store's map, which is what every cTrader mount reads (its
        // `bridges/ctrader` row, `crates/vike-ops/src/settings/rows/bridges.rs`'s
        // `CTRADER_CLIENT_ID`), and `vike_config::REMOVED_ENV` refuses a set process variable at
        // startup.
        name: "CTRADER_CLIENT_ID",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); the credential store holds the app \
                  pair, `vike-cli secrets set CTRADER_CLIENT_ID`",
    },
    Setting {
        // The PROCESS variable of that name, and only it — see `CTRADER_CLIENT_ID`'s row above.
        name: "CTRADER_CLIENT_SECRET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); the credential store holds the app \
                  pair, `vike-cli secrets set CTRADER_CLIENT_SECRET`",
    },
    Setting {
        // `ctrader_authorize`'s OAuth flow parameters were these three process variables until
        // decision 0095 made them the tool's command-line flags (`--redirect-uri`, `--scope`,
        // `--token-file`, with the variables' old defaults). Every booting root refuses them
        // through `vike_config::REMOVED_ENV` — which is the lookup these rows declare. The tool
        // boots nothing and refuses the same five out of its own sweep
        // (`crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `refuse_retired_variables`), a
        // table-driven check that resolves at no `.get(` site — so it is a sighting, not a row, and
        // no `bridges/ctrader` row declares a read of a retired name.
        name: "CTRADER_REDIRECT_URI",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ctrader_authorize --redirect-uri",
    },
    Setting {
        name: "CTRADER_SCOPE",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ctrader_authorize --scope",
    },
    Setting {
        name: "CTRADER_TOKEN_FILE",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ctrader_authorize --token-file",
    },
    Setting {
        name: "HYPERLIQUID_HIP3",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); flags.hyperliquid_hip3 in the \
                  settings database",
    },
    Setting {
        // The row for this RETIRED switch (decision 0095: the arming ceiling alone chooses the
        // network now), refused in TWO places, both in this crate. A SET process-env value is a
        // hard startup refusal (`vike_config::refuse_removed_env` — the switch is RETIRED, not
        // merely unread) and a value in the CREDENTIAL FILE is separately refused
        // (`vike_config::refuse_credential_file_arming`, looked up in the caller-supplied
        // credential map, hence `Injected`/`MapLookup`) for the reason every row in that table
        // exists: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Both refusals
        // point the operator at the same remedy — `policy.venues.hyperliquid` — because the
        // ceiling is the only thing that chooses the network now. See `vike_config::removed` and
        // `vike_config::arming`.
        name: "HYPERLIQUID_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095): `policy.venues.hyperliquid` \
                  chooses the network; also refused as a credential row",
    },
    Setting {
        // The row for this RETIRED switch (decision 0095: the arming ceiling alone chooses the
        // network now), refused in TWO places, both in this crate. A SET process-env value is a
        // hard startup refusal (`vike_config::refuse_removed_env` — the switch is RETIRED, not
        // merely unread) and a value in the CREDENTIAL FILE is separately refused
        // (`vike_config::refuse_credential_file_arming`, looked up in the caller-supplied
        // credential map, hence `Injected`/`MapLookup`) for the reason every row in that table
        // exists: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Both refusals
        // point the operator at the same remedy — `policy.venues.okx` — because the ceiling is
        // the only thing that chooses the network now. See `vike_config::removed` and
        // `vike_config::arming`.
        name: "OKX_MAINNET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095): `policy.venues.okx` chooses the \
                  network; also refused as a credential row",
    },
    Setting {
        // The settlement pollers' switches and the chain watcher's settings were `bridges/polymarket`
        // `Layer::Library` reads until decision 0095 (D4): code nothing starts takes its values as
        // PARAMETERS now, so no crate reads these; `vike_config::REMOVED_ENV` refuses each at
        // startup, which is the lookup these rows declare.
        name: "POLY_AUTO_REDEEM",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); AutoRedeemPoller::spawn's `enabled`, \
                  and nothing starts that poller",
    },
    Setting {
        name: "POLY_CHAIN_MAX_SPAN",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ChainRpcSettings::max_span, and \
                  nothing starts the chain readers",
    },
    Setting {
        name: "POLY_CHAIN_PROXY",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ChainRpcSettings::via_proxy, and \
                  nothing starts the chain readers",
    },
    Setting {
        name: "POLY_CHAIN_RPC_URL",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ChainRpcSettings::rpc_url, and \
                  nothing starts the chain readers",
    },
    Setting {
        name: "POLY_CHAIN_WATCH",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ChainWatchPoller::spawn's \
                  `enabled`, and nothing starts that watcher",
    },
    Setting {
        // The egress guard's two variables became its parameters (decision 0095): the polymarket
        // smokes pass `DEFAULT_EGRESS_PROBE`, and the order-placing ones `DUBLIN_EGRESS_COUNTRY`.
        name: "POLY_EGRESS_PROBE_URL",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); check_expected_egress's probe_url",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Since decision
        // 0095 the variable is ALSO refused outright at startup (`vike_config::REMOVED_ENV`) — see
        // `vike_config::arming`.
        name: "POLY_EXEC",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); flags.poly_exec is the setting; \
                  also refused as a credential row",
    },
    Setting {
        name: "POLY_EXPECT_EGRESS_COUNTRY",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); check_expected_egress's expected",
    },
    Setting {
        name: "POLY_HEARTBEAT",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); HeartbeatPoller::spawn's \
                  `enabled`, and nothing starts that poller",
    },
    Setting {
        // The SECOND row for this name, in the crate that REFUSES it in the CREDENTIAL FILE.
        // `vike_config::refuse_credential_file_arming` looks the name up in the caller-supplied
        // credential map (hence `Injected`/`MapLookup`) and fails startup when an ARMING value is
        // found there: `<project>/settings/secrets.env` is plaintext and parsed last-wins, so an
        // appended line must never be enough to put this process on a live venue. Since decision
        // 0095 the variable is ALSO refused outright at startup (`vike_config::REMOVED_ENV`) — see
        // `vike_config::arming`.
        name: "POLY_RECONCILE",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); flags.poly_reconcile is the \
                  setting; also refused as a credential row",
    },
    Setting {
        name: "POLY_REDEEM_HALT",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup, an empty value included (decision 0095); \
                  AutoRedeemPoller::spawn's `halted` beside the halt file",
    },
    // The nine retired Polymarket settings variables below — decision 0095 — each looked up by
    // `vike_config::refuse_removed_env` in its caller-supplied map (`crate::REMOVED_ENV`), which
    // refuses startup when one is set rather than reading it. `POLY_EXEC` and `POLY_RECONCILE`
    // already have their own `vike-config` row above (the pre-existing credential-file-arming
    // refusal) and are not repeated here.
    Setting {
        name: "POLY_EXEC_MARKETS",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.exec_markets in \
                  the settings database",
    },
    Setting {
        name: "POLY_PRESUBMIT_REGISTER",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.presubmit_register \
                  in the settings database",
    },
    Setting {
        name: "POLY_PROXY_ENABLED",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.proxy_enabled in \
                  the settings database",
    },
    Setting {
        name: "POLY_PROXY_HOST",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.proxy_host in the \
                  settings database",
    },
    Setting {
        name: "POLY_PROXY_PORT",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.proxy_port in the \
                  settings database",
    },
    Setting {
        name: "POLY_RATE_GATE",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.rate_gate in the \
                  settings database",
    },
    Setting {
        name: "POLY_SOCKS_PROXY",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.socks_proxy in the \
                  settings database",
    },
    Setting {
        name: "POLY_WS_PROXY_ENABLED",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.polymarket.ws_proxy_enabled \
                  in the settings database",
    },
    Setting {
        name: "POLY_WS_TOKENS_PER_SOCKET",
        krate: "vike-config",
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); \
                  venue.polymarket.ws_tokens_per_socket in the settings database",
    },
    Setting {
        name: "VIKE_ALLOW_WITHDRAW_KEYS",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); flags.allow_withdraw_keys in the \
                  settings database",
    },
    Setting {
        name: "VIKE_BINANCE_TRADE_LITE_FILL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.binance.trade_lite_fill in \
                  the settings database",
    },
    Setting {
        name: "VIKE_BYBIT_FAST_EXEC",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.bybit.fast_exec in the \
                  settings database",
    },
    // The env layer over `config.datahub_advertise_addr` (split-plane REQ-2): the
    // datahub dial address the vike-tradehub daemon ADVERTISES in `Welcome.features`
    // (`datahub=<addr>`) so a connected client with no explicit `datahub_addr` of its own
    // dials the datahub there. Read once by `vike_config::Config::apply_env` from the
    // caller-supplied map — the same wiring as its `VIKE_TRADEHUB_ADDR` sibling below.
    vike_env("VIKE_DATAHUB_ADVERTISE_ADDR", "vike-config", ""),
    // The env layer over `config.tradehub_advertise_addr`: WHICH BOX the
    // vike-tradehub daemon reports itself to be running on, stamped into the identity block
    // every published frame carries so a thin client can say which daemon it is attached to
    // (a tunnelled client's own socket address is always the tunnel mouth and names no box).
    // ⚠ Unlike its three address siblings, UNSET is not "off": the daemon discovers its own
    // source address from the routing table and reports that. This variable is the OVERRIDE
    // for what a route lookup cannot answer — a NAT's public face above all.
    // Read once by `vike_config::Config::apply_env` from the caller-supplied map.
    vike_env("VIKE_TRADEHUB_ADVERTISE_ADDR", "vike-config", ""),
    Setting {
        // The env layer over `config.backtest_addr` — the COMPUTE daemon's listen address
        // AND the address its clients dial, one key for both sides by the owner's choice (ruling 7).
        // Read once by `vike_config::Config::apply_env` from the caller-supplied map, exactly like
        // its `VIKE_TRADEHUB_ADDR` sibling further down, so the ladder
        // `--addr <v>` → this → `config.backtest_addr` → `DEFAULT_BACKTEST_ADDR` has ONE loader
        // deciding precedence rather than a second one inside a binary.
        name: "VIKE_BACKTEST_ADDR",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        default: "127.0.0.1:7880 (`vike_config::DEFAULT_BACKTEST_ADDR`), after `config.backtest_addr`",
    },
    Setting {
        // The VENUE-CATALOG lane's FORMER arming, re-keyed from `vike-datahub` to `vike-config` on
        // 2026-09-16 because the READ moved with the meaning.
        //
        // `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
        // flipped the default: the lane serves unless refused, and the refusal is the settings key
        // `flags.venue_catalog_off` (whose own row is `VIKE_DATAHUB_VENUE_CATALOG_OFF`, below).
        // This name configures NOTHING now. It is still LOOKED UP, once, by `vike_config::load`, to
        // WARN an operator whose value used to mean OFF — the `VIKE_RECORD_DVOL` row's shape, and
        // the row stays for that row's reason: the lookup is real, and the `default` says what it
        // now means.
        //
        // ⚠ The ASYMMETRY is the interesting half and is the inverse of the reconcile flip's: a
        // `=1` is SILENT (the belief it expresses is still true) and any other non-empty value is
        // the warned case. `vike_config::flags::venue_catalog_refusal_ignored` carries the table.
        name: "VIKE_DATAHUB_VENUE_CATALOG",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::ProcessEnv,
        default: "DEAD — warned at startup when its value was not `1`, configures nothing",
    },
    // The out-of-band kill switch's PATH OVERRIDE, retired (decision 0099): `vike_config::REMOVED_ENV`
    // refuses a process that still carries it, which is the one read of this name left, and an
    // injected lookup. The sentinel itself is unchanged — `crates/vike-bridge-core/src/halt.rs`'s
    // `resolve_halt_path` owns the precedence (the declared project's `settings/state/HALT`, else
    // the exe directory), `halt_path_arming_error` is why an unusable one is loud instead of
    // silent — and so are the two rows that used to stand here: `vike-bridge-core`'s `Library`
    // read (a `LIBRARY_PIN` ratchet SHRINK) and `vike-paper`'s `TestOnly` row, whose test now
    // engages the process-wide sentinel through a working directory instead of a variable.
    Setting {
        name: "VIKE_HALT_FILE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0099); the sentinel is always \
                  <project>/settings/state/HALT",
    },
    vike_env("VIKE_HIST_STORE", "vike-config", "<repo>/market_data/hist"),
    Setting {
        name: "VIKE_HL_OUTCOME",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); OutcomePoller::spawn's `enabled`, \
                  and nothing starts that poller",
    },
    // The env layer over `config.instance_origin`: this deployment's 1–4 character
    // origin tag, stamped into the client order id of every order it places so a SECOND
    // instance sharing the venue API key is recognisable on reconcile instead of anonymous
    // (`vike_model::instance_origin`). The ENV layer is the primary one here by design —
    // two containers from one image differ by exactly this variable. Read once by
    // `vike_config::Config::apply_env` from the caller-supplied map; a malformed value is a
    // hard startup error, because an instance silently running untagged is indistinguishable
    // from one that was never configured.
    vike_env("VIKE_INSTANCE_ORIGIN", "vike-config", ""),
    Setting {
        name: "VIKE_MARK_STREAMS",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.<venue>.mark_streams, one \
                  row per venue, in the settings database",
    },
    Setting {
        name: "VIKE_MARK_STREAMS_ASTER",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); venue.aster.mark_streams in the \
                  settings database",
    },
    Setting {
        // ⚠ REMOVED, not read — settings unification PHASE 5. This was the per-order notional
        // ceiling, read by `vike-desktop`'s `main.rs` (into `OrderLimits`) and by `vike-cli`'s
        // `cmd/verbs.rs` (the advisory preview guardrail). A ceiling any exported variable can
        // raise is not a ceiling, so BOTH reads were deleted and the value moved to
        // the `policy.max_notional_per_order` row (`vike_config::Policy`), which has
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
        medium: Medium::Refused,
        default: "REMOVED — refused at startup; use policy.max_notional_per_order",
    },
    vike_env("VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT", "vike-config", ""),
    Setting {
        name: "VIKE_PM_RESOLVE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); ResolvePoller::spawn's `enabled`, \
                  and nothing starts that poller",
    },
    vike_env("VIKE_RECONCILE", "vike-config", ""),
    // ⚠ SAFETY OVERRIDE — the REFUSAL of the S2 default-on live reconcile. `1` means "do not
    // ask my venues what they hold", which is why it has a row of its own rather than being
    // read as the negation of `VIKE_RECONCILE`: an operator looking for the off switch must
    // find it under its own name. `vike_config::flags`' `RECONCILE_OFF_ENV` is the reader.
    vike_env("VIKE_RECONCILE_OFF", "vike-config", ""),
    // ⚠ SAFETY OVERRIDE — the REFUSAL of the default-on VENUE-CATALOG lane, and the second
    // member of the `*_off` pair `VIKE_RECONCILE_OFF` above opened. `1` means "this datahub
    // serves no venue catalog"; the verb is still answered, having called no venue, and the
    // `venue_catalog` capability is absent from the handshake so a client can say which key is
    // set rather than draw an empty symbol list.
    //
    // It has a row of its own for the reason its sibling does: an operator looking for the off
    // switch must find it under its own name, not as the negation of an arming.
    // `vike_config::flags`' `VENUE_CATALOG_OFF_ENV` is the reader, and the ROW spelling
    // (`flags.venue_catalog_off`) is the one `docs/decisions/0066` rules is primary —
    // the owner's ruling was that the switch moves into settings.
    vike_env("VIKE_DATAHUB_VENUE_CATALOG_OFF", "vike-config", ""),
    // Re-keyed `bridges/deribit`/`Library` -> `vike-config`/`Injected` when the DVOL feed was
    // finally MOUNTED, and the row SURVIVES the flag's deletion with a changed job.
    // `vike_deribit::DvolRecorder` used to read this variable itself, from a constructor
    // nothing outside its own test module called; it then took the resolved flag as a
    // parameter and `apply_env` was the one remaining read.
    //
    // ⚠ That flag is now DELETED (`vike_config::DEAD_FLAG_KEYS`) — it was the tree's one key
    // with no reader on EITHER spelling — so this variable no longer CONFIGURES anything. It is
    // still read, once, by `vike_config::load`, to WARN an operator who exported it that it
    // does nothing. The row stays because the lookup is real; the `default` says what it now
    // means, the `REMOVED_ENV` rows' shape. A hard startup refusal was refused deliberately:
    // the variable configured nothing before the deletion either, so refusing it would stop a
    // correct daemon dead over a spelling that makes no belief false.
    vike_env("VIKE_RECORD_DVOL", "vike-config", "DEAD — warned at startup, configures nothing"),
    Setting {
        // Was `bridges/deribit`/`Library` — the idempotency-bucket width, resolved inside the DVOL
        // recorder's constructor beside the store it buckets for — until decision 0095 (D4) made the
        // cadence a PARAMETER of `DvolRecorder::with_cadence` (nothing constructs a recorder, so
        // nothing reads a setting for it). `vike_config::REMOVED_ENV` refuses a set one.
        name: "VIKE_RECORD_DVOL_CADENCE_MS",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0095); DvolRecorder::with_cadence's \
                  cadence_ms, and nothing constructs a recorder",
    },
    Setting {
        // The daemon run profile's FILE pointer, retired by decision 0111 (verdict 4): the run
        // profile is the ACTIVE `run` row of the settings database, written by
        // `vike-cli config bootstrap-run`, and no binary reads a profile file. Its three readers
        // went in the same change — `vike-core`'s library env read (a `LIBRARY_PIN` shrink), the
        // `incident` bin in `vike-mount`, and the daemon's own selection rung in `vike-tradehub` —
        // so the one read left is `vike_config::refuse_removed_env`'s lookup in the caller's map,
        // which fails startup while the variable is set. The row goes the day nothing refuses it.
        name: "VIKE_RUN_PROFILE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup (decision 0111); the run profile is the active \
                  `run` row, written by vike-cli config bootstrap-run",
    },
    Setting {
        // ⚠ REMOVED, not read. Nothing in this workspace consumes it: credentials are read from
        // the credential store (the settings database; `<project>/settings/secrets.env` only on a
        // box that has not migrated), in plaintext, and there is no second store and nothing to
        // unseal.
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
        medium: Medium::Refused,
        default: "REMOVED — refused at startup; credentials are plaintext in the settings directory",
    },
    Setting {
        // ⚠ NOT the state ROOT (`VIKE_STATE_ROOT`), despite the name: this is the strategy-state
        // SIDECAR directory (the per-mount `strategy_state::write_json_atomic` blobs). It predates
        // settings-unification Phase 2, which is exactly why that phase's root had to be called
        // `VIKE_STATE_ROOT` — one variable cannot mean both without an operator silently dumping
        // every strategy sidecar into the state root (or relocating the window layout while
        // pointing the sidecars somewhere). ⚠ Folding this under the state root WAS called "the
        // obvious follow-up" here, and it LANDED without needing a second variable: the desktop
        // shell's `state_dir_path` joined `strategy-state` onto the boot's own already-resolved
        // state directory, so `<exe_dir>` became the no-project last resort rather than the default
        // this row used to name.
        //
        // ⚠ ...and that READER is gone with the desktop cut — the GUI mounts no strategies now, so
        // nothing in this workspace consumed the `config.state_dir` this variable fed. That left
        // the key declared, validated and reported by `vike-cli config show` as the ORIGIN of an
        // effective value while changing nothing, which is the failure `vike_config::consumed`'s
        // module doc calls worse than an unimplemented feature.
        //
        // ⚠ **So the KEY is now DELETED and this variable is REFUSED**, the same shape
        // `VIKE_MAX_ORDER_NOTIONAL` above takes and for a neighbouring reason: silently ignoring a
        // directory its operator believes is in force is worse than either keeping it or erroring.
        // `vike_config::refuse_removed_env` looks the name up in the caller-supplied env map (hence
        // `Injected`/`MapLookup`, unchanged) and fails startup. The row goes the day nothing
        // refuses it. Deliberately NO repo-anchored citation of the deleted resolver — a `SYMBOL`
        // pointer to a function removed in the same change is a citation-gate failure waiting one
        // commit.
        //
        // ⚠ `VIKE_STATE_ROOT` is NOT the replacement and an operator must not be sent there: it is
        // the state ROOT, a different directory, and the refusal says so. Its rows are in other
        // family files — `crates/vike-ops/src/settings/rows/gui.rs`'s `VIKE_STATE_ROOT` and
        // `crates/vike-ops/src/settings/rows/daemons.rs`'s `VIKE_STATE_ROOT`.
        name: "VIKE_STATE_DIR",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup; nothing read it, and VIKE_STATE_ROOT is not it",
    },
    vike_env("VIKE_STYLE", "vike-config", ""),
    vike_env("VIKE_TELEGRAM_CONTROL", "vike-config", ""),
    vike_env("VIKE_TRADEHUB_ADDR", "vike-config", ""),
    vike_env("VIKE_TRADEHUB_ALLOW_PUBLIC_BIND", "vike-config", ""),
    vike_env("VIKE_TRADEHUB_CONTROL", "vike-config", ""),
    // The env layer over `config.tradehub_account_admin` — the THREE-VALUED barrier
    // DECLARATION (unset/`off` / `loopback` / `contained`) that decides whether the
    // vike-tradehub node builds an account-administration capability at all. Three values
    // rather than a boolean because `loopback` is the one shape the process can CHECK (against
    // `bind_exposure`) and `contained` is one it cannot — collapsing them would make the
    // checkable case uncheckable. Read once by `vike_config::Config::apply_env` from the
    // caller-supplied map, like every other `config.*` key.
    vike_env("VIKE_TRADEHUB_ACCOUNT_ADMIN", "vike-config", ""),
    vike_env("VIKE_TRADEHUB_LIVE", "vike-config", ""),
    Setting {
        // ⚠ REMOVED, not read — settings unification PHASE 5, the daemon twin of
        // `VIKE_MAX_ORDER_NOTIONAL` above (same idea, two names). It was the vike-tradehub
        // server-edge per-order ceiling (`ControlLimitsConfig::max_notional`); that value now comes
        // from the `policy.max_notional_per_order` row. This one mattered most: on a
        // production node the variable lives in a systemd unit, where a stale `Environment=` line
        // raises a live risk limit with no diff and no review.
        //
        // Row kept in the crate that REFUSES it — see the `VIKE_MAX_ORDER_NOTIONAL` row's comment.
        name: "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "REMOVED — refused at startup; use policy.max_notional_per_order",
    },
    // A `vike_config::REMOVED_ENV` row like the literals above it, spelled out as one because no
    // constructor carries `Medium::Refused` (decision 0111's column).
    Setting {
        name: "VIKE_TRADEHUB_RECORD",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "",
    },
    // ⚠ Its SIBLING, re-keyed rather than deleted — `refuse_removed_env` genuinely looks this name
    // up in the caller-supplied map, so it IS read, just never obeyed. The row goes the day
    // nothing refuses it.
    Setting {
        name: "VIKE_TICK_STORE",
        krate: "vike-config",
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium: Medium::Refused,
        default: "",
    },
    vike_env("VIKE_CANCEL_ORDERS_ON_SHUTDOWN", "vike-config", ""),
];
