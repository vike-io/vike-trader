//! The data: `REMOVED_ENV`, one row per removed variable, and the message constants its rows share.

#[cfg(doc)]
use super::refuse_removed_env;
use super::{EmptyMeaning, RemovedSetting, ValueMap};

/// Why the four `{VENUE}_MAINNET` rows are refused — decision 0095 retired the switch, decision
/// 0119 made the account's own tier the network.
///
/// ⚠ It carries the whole answer, because the row has no settings key to paste: an account's tier
/// is its `account` row, written by an account verb rather than by `config set`.
const MAINNET_WHY: &str = "an account's own tier chooses its network now — a `live` account trades \
     MAINNET with the venue's LIVE keys, a `demo` account the demo network. To trade MAINNET on \
     purpose, set the account's tier: vike-cli secrets account set-tier --id <N> --tier live \
     (`vike-cli secrets accounts` lists the ids)";

/// The `removed_in` of the four `{VENUE}_MAINNET` rows.
const MAINNET_REMOVED_IN: &str = "decision 0119 (the account's tier is the network)";

/// Why the eleven Polymarket rows are refused — decision 0095.
const POLY_WHY: &str = "no environment variable configures a venue any more (decision 0095): this \
     setting is a row in the settings database, and a variable that still looked set would \
     silently configure nothing";

/// Why the six venue toggles are refused — decision 0095.
///
/// ⚠ It never spells the paste command itself: every line a refusal prints that starts with that
/// command is one a test counts, so the reason names the verb only as `config set`.
const VENUE_TOGGLE_WHY: &str = "no environment variable configures a venue any more (decision \
     0095): this setting is a row in the settings database, written with `config set` and \
     recorded in the change journal";

/// The lead line of the retired `VIKE_MARK_STREAMS` master's refusal.
const MARK_STREAMS_KILL_LEAD: &str = "The one switch became one field per venue. `0` — the only \
     value it ever acted on — keeps every venue's mark stream off; set each field instead";

/// The `removed_in` of the tool, smoke and unstarted-code rows (spec PR 5 of decision 0095).
const PR5_REMOVED_IN: &str = "decision 0095 (venues read no environment)";

/// D4 of decision 0095, for a variable whose only reader is code no composition root starts.
const NOTHING_STARTS: &str = "nothing starts the code it configured (no composition root spawns \
     it, and its entry point takes the value as a parameter now), so the variable was already \
     changing nothing — D4 of decision 0095";

/// [`NOTHING_STARTS`] for the one variable that was a KILL SWITCH: an operator told only "unset it"
/// reads "your halt is gone", so the refusal also names the switch that stays — the halt FILE the
/// poller checks every tick, which is not a variable and which this decision did not touch.
const REDEEM_HALT_WHY: &str = "nothing starts the auto-redeem poller it halted (no composition \
     root spawns it, and `AutoRedeemPoller::spawn` takes `halted` as a parameter now), so the \
     variable was already changing nothing — D4 of decision 0095. The poller's RUNTIME kill switch \
     is unchanged and is not a variable: the halt FILE its caller hands `AutoRedeemPoller::spawn`, \
     checked every tick";

/// Why `VIKE_HALT_FILE` is refused — decision 0099.
///
/// ⚠ It carries the whole answer, because the row has no key to paste and no section to name:
/// the sentinel's LOCATION stopped being a setting at all. Three things have to be in it. The file
/// is unchanged and so is how it is used (`touch`), so an operator who reads "removed" does not
/// conclude the kill switch went; the one path now, so they know what to `touch`; and why a stale
/// value is refused rather than ignored, because that is the sentence that stops a well-meaning
/// operator from "just" suppressing the refusal. It never spells the paste command, for the reason
/// [`VENUE_TOGGLE_WHY`] gives.
const HALT_FILE_WHY: &str = "the kill switch's FILE is unchanged but its location is no longer a \
     setting: it is always `<project>/settings/state/HALT`, in the state directory the daemon \
     booted with (`VIKE_SETTINGS_DIR` moves it together with the settings), and the daemon logs \
     the path it resolved at startup (`HALT sentinel path resolved`). A value left in a unit would \
     send an operator to `touch` a path nothing watches — a dead kill switch that reads exactly \
     like an armed one — so it refuses to start instead of being ignored";

/// Why `VIKE_RUN_PROFILE` is refused — decision 0111.
///
/// ⚠ It never spells a `config set` paste line, for the reason [`VENUE_TOGGLE_WHY`] gives; the
/// writer it names is a different verb.
const RUN_PROFILE_WHY: &str = "the daemon's run profile — the pre-trade [risk] ceilings, [guards] \
     and [sinks] — is the ACTIVE `run` row of the settings database, and no profile file is read \
     any more. Write it with `vike-cli config bootstrap-run <name> --mode <mode> --risk.<key> \
     <value> ...` (every key of the old file is a flag spelled as its dotted path, and the command \
     stores the body AND makes it the active run profile), check it with `vike-cli config show`, \
     and restart the daemon. A variable that still looked set would be a ceiling you believe is in \
     force while the daemon reads the row — so it refuses to start instead of being ignored";

/// The `removed_in` of every row decision 0111 retired.
const P0111_REMOVED_IN: &str = "decision 0111 (no setting lives in the environment)";

/// Why a variable whose setting became a row is refused — decision 0111.
///
/// ⚠ It never spells the paste command itself, for the reason [`VENUE_TOGGLE_WHY`] gives.
const ENV_TO_ROW_WHY: &str = "no setting is read from the process environment any more (decision \
     0111): this setting is a row in the settings database of the box that runs the process, \
     written with `config set` and recorded in the change journal, and a variable that still looked \
     set would silently configure nothing";

/// Why `VIKE_ALERTS` is refused — decision 0111. It named the alert-rules FILE, which is a path the
/// daemon derives, not a setting.
const ALERTS_WHY: &str = "the alert-rules file is always `alerts.json` in the state directory the \
     daemon booted with (`<settings>/state/alerts.json`; `VIKE_SETTINGS_DIR` moves it together with \
     the settings), and a variable naming another file would be a rules file nothing reads";

/// Why `VIKE_DATAHUB_VENUE_CATALOG` is refused — decision 0111, over the warning decision 0066 gave
/// it. Its `=1` asked for what is now the default; any other value asked for the catalog OFF, which
/// only the `flags.venue_catalog_off` row says now.
const VENUE_CATALOG_WHY: &str = "it has configured nothing since the venue catalog became ON by \
     default (decision 0066): a datahub serves the venue-catalog verb unless the \
     `flags.venue_catalog_off` row refuses it — write that row `true` with `config set` if this \
     variable was meant to turn the catalog off";

/// Why `VIKE_RECORD_DVOL` is refused — decision 0111, over the warning `crate::DEAD_FLAG_KEYS` gave
/// it.
const RECORD_DVOL_WHY: &str = "the `record_dvol` flag it fed was deleted because nothing read it, \
     on either spelling; the DVOL feed is unmounted rather than removed \
     (`vike_deribit::spawn_deribit_dvol_feed` takes its enable gate as a parameter)";

/// Why `VIKE_RECORD_CHAINS_CADENCE_MS` is refused — decision 0111. It tuned a recorder nothing
/// mounts, and it was never a settings key.
const RECORD_CHAINS_CADENCE_WHY: &str = "the option-chain recorder it tuned is unmounted (no \
     composition root opens it), and its cadence is a parameter of \
     `vike_data::ChainRecorder::open` now: a root that mounts the recorder states the cadence";

/// Why `VIKE_DATAHUB_ADDR` is refused — decision 0111. One variable carried two settings: the data
/// server's listen address and a client's dial address, which are two rows.
const DATAHUB_ADDR_WHY: &str = "no setting is read from the process environment any more (decision \
     0111). This variable named two things, and each is its own row now: on the data server it is \
     the listen address, `config.datahub_bind_addr` (the line above); on a client (`vike-cli`, the \
     desktop) it is the address dialled, `config.datahub_addr`";

/// Why `VIKE_DATAHUB_STORE` and `VIKE_HIST_STORE` are refused — decision 0111.
const STORE_ROOT_WHY: &str = "no setting is read from the process environment any more (decision \
     0111): the hist-store root of a box is the `config.store_root` row; one datahub run on other \
     files is `vike-backend datahub --store DIR`, and a tool that opens no settings database takes \
     its `--store` argument. ⚠ Write the row BEFORE you remove the line: \
     with neither, the data server resolves the project's default store, which is not the store \
     this box has been recording into";

/// Why `VIKE_STATE_ROOT` is refused — decision 0111. It relocated the daemons' state tree away from
/// the settings directory, which is a path the processes derive, not a setting.
const STATE_ROOT_WHY: &str = "the state tree is always the settings directory's `state/` \
     (`<settings>/state`): `VIKE_SETTINGS_DIR` moves it together with the settings, the \
     credentials and the logs, so the log home, `alerts.json` and the strategy sidecars can never \
     hang off a different project from the settings that wrote them";

/// The removed variables. Every entry is refused at startup by [`refuse_removed_env`].
///
/// The first two carried the SAME idea — a per-order notional ceiling — under two names, one for
/// the GUI (`vike-app`, via `vike_app_core::orders::order_entry::OrderLimits`, plus `vike-cli`'s advisory
/// client-side guardrail) and one for the headless daemon (`vike-tradehub`'s server-edge
/// `ControlLimitsConfig`). Both now read [`crate::Policy::max_notional_per_order`], which is the
/// point: one key, one authority.
///
/// The last four carry decision 0095's retired `{VENUE}_MAINNET` switches: the account's own tier
/// chooses the network now (decision 0119), and each row stays so a stale switch is refused rather
/// than silently doing nothing.
///
/// The Polymarket rows follow (decision 0095). Two of them — `POLY_SOCKS_PROXY` and
/// `POLY_WS_PROXY_ENABLED` — carry a [`RemovedSetting::when_empty`]: their blank value meant
/// "direct", so they refuse blank too.
///
/// The venue toggles close the table (decision 0095): `HYPERLIQUID_HIP3` and
/// `VIKE_ALLOW_WITHDRAW_KEYS` were the environment layer of two `flags.*` rows that no longer have
/// one, and `VIKE_BINANCE_TRADE_LITE_FILL`, `VIKE_BYBIT_FAST_EXEC`, `VIKE_MARK_STREAMS_ASTER` and
/// the `VIKE_MARK_STREAMS` master became `venue.*` fields. None of the six carries a `when_empty`:
/// every deleted reader took a blank value as unset
/// (`a_blank_retired_venue_toggle_starts_normally_because_blank_was_unset` names what each did).
/// The three fields carry [`ValueMap::ExactOneUntrimmed`] and the master
/// [`ValueMap::KillSwitchEach`]: their readers compared the untrimmed value exactly, so a padded
/// or commented `1`/`0` is refused as the default it ran, not as the row it spells.
/// ⚠ `VIKE_MARK_STREAMS_ASTER` stands BEFORE the master on purpose: the
/// refusal prints its blocks in this table's order, so an operator who had both set and pastes the
/// lines top to bottom ends on the master's `0` for aster — the master kill was absolute in the
/// deleted resolver (`the_master_kill_still_wins_when_both_mark_stream_variables_are_set`).
/// `VIKE_MARK_STREAMS_BINANCE` and its bybit/okx/hyperliquid twins are NOT rows: only aster's
/// per-venue spelling was ever read, so refusing one would stop a correct daemon over a spelling
/// that never did anything.
///
/// The tools, the smokes and the code nothing starts close the table (decision 0095's spec PR 5):
/// the five `ctrader_authorize` variables (three became flags, the app pair is read from the
/// credential store), the two egress-guard variables (the smokes pass constants), and ten read only
/// by code no composition root starts — the chain watcher's four, the auto-redeem pair, the
/// heartbeat, the resolve and outcome pollers, and the DVOL cadence (D4: each is a parameter of that
/// code now). Every one carries `key: None` and an empty `file`, and prints no `config set` line: the
/// five poller flags keep their `flags.*` rows, but nothing reads those rows, so pointing at one
/// would confirm something false. ⚠ That makes these refusals of variables a RUNNING process never
/// read — a deliberate departure from the "belief made false" argument above, taken so a leftover
/// variable is named rather than silently dropped the day something starts its code. Only
/// `POLY_REDEEM_HALT` carries a `when_empty` (its readers halted on presence).
pub const REMOVED_ENV: &[RemovedSetting] = &[
    RemovedSetting::moved(
        "VIKE_MAX_ORDER_NOTIONAL",
        "policy",
        "policy.max_notional_per_order",
        ValueMap::PositiveNumber,
        "Phase 5 (settings unification)",
        "an order-size ceiling any exported variable can raise is not a ceiling",
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        "policy",
        "policy.max_notional_per_order",
        ValueMap::PositiveNumber,
        "Phase 5 (settings unification)",
        "an order-size ceiling any exported variable can raise is not a ceiling",
    )
    .echoed(),
    // ⚠ The THIRD shape on this list, and the one to copy from when a key is deleted for being
    // UNREAD rather than for being an env-settable ceiling. `VIKE_STATE_DIR` named the
    // DESKTOP's strategy-state sidecar directory; `crates/vike-desktop/src/main.rs`'s
    // `state_dir_path` was its only reader and went with the desktop cut's local core, leaving
    // `config.state_dir` declared, validated, and reported by `vike-cli config show` as the
    // ORIGIN of an effective value that configured nothing. `crate::consumed`'s row for that
    // key had specified exactly this end state in writing since the reader was deleted.
    //
    // ⚠ `key: None` and `file: "config"` together are doing something specific: the value
    // did NOT move, so there is no line to paste — but the operator asking "where do I set this
    // now" has to be told the answer is nowhere, in the section they were using, rather than
    // left to guess. The one thing they must NOT conclude is that the state ROOT
    // (`<settings>/state`) is the same knob under a new name: `vike_model::paths::state_path`'s
    // module doc records the collision between the two names.
    //
    // ⚠ The six `flags` keys deleted in the same change are deliberately NOT here — their
    // variables are still read by the venue adapters that own them, so refusing one would take
    // down a correct deployment. `crate::flags::REMOVED_FLAG_KEYS` is their (row-only)
    // tombstone and argues the split.
    //
    // ⚠ `echo_value: false` (`dropped` never echoes), and NOT because the value is a secret — a
    // directory path is not one. This row carries `key: None`, so the refusal offers no TOML line
    // to paste, and `only_rows_with_a_key_to_paste_echo_their_value` holds the pairing: a row with
    // nothing to paste has no reason to echo, and an echo with no line beside it reads as a
    // suggestion that the value should be moved somewhere. It should not be moved anywhere.
    RemovedSetting::dropped(
        "VIKE_STATE_DIR",
        "config",
        "the unread-settings sweep (settings unification)",
        "nothing reads it — its one reader, the desktop's strategy-state sidecar resolver, \
              went with the desktop's local core. ⚠ the state ROOT (`<settings>/state`) is a \
              DIFFERENT directory and is not a replacement for it",
    ),
    // ⚠ Same THIRD shape as `VIKE_STATE_DIR` above — a key deleted for having no reader left,
    // not an env-settable ceiling. It gated the live-feed tee `open_tradehub_recorder` in the
    // daemon's CLI — the writer that put quote/trade/book rows into the history store from
    // inside the order-signing process.
    //
    // ⚠ That function is GONE rather than moved, so the name above is deliberately NOT written
    // as a path-plus-symbol citation: there is no live site to point at, and
    // `crates/vike-ops/tests/docs/citation_gate.rs` is right to refuse one — a citation whose file
    // still exists while the symbol it names does not is exactly the silent rot that gate was
    // written for, and it caught this comment's first draft. The bare name stays in prose
    // because it is the evidence for the claim around it.
    //
    // Both it and the journal materializer went with
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md`: the store has one
    // writer plane, and the recorder that survives runs inside the datahub — with the watchdog,
    // the silence detection and the record profiles this tee never had.
    //
    // ⚠ `key: None` for the same reason that entry gives: the value did NOT move, so there is
    // no line to paste. An operator asking "where do I turn recording on now" is told the
    // answer is `vike-backend datahub --record-profile <name>`, in the OTHER daemon, rather
    // than left to guess — and the one thing they must not conclude is that the feature moved
    // under a new flag name here.
    //
    // ⚠ **That flag is spelled correctly TODAY and the owner has ruled it will be renamed** to
    // `--recorder-profile [NAME]`, with an optional value
    // (`docs/superpowers/specs/2026-09-22-data-realtime-record-design.md`, ruling 5 — SHAPE
    // ACCEPTED, nothing built). Named here so the rename finds this site: a refusal that points
    // an operator at a flag that no longer exists is the same defect as the one this row cures,
    // one layer along.
    //
    // ⚠ It belongs HERE rather than in `crate::flags::REMOVED_FLAG_KEYS` by that table's own
    // rule: those rows exist because their variables are STILL READ by the venue adapters that
    // own them, so refusing one would take down a correct box. This variable is read by nothing
    // after 0084, which is exactly the graduation condition that table names.
    RemovedSetting::dropped(
        "VIKE_TRADEHUB_RECORD",
        "flags",
        "0084 (only the datahub touches the store)",
        "the daemon stopped writing the store; the recorder that survives runs in the datahub",
    ),
    // ⚠ The SIBLING of the row above, and it dies of the same change one step removed.
    // `VIKE_TRADEHUB_RECORD` gated the live-feed tee; this named the DIRECTORY that tee wrote
    // into. With the tee gone its reader — `tick_store_root` — went too, and it was the LAST
    // reader anywhere: the desktop's had already gone with the GUI's local tick plane. So an
    // operator who sets this now configures NOTHING, silently, while believing they have
    // directed where ticks land. That is the whole reason this table exists.
    //
    // ⚠ **The refusal was withheld until it was MEASURED, because it fails a daemon's
    // startup.** A refusal for a variable somebody actually sets is an outage, not a
    // correction. Swept on the live box 2026-09-22 before adding this row: not in any unit's
    // `Environment=`, not in `vike-tradehub`'s `EnvironmentFile` (`<project>/.env`), nowhere
    // under `/etc/systemd`, `/etc/environment`, `/etc/profile*` or `/etc/default`, in no shell
    // profile, and — the check that actually settles it — in the real `/proc/<pid>/environ` of
    // all three running daemons, which carry it zero times. No tracked `deploy/` unit names it
    // either. The only hits on disk were the string compiled INTO the shipped binaries by this
    // very registry.
    //
    // ⚠ `key: None`, so `echo_value: false` (`only_rows_with_a_key_to_paste_echo_their_value`
    // holds the pairing): the value did not MOVE to a settings key, so there is no line to paste.
    // Where ticks land is the datahub's question now, and its store is named by
    // `VIKE_DATAHUB_STORE`.
    RemovedSetting::dropped(
        "VIKE_TICK_STORE",
        "config",
        "0084 (only the datahub touches the store)",
        "its last reader went with the daemon's live-feed tee; the datahub names its own store",
    ),
    RemovedSetting::dropped(
        "VIKE_SECRETS_PASSPHRASE",
        "the settings database",
        "the one-store change (settings unification)",
        "nothing consumes it — credentials are read from the settings database, in plaintext",
    ),
    RemovedSetting::dropped("BINANCE_MAINNET", "", MAINNET_REMOVED_IN, MAINNET_WHY),
    RemovedSetting::dropped("BYBIT_MAINNET", "", MAINNET_REMOVED_IN, MAINNET_WHY),
    RemovedSetting::dropped("OKX_MAINNET", "", MAINNET_REMOVED_IN, MAINNET_WHY),
    RemovedSetting::dropped("HYPERLIQUID_MAINNET", "", MAINNET_REMOVED_IN, MAINNET_WHY),
    RemovedSetting::moved(
        "POLY_EXEC",
        "the settings database",
        "flags.poly_exec",
        ValueMap::Switch,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_RECONCILE",
        "the settings database",
        "flags.poly_reconcile",
        ValueMap::Switch,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_PROXY_ENABLED",
        "the settings database",
        "venue.polymarket.proxy_enabled",
        ValueMap::TrueUnlessFalsey,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_PROXY_HOST",
        "the settings database",
        "venue.polymarket.proxy_host",
        ValueMap::FirstToken,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_PROXY_PORT",
        "the settings database",
        "venue.polymarket.proxy_port",
        ValueMap::FirstToken,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_SOCKS_PROXY",
        "the settings database",
        "venue.polymarket.socks_proxy",
        ValueMap::Stdin,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .empty_means(EmptyMeaning {
        // The deleted resolver read `POLY_SOCKS_PROXY=` as the `none`/`direct` sentinel — no proxy
        // on EITHER lane — so an empty line here is an operator who chose to bypass the tunnel.
        meant: "connect DIRECT on both lanes (REST and WebSocket) — the same as `direct`",
        write: "direct",
    }),
    RemovedSetting::moved(
        "POLY_WS_PROXY_ENABLED",
        "the settings database",
        "venue.polymarket.ws_proxy_enabled",
        ValueMap::FalseUnlessTruthy,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed()
    .empty_means(EmptyMeaning {
        // Any value but `1`/`true`/`yes`/`on` — an empty one included — sent the WebSocket lanes
        // DIRECT while REST kept its tunnel; only the truthy spellings left them on it.
        meant: "send the WebSocket lanes DIRECT while REST kept its tunnel (only `1`, `true`, \
                    `yes` or `on` left them on it)",
        write: "false",
    }),
    RemovedSetting::moved(
        "POLY_RATE_GATE",
        "the settings database",
        "venue.polymarket.rate_gate",
        ValueMap::ExactOne,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_EXEC_MARKETS",
        "the settings database",
        "venue.polymarket.exec_markets",
        ValueMap::List,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_PRESUBMIT_REGISTER",
        "the settings database",
        "venue.polymarket.presubmit_register",
        ValueMap::ExactOne,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "POLY_WS_TOKENS_PER_SOCKET",
        "the settings database",
        "venue.polymarket.ws_tokens_per_socket",
        ValueMap::FirstToken,
        "decision 0095 (venues read no environment)",
        POLY_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "HYPERLIQUID_HIP3",
        "the settings database",
        "flags.hyperliquid_hip3",
        ValueMap::Switch,
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_ALLOW_WITHDRAW_KEYS",
        "the settings database",
        "flags.allow_withdraw_keys",
        ValueMap::Switch,
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_BINANCE_TRADE_LITE_FILL",
        "the settings database",
        "venue.binance.trade_lite_fill",
        ValueMap::ExactOneUntrimmed,
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_BYBIT_FAST_EXEC",
        "the settings database",
        "venue.bybit.fast_exec",
        ValueMap::ExactOneUntrimmed,
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    // ⚠ BEFORE the master below, and the order is the argument — see this table's doc.
    RemovedSetting::moved(
        "VIKE_MARK_STREAMS_ASTER",
        "the settings database",
        "venue.aster.mark_streams",
        ValueMap::ExactOneUntrimmed,
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_MARK_STREAMS",
        "the settings database",
        "venue.binance.mark_streams",
        ValueMap::KillSwitchEach {
            lead: MARK_STREAMS_KILL_LEAD,
            also: &[
                "venue.bybit.mark_streams",
                "venue.okx.mark_streams",
                "venue.hyperliquid.mark_streams",
                "venue.aster.mark_streams",
            ],
        },
        "decision 0095 (venues read no environment)",
        VENUE_TOGGLE_WHY,
    )
    .echoed(),
    // --- the tools, the smokes and the code nothing starts (spec PR 5) ---------------------------
    //
    // No key and no section: the replacement is a command-line flag, a credential-store row, a
    // smoke's constant, or a PARAMETER of code no composition root starts (D4), and the `why`
    // carries the whole answer. Five of them used to feed `flags.*` rows that still exist; those
    // rows are read by nothing (`crate::CONSUMPTION`), so a `config set` line would confirm
    // something false. None echoes its value: `CTRADER_CLIENT_SECRET` is a secret and
    // `POLY_CHAIN_RPC_URL` can carry an endpoint's key.
    //
    // The cTrader five were read only by the `ctrader_authorize` tool, which boots nothing — so it
    // does not reach this table — and refuses the same five itself, out of its own sweep
    // (`crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `refuse_retired_variables`). The two
    // app-pair rows name `vike-cli secrets set` as coming AFTER the unset this refusal's tail asks
    // for, because `vike-cli` itself refuses to start while one is set.
    RemovedSetting::dropped(
        "CTRADER_REDIRECT_URI",
        "",
        PR5_REMOVED_IN,
        "the ctrader_authorize tool takes it on its command line: pass --redirect-uri <URI>",
    ),
    RemovedSetting::dropped(
        "CTRADER_SCOPE",
        "",
        PR5_REMOVED_IN,
        "the ctrader_authorize tool takes it on its command line: pass --scope <SCOPE>",
    ),
    RemovedSetting::dropped(
        "CTRADER_TOKEN_FILE",
        "",
        PR5_REMOVED_IN,
        "the ctrader_authorize tool takes it on its command line: pass --token-file <PATH>",
    ),
    RemovedSetting::dropped(
        "CTRADER_CLIENT_ID",
        "",
        PR5_REMOVED_IN,
        "the ctrader_authorize tool reads the app pair from the credential store, the rows \
              every cTrader mount reads; `vike-cli secrets set CTRADER_CLIENT_ID` writes it there \
              (the value on stdin) — run that after the unset below, because vike-cli refuses to \
              start while the variable is set",
    ),
    RemovedSetting::dropped(
        "CTRADER_CLIENT_SECRET",
        "",
        PR5_REMOVED_IN,
        "the ctrader_authorize tool reads the app pair from the credential store, the rows \
              every cTrader mount reads; `vike-cli secrets set CTRADER_CLIENT_SECRET` writes it \
              there (the value on stdin) — run that after the unset below, because vike-cli \
              refuses to start while the variable is set",
    ),
    RemovedSetting::dropped(
        "POLY_EGRESS_PROBE_URL",
        "",
        PR5_REMOVED_IN,
        "the egress guard takes its probe URL as a parameter; the polymarket smokes pass \
              vike_polymarket::DEFAULT_EGRESS_PROBE",
    ),
    RemovedSetting::dropped(
        "POLY_EXPECT_EGRESS_COUNTRY",
        "",
        PR5_REMOVED_IN,
        "the egress guard takes the expected country as a parameter; the order-placing \
              polymarket smokes pass vike_polymarket::DUBLIN_EGRESS_COUNTRY, the read-only ones \
              none",
    ),
    RemovedSetting::dropped("POLY_CHAIN_WATCH", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("POLY_CHAIN_RPC_URL", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("POLY_CHAIN_MAX_SPAN", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("POLY_CHAIN_PROXY", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("POLY_AUTO_REDEEM", "", PR5_REMOVED_IN, NOTHING_STARTS),
    // Both readers — the poller's kill switch and `Flags::apply_env` — halted on PRESENCE, so
    // an empty line halted exactly as `=1` did. Nothing starts the poller, so there is no row
    // to write: the refusal says what the blank meant and prints no line.
    RemovedSetting::dropped("POLY_REDEEM_HALT", "", PR5_REMOVED_IN, REDEEM_HALT_WHY).empty_means(
        EmptyMeaning {
            meant: "HALTED the auto-redeem poller — both of its readers tested PRESENCE, so \
                    `POLY_REDEEM_HALT=` halted exactly as `=1` did",
            write: "",
        },
    ),
    RemovedSetting::dropped("POLY_HEARTBEAT", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("VIKE_PM_RESOLVE", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("VIKE_HL_OUTCOME", "", PR5_REMOVED_IN, NOTHING_STARTS),
    RemovedSetting::dropped("VIKE_RECORD_DVOL_CADENCE_MS", "", PR5_REMOVED_IN, NOTHING_STARTS),
    // --- the HALT sentinel's path (decision 0099) -------------------------------------------------
    //
    // The one row here that retires a KILL SWITCH's configuration rather than a ceiling or a venue
    // toggle, and the reason it REFUSES where a quiet ignore would have been tidier. The old
    // resolver took `VIKE_HALT_FILE` first, so a unit that set it named the file an operator
    // `touch`ed; a build that merely stopped reading it would leave that operator `touch`ing a path
    // nothing watches. `key: None`, `file: ""` and `echo_value: false` for the reasons the
    // other key-less rows give (`only_rows_with_a_key_to_paste_echo_their_value`); no `when_empty`,
    // because the old resolver trimmed the value and fell through on a blank one, so a blank line
    // says nothing the default does not. Its settings-registry row moved here from
    // `vike-bridge-core` (a ratchet shrink) and the `vike-paper` test row left with the test that
    // read it.
    RemovedSetting::dropped(
        "VIKE_HALT_FILE",
        "",
        "decision 0099 (the HALT sentinel's path is data)",
        HALT_FILE_WHY,
    ),
    // --- the daemon run profile (decision 0111, verdict 4) ----------------------------------------
    //
    // The variable named a run-profile FILE — the operator's pre-trade `[risk]` ceilings, `[guards]`
    // and `[sinks]` — and the run profile is rows only now: `vike-tradehub` reads the ACTIVE `run`
    // row and nothing else. Refused rather than ignored because the file it names holds the numbers
    // every order is judged against: a variable that still looked set would be a ceiling the
    // operator believes is in force while the daemon reads a different body, or none. `key: None`
    // and `file: ""`: the replacement is not one settings key but a whole body of rows, written by
    // a command the `why` names; no value is echoed (it is a path, and there is nothing to paste it
    // into). No `when_empty`: every deleted reader took a blank value as unset.
    RemovedSetting::dropped(
        "VIKE_RUN_PROFILE",
        "",
        "decision 0111 (the daemon run profile is settings rows only)",
        RUN_PROFILE_WHY,
    ),
    // --- every other setting the environment carried (decision 0111, verdict 1) -------------------
    //
    // Each was the environment layer of a `config.*` / `flags.*` / `preferences.*` row, or a
    // daemon's own read beside one, and every reader is gone: the row alone decides. A flag
    // variable's exact-`1` grammar maps to `true`/`false` (`ValueMap::Switch`); every other value
    // is carried verbatim. None carries a `when_empty`: every deleted reader took a blank value as
    // unset (the exact-`"1"` flags, the `get` that skipped a blank, and the daemons' `parse_*`
    // helpers that fell back to the default).
    RemovedSetting::moved(
        "VIKE_RECONCILE",
        "flags",
        "flags.reconcile",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_OFF",
        "flags",
        "flags.reconcile_off",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_RESTORE_OFF",
        "flags",
        "flags.reconcile_restore_off",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_BALANCE",
        "flags",
        "flags.reconcile_balance",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_GENERATE_MISSING",
        "flags",
        "flags.reconcile_generate_missing",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_POLICY",
        "config",
        "config.reconcile_policy",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_INTERVAL_MS",
        "config",
        "config.reconcile_interval_ms",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_AUDIT_MS",
        "config",
        "config.reconcile_audit_ms",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_LOOKBACK_MS",
        "config",
        "config.reconcile_lookback_ms",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_STARTUP_DELAY_MS",
        "config",
        "config.reconcile_startup_delay_ms",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_BALANCE_TOL_ABS",
        "config",
        "config.reconcile_balance_tol_abs",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECONCILE_BALANCE_TOL_REL",
        "config",
        "config.reconcile_balance_tol_rel",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_PREFLIGHT_SKIP",
        "flags",
        "flags.preflight_skip",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_LIVE",
        "flags",
        "flags.tradehub_live",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_ALLOW_PUBLIC_BIND",
        "flags",
        "flags.tradehub_allow_public_bind",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_ADDR",
        "config",
        "config.tradehub_addr",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_ADVERTISE_ADDR",
        "config",
        "config.tradehub_advertise_addr",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_ACCOUNT_ADMIN",
        "config",
        "config.tradehub_account_admin",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_CONTROL_RATE",
        "config",
        "config.tradehub_control_rate",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TELEGRAM_CONTROL",
        "flags",
        "flags.telegram_control",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_CANCEL_ORDERS_ON_SHUTDOWN",
        "flags",
        "flags.cancel_orders_on_shutdown",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT",
        "flags",
        "flags.oco_cancel_sibling_on_dead_exit",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_INSTANCE_ORIGIN",
        "config",
        "config.instance_origin",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_PIN_CORES",
        "config",
        "config.pin_cores",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_JOURNAL_SNAPSHOT_EVERY",
        "config",
        "config.journal_snapshot_every",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_BACKTEST_ADDR",
        "config",
        "config.backtest_addr",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_ADVERTISE_ADDR",
        "config",
        "config.datahub_advertise_addr",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_VENUE_CATALOG_OFF",
        "flags",
        "flags.venue_catalog_off",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_STYLE",
        "preferences",
        "preferences.chart_style",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_LOG",
        "preferences",
        "preferences.log_level",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    // The FILE log's level (decision 0111, verdict 3). Every binary that builds a rolling file
    // reads the row before its subscriber — the daemons and the desktop from their boot, the
    // batch tools through `vike_boot::log_file_level` — and `vike_log` reads no variable for it.
    RemovedSetting::moved(
        "VIKE_LOG_FILE_LEVEL",
        "preferences",
        "preferences.log_file_level",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_TRADEHUB_CONTROL",
        "flags",
        "flags.tradehub_control",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_JOURNAL_DIR",
        "config",
        "config.journal_dir",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECORD_PROPERTIES",
        "flags",
        "flags.record_properties",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_RECORD_CHAINS",
        "flags",
        "flags.record_chains",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_LIVE",
        "flags",
        "flags.datahub_live",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_CHART_SEED",
        "flags",
        "flags.datahub_chart_seed",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_ALLOW_PUBLIC_BIND",
        "flags",
        "flags.datahub_allow_public_bind",
        ValueMap::Switch,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_ADDR",
        "config",
        "config.datahub_bind_addr",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        DATAHUB_ADDR_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_LIVE_RESIDENT",
        "config",
        "config.datahub_live_resident",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_DATAHUB_STORE",
        "config",
        "config.store_root",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        STORE_ROOT_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_HIST_STORE",
        "config",
        "config.store_root",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        STORE_ROOT_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_SWEEP_THREADS",
        "preferences",
        "preferences.sweep_threads",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_MAX_ORDER_QTY",
        "preferences",
        "preferences.max_order_qty",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::moved(
        "VIKE_EXPORT_DIR",
        "preferences",
        "preferences.export_dir",
        ValueMap::Verbatim,
        P0111_REMOVED_IN,
        ENV_TO_ROW_WHY,
    )
    .echoed(),
    RemovedSetting::dropped("VIKE_STATE_ROOT", "", P0111_REMOVED_IN, STATE_ROOT_WHY),
    // Process SHAPES rather than settings: each is how one process is started, so it is an
    // argument its unit's `ExecStart=` (or the command line) states (decision 0111).
    RemovedSetting::dropped(
        "VIKE_BACKTEST_NAMED_RUN",
        "",
        P0111_REMOVED_IN,
        "the compute daemon takes it on its command line: run `backtest --addr --named-run`",
    ),
    RemovedSetting::dropped(
        "VIKE_API_BASE",
        "",
        P0111_REMOVED_IN,
        "the vikedata_backfill tool takes it on its command line: pass --api-base <URL>",
    ),
    RemovedSetting::dropped(
        "VIKE_COUNTERS_FILE",
        "",
        P0111_REMOVED_IN,
        "the vike_stat tool takes it on its command line: run vike_stat <counters-file>",
    ),
    RemovedSetting::dropped(
        "VIKE_STRATEGY_BUILDER_PORT",
        "",
        P0111_REMOVED_IN,
        "the strategy builder takes it on its command line: pass --port <PORT>",
    ),
    RemovedSetting::dropped(
        "VIKE_STRATEGY_BUILDER_OUT_DIR",
        "",
        P0111_REMOVED_IN,
        "the strategy builder takes it on its command line: pass --out-dir <DIR>",
    ),
    RemovedSetting::dropped(
        "VIKE_STRATEGY_BUILDER_RETAIN",
        "",
        P0111_REMOVED_IN,
        "the strategy builder takes it on its command line: pass --retain <N>",
    ),
    RemovedSetting::dropped(
        "VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT",
        "",
        P0111_REMOVED_IN,
        "the strategy builder takes it on its command line: pass --workspace-root <DIR>",
    ),
    // A path the daemon derives, and two warnings promoted to refusals (decision 0111's verdict 1:
    // a retired variable is refused, never left to configure nothing).
    RemovedSetting::dropped("VIKE_ALERTS", "", P0111_REMOVED_IN, ALERTS_WHY),
    RemovedSetting::dropped("VIKE_DATAHUB_VENUE_CATALOG", "", P0111_REMOVED_IN, VENUE_CATALOG_WHY),
    RemovedSetting::dropped("VIKE_RECORD_DVOL", "", P0111_REMOVED_IN, RECORD_DVOL_WHY),
    // The chain recorder's own cadence variable, read only by a door decision 0111 deleted
    // (`vike_data`'s `ChainRecorder::from_env`); no settings key ever carried it.
    RemovedSetting::dropped(
        "VIKE_RECORD_CHAINS_CADENCE_MS",
        "",
        P0111_REMOVED_IN,
        RECORD_CHAINS_CADENCE_WHY,
    ),
];
