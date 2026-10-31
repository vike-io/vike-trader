//! The operator, deploy and documentation command lines of `config`, `secrets`, `backend`,
//! `datahub`, `gatewayhub`, `mcp`, `report`, `research`, `init`, `indicators`, `surface` and the
//! top-level help/version tokens, pinned as ACCEPTED (decision 0155).
//!
//! Written against the hand-rolled `Flags` parsers and run UNCHANGED against the `clap` ones: the
//! only question each line asks is whether the argument reader let it through (`common::accepted`
//! explains the sandbox, and why "not exit 2" is that question). A sandbox has an EMPTY settings
//! directory, so most lines go on to a clean `1` ("no store") or `3` (a refused loopback dial) AFTER
//! the reader accepted them; that is fine, and it is why each argv here is also chosen to be fully
//! valid, so that no post-parse refusal answers `2`. The lines that are REFUSED on purpose (the
//! retired top-level verbs, a removed flag, a value on argv) are pinned as refused so a rewrite keeps
//! refusing them, and `config` with no or an unknown verb is pinned at `1`, not `2`.

mod common;

use common::accepted::{assert_accepted, pin, pin_code};

/// `-h` / `--help` / `help` / `-V` / `--version` at the top: every trailing token is ignored.
#[test]
fn top_level_help_and_version() {
    assert_accepted(&[
        pin_code(&["-h"], 0, "dispatch: the short help"),
        pin_code(&["--help"], 0, "dispatch: the long help, which scripts/cli_mcp_smoke.sh asserts"),
        pin_code(&["help"], 0, "dispatch: the word form"),
        pin_code(&["help", "config"], 0, "dispatch ignores whatever follows `help`"),
        pin_code(&["--help", "--json", "extra"], 0, "dispatch ignores whatever follows --help"),
        pin_code(&["-V"], 0, "dispatch: the short version, an installer's first probe"),
        pin_code(&["--version"], 0, "dispatch: the long version"),
        pin_code(&["--version", "extra"], 0, "dispatch ignores whatever follows --version"),
    ]);
}

/// Refused ON PURPOSE (exit 2): the retired top-level verbs ignore everything after their name, an
/// unknown verb and a bare `vike-cli` are usage errors, and a few removed spellings are refused by
/// name. `config` with no verb or an unknown verb is `1`, not `2` (the old dispatcher's rung).
#[test]
fn retired_and_refused_on_purpose() {
    assert_accepted(&[
        pin_code(&["sweep"], 2, "lib.rs RETIRED_COMMANDS: bare"),
        pin_code(&["sweep", "--help"], 2, "retired verbs answer 2 even for --help"),
        pin_code(&["sweep", "-h"], 2, "retired verbs answer 2 even for -h"),
        pin_code(
            &["sweep", "--profile", "sweep.toml", "--rank-by", "sharpe"],
            2,
            "the retired message's own `was:` line",
        ),
        pin_code(
            &["sweep", "--no-such-flag", "=", "--"],
            2,
            "everything after a retired verb is ignored",
        ),
        pin_code(&["walkforward"], 2, "lib.rs RETIRED_COMMANDS: bare"),
        pin_code(&["walkforward", "--help"], 2, "retired verbs answer 2 even for --help"),
        pin_code(
            &["walkforward", "--profile", "wf.toml"],
            2,
            "the retired message's own `was:` line",
        ),
        pin_code(&["study"], 2, "lib.rs RETIRED_COMMANDS: bare"),
        pin_code(&["study", "--help"], 2, "the top-level study stays retired for --help"),
        pin_code(
            &[
                "study",
                "--study",
                "NAME",
                "--recipe",
                "recipe.toml",
                "--from",
                "2026-01-01",
                "--to",
                "2026-02-01",
            ],
            2,
            "the retired message's own `was:` line",
        ),
        pin_code(&["study", "--study=NAME", "--json"], 2, "inline form after a retired verb"),
        pin_code(&[], 2, "dispatch: no verb prints help on the usage rung"),
        pin_code(&["definitely-not-a-verb"], 2, "dispatch: an unknown verb (cli_mcp_smoke.sh)"),
        pin_code(&["--json"], 2, "dispatch: a flag where the verb belongs is an unknown command"),
        pin_code(&["secrets"], 2, "secrets grammar: a subcommand is required"),
        pin_code(
            &["secrets", "template"],
            2,
            "TEMPLATE_REMOVED: refused by name (cli_mcp_smoke.sh)",
        ),
        pin_code(&["secrets", "list", "--file", "x"], 2, "FILE_FLAG_REMOVED: refused by name"),
        pin_code(
            &["secrets", "set", "BINANCE_LIVE_API_KEY", "notavalue"],
            2,
            "ARGV_VALUE_REFUSAL: a value on argv is refused",
        ),
        pin_code(&["backend"], 2, "node/mod.rs parse: a subcommand is required"),
        pin_code(&["datahub"], 2, "datahub parse: a subcommand is required"),
        pin_code(&["gatewayhub"], 2, "gatewayhub parse: a subcommand is required"),
        pin_code(&["research"], 2, "research.rs: a subcommand is required"),
        pin_code(
            &[
                "config",
                "set",
                "preferences.log_file_level",
                "warn",
                "--confirm",
                "preferences.log_file_level",
            ],
            2,
            "config set: --confirm was removed (0086)",
        ),
        pin_code(
            &["indicators", "--name", "sma", "--category", "trend"],
            2,
            "--name XOR --category",
        ),
        pin_code(
            &[
                "research", "study", "--study", "s", "--recipe", "r.toml", "--from", "a", "--to",
                "b", "--store", "x",
            ],
            2,
            "study: --store names a path on the backend's box",
        ),
    ]);
}

/// `-h` / `--help` on every verb path of this file's verbs: exit 0, never the sentinel.
#[test]
fn help_for_every_verb_path() {
    assert_accepted(&[
        pin_code(&["config", "--help"], 0, "config.rs run: the verb list"),
        pin_code(&["config", "-h"], 0, "config.rs run: short"),
        pin_code(&["config", "help"], 0, "config.rs run: the word form"),
        pin_code(&["config", "show", "--help"], 0, "config show"),
        pin_code(&["config", "show", "-h"], 0, "config show, short"),
        pin_code(&["config", "check", "--help"], 0, "config check"),
        pin_code(&["config", "check", "-h"], 0, "config check, short"),
        pin_code(&["config", "set", "--help"], 0, "config set"),
        pin_code(&["config", "set", "-h"], 0, "config set, short"),
        pin_code(&["config", "unset", "--help"], 0, "config unset"),
        pin_code(&["config", "unset", "-h"], 0, "config unset, short"),
        pin_code(&["config", "retired-env", "--help"], 0, "config retired-env"),
        pin_code(&["config", "retired-env", "-h"], 0, "config retired-env, short"),
        pin_code(&["config", "recorder", "--help"], 0, "config recorder"),
        pin_code(&["config", "activate", "--help"], 0, "config activate"),
        pin_code(&["config", "activate", "-h"], 0, "config activate, short"),
        pin_code(&["config", "deactivate", "--help"], 0, "config deactivate"),
        pin_code(&["config", "bootstrap-daemon", "--help"], 0, "config bootstrap-daemon"),
        pin_code(&["config", "bootstrap-recorder", "--help"], 0, "config bootstrap-recorder"),
        pin_code(&["config", "bootstrap-run", "--help"], 0, "config bootstrap-run"),
        pin_code(&["secrets", "--help"], 0, "secrets grammar: the roster"),
        pin_code(&["secrets", "-h"], 0, "secrets grammar: short"),
        pin_code(&["secrets", "help"], 0, "secrets grammar: the word form"),
        pin_code(&["secrets", "list", "--help"], 0, "secrets list"),
        pin_code(&["secrets", "path", "-h"], 0, "secrets path"),
        pin_code(
            &["secrets", "set", "--help"],
            0,
            "secrets set (the help arm precedes the KEY arm)",
        ),
        pin_code(&["secrets", "set", "-h"], 0, "secrets set, short"),
        pin_code(&["secrets", "delete", "--help"], 0, "secrets delete"),
        pin_code(&["secrets", "init", "--help"], 0, "secrets init"),
        pin_code(&["secrets", "accounts", "--help"], 0, "secrets accounts"),
        pin_code(&["secrets", "set-book", "--help"], 0, "secrets set-book"),
        pin_code(&["secrets", "confirm", "--help"], 0, "secrets confirm"),
        pin_code(&["secrets", "account", "--help"], 0, "secrets account"),
        pin_code(&["secrets", "account", "add", "--help"], 0, "secrets account <action>"),
        pin_code(&["secrets", "copy-hub-keys", "--help"], 0, "secrets copy-hub-keys"),
        pin_code(&["secrets", "ibc-start", "--help"], 0, "docs/ops: `secrets ibc-start --help`"),
        pin_code(&["secrets", "ibkr-cp-login", "--help"], 0, "secrets ibkr-cp-login"),
        pin_code(&["backend", "--help"], 0, "node/mod.rs: skills/connect-to-a-node.md"),
        pin_code(&["backend", "-h"], 0, "backend, short"),
        pin_code(&["backend", "help"], 0, "backend, the word form"),
        pin_code(&["backend", "setup", "--help"], 0, "backend setup"),
        pin_code(&["backend", "admin-key", "--help"], 0, "backend admin-key"),
        pin_code(&["backend", "connect", "--help"], 0, "backend connect, no HOST needed for help"),
        pin_code(&["backend", "status", "--help"], 0, "backend status"),
        pin_code(&["backend", "disconnect", "--help"], 0, "backend disconnect"),
        pin_code(&["backend", "ping", "--help"], 0, "backend ping"),
        pin_code(&["datahub", "--help"], 0, "datahub"),
        pin_code(&["datahub", "-h"], 0, "datahub, short"),
        pin_code(&["datahub", "help"], 0, "datahub, the word form"),
        pin_code(&["datahub", "setup", "--help"], 0, "datahub setup"),
        pin_code(&["datahub", "control-key", "-h"], 0, "datahub control-key"),
        pin_code(&["gatewayhub", "--help"], 0, "gatewayhub"),
        pin_code(&["gatewayhub", "-h"], 0, "gatewayhub, short"),
        pin_code(&["gatewayhub", "help"], 0, "gatewayhub, the word form"),
        pin_code(&["gatewayhub", "setup", "--help"], 0, "gatewayhub setup"),
        pin_code(&["mcp", "--help"], 0, "mcp"),
        pin_code(&["mcp", "-h"], 0, "mcp, short"),
        pin_code(&["report", "--help"], 0, "report"),
        pin_code(&["report", "-h"], 0, "report, short"),
        pin_code(&["research", "--help"], 0, "research"),
        pin_code(&["research", "-h"], 0, "research, short"),
        pin_code(&["research", "help"], 0, "research, the word form"),
        pin_code(&["research", "study", "--help"], 0, "research study"),
        pin_code(&["research", "study", "-h"], 0, "research study, short"),
        pin_code(&["init", "--help"], 0, "init"),
        pin_code(&["init", "-h"], 0, "init, short"),
        pin_code(&["indicators", "--help"], 0, "indicators"),
        pin_code(&["indicators", "-h"], 0, "indicators, short"),
        pin_code(&["surface", "--help"], 0, "surface"),
        pin_code(&["surface", "-h"], 0, "surface, short"),
    ]);
}

/// `config show`, `config check`, `config retired-env`, `config recorder`: the read verbs.
#[test]
fn config_read_verbs() {
    assert_accepted(&[
        // show
        pin(&["config", "show"], "README.md; docs/ops: the provenance table"),
        pin(&["config", "show", "--json"], "scripts/cli_mcp_smoke.sh: config show --json"),
        pin(&["config", "show", "--changed-only"], "cmd/config.rs module doc"),
        pin(&["config", "show", "--filter", "policy"], "docs/ops: config show --filter policy"),
        pin(&["config", "show", "--filter", "notional"], "docs/ops: config show --filter notional"),
        pin(&["config", "show", "--filter", "risk"], "the run-profile row: read the numbers back"),
        pin(&["config", "show", "--filter", "tradehub_live"], "activate.rs RUN_ARMING_WARNING"),
        pin(&["config", "show", "--filter", "bridges/okx"], "module doc: a crate path filter"),
        pin(&["config", "show", "--section", "files"], "cmd/config.rs: --section files"),
        pin(&["config", "show", "--section", "env"], "cmd/config.rs: --section env"),
        pin(&["config", "show", "--section", "all"], "cmd/config.rs: --section all"),
        pin(
            &["config", "show", "--section", "files", "--filter", "notional"],
            "docs/ops: --section files --filter notional",
        ),
        pin(
            &[
                "config",
                "show",
                "--json",
                "--filter",
                "reconcile",
                "--changed-only",
                "--section",
                "all",
            ],
            "every show flag at once",
        ),
        pin(&["config", "show", "--filter=policy"], "inline form of --filter"),
        pin(&["config", "show", "--section=env", "--filter=flags"], "inline forms"),
        pin(&["config", "show", "--filter", "a=b"], "a value containing `=` is taken raw"),
        pin(&["config", "show", "--filter=a=b"], "inline value containing `=` splits on the first"),
        // check
        pin(&["config", "check"], "every deploy/*.service ExecStartPre= and docker pre-flight"),
        pin(&["config", "check", "--strict"], "docs/ops: config check --strict"),
        pin(&["config", "check", "--json"], "entrypoint.sh hint; cli_mcp_smoke.sh"),
        pin(&["config", "check", "--strict", "--json"], "both switches"),
        pin(&["config", "check", "--json", "--strict"], "both switches, other order"),
        // retired-env (stdin is closed: no input, no retired variable, exit 0)
        pin_code(
            &["config", "retired-env"],
            0,
            "deploy/sbin/vike-trader-ci-deploy: `vike-cli config retired-env < environment`",
        ),
        // recorder
        pin(&["config", "recorder"], "docs/ops/recorder-deploy.md: what the store holds"),
        pin(&["config", "recorder", "--name", "default"], "recorder.rs: --name"),
        pin(&["config", "recorder", "--name=default"], "recorder.rs: inline --name"),
    ]);
}

/// `config set <key> <value>`: two positionals, `--` ends options, the value is never split on `=`.
#[test]
fn config_set() {
    assert_accepted(&[
        pin(
            &["config", "set", "preferences.log_file_level", "warn"],
            "scripts/cli_mcp_smoke.sh; docs/ops/*.md: the log level row",
        ),
        pin(&["config", "set", "flags.reconcile_off", "true"], "docs/ops/reconcile-on-restart.md"),
        pin(&["config", "set", "flags.tradehub_live", "true"], "the live mount recipes"),
        pin(&["config", "set", "flags.tradehub_live", "false"], "docs/ops: disarm"),
        pin(&["config", "set", "flags.tradehub_control", "true"], "cli_mcp_smoke.sh row"),
        pin(&["config", "set", "flags.poly_reconcile", "false"], "docs/ops: polymarket reconcile"),
        pin(&["config", "set", "flags.poly_exec", "false"], "docs/ops: polymarket exec"),
        pin(&["config", "set", "flags.cancel_orders_on_shutdown", "true"], "docs/ops"),
        pin(&["config", "set", "flags.journal_materializer", "true"], "docs/ops: journal wave"),
        pin(&["config", "set", "flags.datahub_live", "true"], "docs/ops: datahub live"),
        pin(&["config", "set", "policy.max_notional_per_order", "250"], "docs/ops: a risk ceiling"),
        pin(&["config", "set", "policy.max_notional_per_order", "25"], "docs/ops: a risk ceiling"),
        pin(&["config", "set", "policy.max_account_exposure", "5000"], "docs/ops: <n> substituted"),
        pin(&["config", "set", "policy.max_sizing_equity", "250000"], "docs/ops"),
        pin(&["config", "set", "policy.max_mount_loss", "50"], "docs/ops: a mount's loss budget"),
        pin(&["config", "set", "policy.mount_loss_action", "cancel_only"], "docs/ops: the mode"),
        pin(
            &["config", "set", "policy.deadman_timeout_ms", "60000"],
            "skills/arm-the-dead-man-switches",
        ),
        pin(&["config", "set", "policy.deadman_timeout_ms", "0"], "skills: 0 turns it off"),
        pin(&["config", "set", "policy.link_deadman_grace_ms", "0"], "skills: 0 turns it off"),
        pin(
            &["config", "set", "policy.deadman_action", "cancel_all_and_halt"],
            "skills: the default",
        ),
        pin(&["config", "set", "policy.deadman_action", "cancel_all"], "skills"),
        pin(&["config", "set", "policy.halt_admit", "verify"], "docs/ops"),
        pin(
            &["config", "set", "config.tradehub_addr", "127.0.0.1:7879"],
            "docs/ops; backend setup",
        ),
        pin(&["config", "set", "config.datahub_addr", "127.0.0.1:7878"], "docs/ops: <addr>"),
        pin(&["config", "set", "config.backtest_addr", "127.0.0.1:7882"], "docs/ops"),
        pin(&["config", "set", "config.journal_dir", "journal"], "docs/ops: a RELATIVE dir here"),
        pin(&["config", "set", "config.reconcile_policy", "quarantine"], "docs/ops/reconcile"),
        pin(
            &["config", "set", "config.reconcile_policy", "external-quarantine"],
            "docs/ops/reconcile: a hyphenated word",
        ),
        pin(&["config", "set", "config.instance_origin", "west"], "docs/ops"),
        pin(&["config", "set", "config.tradehub_control_rate", "20"], "docs/ops"),
        pin(
            &["config", "set", "config.pin_cores", "md:2,core:3,exec:4"],
            "docs/ops: colons and commas",
        ),
        pin(&["config", "set", "venue.polymarket.proxy_enabled", "false"], "docs/ops: a venue row"),
        pin(&["config", "set", "venue.polymarket.rate_gate", "1"], "docs/ops: a venue row"),
        pin(
            &["config", "set", "venue.polymarket.proxy_host", "host.docker.internal"],
            "docs/ops/tradehub-container.md",
        ),
        pin(
            &["config", "set", "venue.ibkr.demo.backend", "cpapi"],
            "docs/ops/ibkr-cpapi-gateway.md",
        ),
        pin(&["config", "set", "venue.ibkr.live.backend", "socket"], "docs/ops: a tier-scoped row"),
        pin(&["config", "set", "venue.ibkr.live.port", "4101"], "docs/ops: the live port"),
        pin(
            &["config", "set", "venue.ibkr.demo.cpapi_url", "https://127.0.0.1:5000"],
            "docs/ops: a URL value",
        ),
        // The shapes a reader must not mis-tokenise.
        pin(&["config", "set", "config.journal_dir", "a=b"], "the VALUE is never split on `=`"),
        pin(&["config", "set", "config.journal_dir", "a=b=c"], "the VALUE keeps every `=`"),
        pin(
            &["config", "set", "--", "config.journal_dir", "--dark"],
            "set.rs SET_USAGE: `--` makes everything after it positional",
        ),
        pin(
            &["config", "set", "config.journal_dir", "--", "--dark"],
            "set.rs SET_USAGE: a value beginning with `--`, after the key",
        ),
        pin(
            &["config", "set", "policy.link_deadman_grace_ms", "-1"],
            "a single-dash value is a positional",
        ),
        pin(&["config", "set", "config.journal_dir", "with space"], "one argv token with a space"),
    ]);
}

/// `config unset <key>`: ONE positional and nothing else (decision 0161). The lines are the ones
/// `docs/ops/config-unset.md` and the refusals' own text tell an operator to type; in the sandbox
/// there is no database, so each ends in a refused local precondition (1) AFTER the reader accepted
/// it. The lines the reader REFUSES on purpose — no key, a second token, `KEY=VALUE` — are pinned as
/// usage errors so a rewrite keeps refusing them.
#[test]
fn config_unset() {
    assert_accepted(&[
        pin(
            &["config", "unset", "policy.max_total_exposure"],
            "docs/ops/config-unset.md: the retired-key cure; also the tombstone refusals' text",
        ),
        pin(&["config", "unset", "preferences.rate_utilization"], "docs/ops/config-unset.md"),
        pin(&["config", "unset", "policy.rate.max_utilization"], "a dotted in-section key"),
        pin(
            &["config", "unset", "policy.link_deadman_grace_ms"],
            "the link dead-man warning's way back to its default",
        ),
        pin(&["config", "unset", "flags.reconcile_off"], "docs/ops/config-unset.md: a flag"),
        pin_code(&["config", "unset"], 2, "unset.rs: a KEY is required"),
        pin_code(&["config", "unset", "config.log_dir", "x"], 2, "unset.rs: no value, one key"),
        pin_code(&["config", "unset", "config.log_dir=x"], 2, "unset.rs: KEY=VALUE is refused"),
    ]);
}

/// `config activate`, `config deactivate`: the profile-selection verbs.
#[test]
fn config_activate_deactivate() {
    assert_accepted(&[
        pin(
            &["config", "activate", "daemon", "default"],
            "docs/ops: `config activate daemon <name>`",
        ),
        pin(
            &["config", "activate", "run", "run-live", "--dry-run"],
            "activate.rs: run + --dry-run",
        ),
        pin(&["config", "activate", "recorder", "default"], "docs/ops/recorder-deploy.md"),
        pin(
            &["config", "activate", "--dry-run", "daemon", "default"],
            "flags interleaved before the positionals",
        ),
        pin(&["config", "deactivate", "run"], "activate.rs DEACTIVATE_USAGE"),
        pin(&["config", "deactivate", "daemon", "--dry-run"], "deactivate with --dry-run"),
        pin(&["config", "deactivate", "--dry-run", "recorder"], "flag before the kind"),
    ]);
}

/// `config bootstrap-daemon <name> --venue .. --asset-class .. (--symbol | --token-id) ..`.
#[test]
fn config_bootstrap_daemon() {
    assert_accepted(&[
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "default",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
            ],
            "docs/ops/tradehub-container.md: the first-run command",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-daemon",
                "default",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
                "--dry-run",
            ],
            0,
            "the same, rehearsed (builds the row in memory, writes nothing)",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "binance-live",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1m",
                "--tick-size",
                "0.01",
                "--qty",
                "0.0001",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.02",
            ],
            "the binance live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "bybit-live",
                "--venue",
                "bybit",
                "--asset-class",
                "CryptoPerp",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1m",
                "--tick-size",
                "0.1",
                "--qty",
                "0.001",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.2",
            ],
            "the bybit live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "okx-live",
                "--venue",
                "okx",
                "--asset-class",
                "CryptoPerp",
                "--symbol",
                "BTC-USDT-SWAP",
                "--interval",
                "1m",
                "--tick-size",
                "0.1",
                "--qty",
                "0.01",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.2",
            ],
            "the okx live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "aster-live",
                "--venue",
                "aster",
                "--asset-class",
                "CryptoPerp",
                "--symbol",
                "BTCUSDT.P",
                "--interval",
                "1m",
                "--tick-size",
                "0.1",
                "--qty",
                "0.001",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.02",
            ],
            "the aster live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "deribit-live",
                "--venue",
                "deribit",
                "--asset-class",
                "CryptoPerp",
                "--symbol",
                "BTC-PERPETUAL",
                "--interval",
                "1m",
                "--tick-size",
                "0.5",
                "--qty",
                "10",
                "--seed-cash",
                "10000",
                "--half-spread",
                "1",
            ],
            "the deribit live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "alpaca-live",
                "--venue",
                "alpaca",
                "--asset-class",
                "Equity",
                "--symbol",
                "AAPL",
                "--interval",
                "1m",
                "--tick-size",
                "0.01",
                "--qty",
                "1",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.02",
            ],
            "the alpaca live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "oanda-live",
                "--venue",
                "oanda",
                "--asset-class",
                "Fx",
                "--symbol",
                "EURUSD",
                "--interval",
                "1m",
                "--tick-size",
                "0.00001",
                "--qty",
                "1000",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.0001",
                "--shutdown-deadline-ms",
                "10000",
            ],
            "the OANDA live recipe: --shutdown-deadline-ms",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "oanda-data",
                "--venue",
                "oanda",
                "--asset-class",
                "Fx",
                "--symbol",
                "EURUSD",
                "--data-only",
            ],
            "the OANDA data-only live recipe: --data-only",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "ctrader-live",
                "--venue",
                "ctrader",
                "--asset-class",
                "Fx",
                "--symbol",
                "EURUSD",
                "--interval",
                "1m",
                "--tick-size",
                "0.00001",
                "--qty",
                "1000",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.0001",
            ],
            "the ctrader live mount recipe",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "ig-live",
                "--venue",
                "ig",
                "--asset-class",
                "Fx",
                "--symbol",
                "CS.D.EURUSD.MINI.IP",
                "--interval",
                "1m",
                "--tick-size",
                "0.0001",
                "--qty",
                "1",
                "--seed-cash",
                "10000",
                "--half-spread",
                "0.0002",
            ],
            "the IG live recipe: dots in the symbol",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "cli-mcp-smoke",
                "--venue",
                "polymarket",
                "--asset-class",
                "PredictionMarket",
                "--token-id",
                "123456",
            ],
            "scripts/cli_mcp_smoke.sh and vike-agent-eval: --token-id",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-daemon",
                "all-flags",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
                "--interval",
                "1m",
                "--interval-ms",
                "60000",
                "--qty",
                "0.0001",
                "--half-spread",
                "0.02",
                "--tick-size",
                "0.01",
                "--seed-cash",
                "10000",
                "--summary-ms",
                "5000",
                "--shutdown-deadline-ms",
                "9000",
                "--account",
                "HEDGE",
                "--data-only",
                "--dry-run",
            ],
            0,
            "BOOTSTRAP_DAEMON_USAGE: every flag once, rehearsed",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-daemon",
                "inline",
                "--venue=binance",
                "--asset-class=CryptoSpot",
                "--symbol=BTCUSDT",
                "--interval=1m",
                "--interval-ms=60000",
                "--qty=0.5",
                "--half-spread=0.02",
                "--tick-size=0.01",
                "--seed-cash=10000",
                "--summary-ms=5000",
                "--shutdown-deadline-ms=9000",
                "--account=HEDGE",
                "--dry-run",
            ],
            0,
            "the inline `--flag=value` spelling of every valued flag",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-daemon",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
                "--dry-run",
                "late-name",
            ],
            0,
            "the NAME may come after the flags",
        ),
        pin(
            &[
                "config",
                "bootstrap-daemon",
                "neg",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "BTCUSDT",
                "--qty",
                "-1",
            ],
            "a single-dash value (a negative number) is a value",
        ),
    ]);
}

/// `config bootstrap-recorder <name> --store .. --venue .. (--family | --symbols) ..`.
#[test]
fn config_bootstrap_recorder() {
    assert_accepted(&[
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "flip-dry-run",
                "--store",
                "hist",
                "--venue",
                "binance",
                "--family",
                "spot",
            ],
            "docs/ops/recorder-deploy.md: `bootstrap-recorder <name> --store <root> ..`",
        ),
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "mm-probe",
                "--store",
                "scratch",
                "--venue",
                "X",
                "--family",
                "Y",
                "--max-merge-rows",
                "1000000",
            ],
            "docs/ops/recorder-deploy.md: --max-merge-rows",
        ),
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "syms",
                "--store",
                "hist",
                "--venue",
                "binance",
                "--symbols",
                "BTCUSDT,ETHUSDT,SOLUSDT",
                "--dry-run",
            ],
            "BOOTSTRAP_RECORDER_USAGE: --symbols is a comma list",
        ),
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "all-flags",
                "--store",
                "hist",
                "--venue",
                "binance",
                "--family",
                "spot",
                "--interval-secs",
                "60",
                "--min-parts",
                "4",
                "--target-mb",
                "256",
                "--max-merge-rows",
                "1000000",
                "--retention-days",
                "30",
                "--webhooks",
                "ops,pager",
                "--alert-repeat-secs",
                "300",
                "--alert-series-prefix",
                "rec",
                "--note",
                "probe",
                "--dry-run",
            ],
            "BOOTSTRAP_RECORDER_USAGE: every flag once",
        ),
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "inline",
                "--store=hist",
                "--venue=binance",
                "--symbols=BTCUSDT",
                "--interval-secs=60",
                "--min-parts=4",
                "--target-mb=256",
                "--max-merge-rows=1",
                "--retention-days=30",
                "--webhooks=ops",
                "--alert-repeat-secs=300",
                "--alert-series-prefix=rec",
                "--note=probe",
                "--dry-run",
            ],
            "the inline spelling of every valued flag",
        ),
        pin(
            &[
                "config",
                "bootstrap-recorder",
                "--store",
                "hist",
                "--venue",
                "binance",
                "--family",
                "spot",
                "--dry-run",
                "name-last",
            ],
            "the NAME may come after the flags",
        ),
    ]);
}

/// `config bootstrap-run <name> [--<dotted.path> <value> ..]` (decision 0156 item 7: the run profile
/// states no `mode`, so the line each of these once began with `--mode <word>` no longer does).
#[test]
fn config_bootstrap_run() {
    assert_accepted(&[
        pin_code(
            &["config", "bootstrap-run", "run-paper", "--dry-run"],
            0,
            "BOOTSTRAP_RUN_USAGE: the minimal profile, rehearsed",
        ),
        pin(&["config", "bootstrap-run", "run-paper"], "the same, applied (no store: 1)"),
        // REFUSED since decision 0156 item 7: a run profile has no `mode` (it was `--mode
        // <backtest|paper|live>`, REQUIRED), whatever its word.
        pin_code(
            &["config", "bootstrap-run", "run-paper", "--mode", "paper"],
            2,
            "0156: `--mode` is an unknown option, the profile states no tier",
        ),
        pin_code(
            &["config", "bootstrap-run", "run-bt", "--mode", "backtest"],
            2,
            "0156: `--mode backtest` is refused too",
        ),
        pin_code(
            &["config", "bootstrap-run", "run-live", "--mode=live", "--dry-run"],
            2,
            "0156: the inline `--mode=live` spelling is refused as well",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-run",
                "run-live",
                "--risk.max_notional_per_order",
                "5000",
                "--risk.max_total_exposure",
                "25000",
                "--risk.max_orders_per_window",
                "20",
                "--risk.window_ms",
                "1000",
                "--risk.max_leverage",
                "1.0",
                "--risk.required_free_bp_pct",
                "0.05",
                "--risk.block_reduce_only_overshoot",
                "true",
                "--dry-run",
            ],
            0,
            "the documented live run-profile line, rehearsed",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "run-live",
                "--risk.max_notional_per_order",
                "5000",
                "--risk.max_total_exposure",
                "25000",
                "--risk.max_orders_per_window",
                "20",
                "--risk.window_ms",
                "1000",
                "--risk.max_leverage",
                "1.0",
                "--risk.required_free_bp_pct",
                "0.05",
                "--risk.block_reduce_only_overshoot",
                "true",
            ],
            "the documented live run-profile line, applied",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "run-live",
                "--risk.max_notional_per_order",
                "5000",
                "--risk.max_total_exposure",
                "25000",
            ],
            "bootstrap_run.rs: the two ceilings a demo/live mount needs (a body without them is only WARNED about)",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "run-live",
                "--risk.max_notional_per_order=5000",
                "--risk.max_total_exposure=25000",
                "--dry-run",
            ],
            "dotted-path flags in the inline `=` spelling",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "--risk.max_notional_per_order",
                "5000",
                "run-paper",
                "--dry-run",
            ],
            "the NAME between and after the dotted flags",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "all-keys",
                "--risk.tick_size",
                "0.01",
                "--risk.lot_size",
                "0.001",
                "--risk.min_notional",
                "5",
                "--risk.min_qty",
                "0.001",
                "--risk.max_notional_per_order",
                "5000",
                "--risk.max_total_exposure",
                "25000",
                "--risk.max_orders_per_window",
                "20",
                "--risk.window_ms",
                "1000",
                "--risk.max_leverage",
                "2",
                "--risk.block_reduce_only_overshoot",
                "false",
                "--risk.required_free_bp_pct",
                "0.05",
                "--dry-run",
            ],
            "every [risk] key once (ProfileRisk::keys)",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "all-doc-keys",
                "--sinks.equity_sample_ms",
                "1000",
                "--sinks.journal.segment_bytes",
                "1048576",
                "--sinks.journal.flush_every",
                "100",
                "--sinks.journal.snapshot_every",
                "1024",
                "--guards.initial_trading_state",
                "active",
                "--guards.submit_ack_timeout_ms",
                "5000",
                "--guards.submit_ack_confirm_grace_ms",
                "2000",
                "--guards.max_drawdown",
                "0.15",
                "--guards.conditionals_on_ticks",
                "true",
                "--guards.margin_call.mm_requirement",
                "0.05",
                "--guards.margin_call.warn_fraction",
                "0.8",
                "--guards.margin_call.buffer",
                "0.1",
                "--dry-run",
            ],
            "every DOCUMENT_KEYS path once (sinks, guards, the nested margin_call table)",
        ),
        pin(
            &[
                "config",
                "bootstrap-run",
                "words",
                "--guards.initial_trading_state",
                "reducing",
                "--dry-run",
            ],
            "a closed-vocabulary word",
        ),
        pin(
            &["config", "bootstrap-run", "neg", "--risk.max_leverage", "-1", "--dry-run"],
            "a negative number is a value to a dotted flag (the daemon refuses it later)",
        ),
    ]);
}

/// `secrets list | path | init`, the reads and the one creator.
#[test]
fn secrets_reads_and_init() {
    assert_accepted(&[
        pin(&["secrets", "list"], "README.md; docs/ops: key NAMES only"),
        pin(&["secrets", "list", "--json"], "docs/ops; cli_mcp_smoke.sh: secrets list --json"),
        pin(&["secrets", "path"], "README.md; every ops page: where the store is"),
        pin(
            &["secrets", "init"],
            "deploy/sbin/vike-trader-ci-deploy; README: create the empty store",
        ),
        pin(&["secrets", "init", "--dry-run"], "docs/ops: rehearse init"),
        pin(&["secrets", "accounts"], "scripts/skills/arm-a-venue.md: the account table"),
        pin(&["secrets", "confirm"], "secrets.rs USAGE: fold the parked confirmations"),
        pin(&["secrets", "confirm", "--dry-run"], "secrets.rs USAGE: rehearse the fold"),
    ]);
}

/// `secrets set` (FIELD or NAME; value on stdin or `--from-env`, never argv) and `secrets delete KEY`.
#[test]
fn secrets_set_and_delete() {
    assert_accepted(&[
        // Decision 0156 item 6: an account's credential is a FIELD of an EXISTING account row.
        pin(
            &["secrets", "set", "--account", "7", "API_KEY"],
            "secrets.rs USAGE: the value on stdin, aimed at an account row",
        ),
        pin(
            &["secrets", "set", "--account", "7", "API_SECRET", "--from-env", "SECRET_VAR"],
            "secrets.rs USAGE: the CI/deploy form",
        ),
        pin(
            &["secrets", "set", "--account=7", "API_KEY", "--from-env=SECRET_VAR"],
            "the inline spelling of both flags",
        ),
        pin(
            &["secrets", "set", "--venue", "binance", "API_KEY"],
            "secrets.rs USAGE: the one active unlabelled account of a venue",
        ),
        pin(
            &["secrets", "set", "--venue", "binance", "--label", "HEDGE", "API_KEY"],
            "secrets.rs USAGE: a labelled account of a venue",
        ),
        pin(
            &["secrets", "set", "API_KEY", "--venue", "binance", "--label", "HEDGE"],
            "the FIELD positional first, flags after",
        ),
        pin(
            &["secrets", "set", "--venue", "ctrader", "CLIENT_ID"],
            "a VENUE-wide field takes --venue alone",
        ),
        pin_code(
            &["secrets", "set", "--account", "7", "--venue", "binance", "API_KEY"],
            2,
            "0156: an id IS an account; a venue beside it is refused",
        ),
        pin_code(
            &["secrets", "set", "--label", "HEDGE", "API_KEY"],
            2,
            "0156: a label names an account of a venue, so it needs --venue",
        ),
        pin_code(
            &["secrets", "set", "--account", "seven", "API_KEY"],
            2,
            "0156: --account takes a row id",
        ),
        pin_code(&["secrets", "list", "--account", "7"], 2, "0156: --account is `set`'s alone"),
        pin(
            &["secrets", "set", "BINANCE_LIVE_API_KEY"],
            "the retired tiered NAME: the reader accepts it, the verb refuses it by name later (0156)",
        ),
        pin(
            &["secrets", "set", "BYBIT_DEMO_API_SECRET"],
            "the retired tiered NAME: reader-accepted, refused at run time (0156)",
        ),
        pin(&["secrets", "set", "IBKR_DEMO_USERNAME"], "docs/ops/ibkr-*.md: the login pair"),
        pin(&["secrets", "set", "IBKR_LIVE_PASSWORD"], "docs/ops/ibkr-*.md: the login pair"),
        pin(
            &["secrets", "set", "OANDA_DEMO_API_KEY"],
            "the retired tiered NAME: reader-accepted, refused at run time (0156)",
        ),
        pin(
            &["secrets", "set", "VIKE_ALERT_TELEGRAM_TOKEN"],
            "docs/ops: the pager token, on stdin",
        ),
        pin(&["secrets", "set", "VIKE_ALERT_TELEGRAM_CHAT_ID"], "docs/ops: the pager chat id"),
        pin(&["secrets", "set", "VIKE_ALERT_WEBHOOK_URL"], "docs/ops: the generic webhook url"),
        pin(
            &["secrets", "set", "BINANCE_LIVE_API_KEY", "--from-env", "BINANCE_KEY"],
            "secrets.rs USAGE: the CI/deploy form",
        ),
        pin(
            &["secrets", "set", "BYBIT_DEMO_API_SECRET", "--from-env", "VIKE_SMOKE_SECRET_SOURCE"],
            "scripts/cli_mcp_smoke.sh: --from-env",
        ),
        pin(
            &["secrets", "set", "BINANCE_LIVE_API_KEY", "--from-env=BINANCE_KEY"],
            "the inline spelling of --from-env",
        ),
        pin(
            &["secrets", "set", "--from-env", "BINANCE_KEY", "BINANCE_LIVE_API_KEY"],
            "the KEY positional after the flag",
        ),
        pin(
            &["secrets", "set", "BINANCE_LIVE_API_KEY__HEDGE"],
            "the retired KEY__LABEL NAME: reader-accepted, refused at run time (0156)",
        ),
        // delete
        pin(
            &["secrets", "delete", "BINANCE_LIVE_API_KEY", "--confirm", "BINANCE_LIVE_API_KEY"],
            "secrets.rs USAGE: the typed confirm",
        ),
        pin(
            &[
                "secrets",
                "delete",
                "BINANCE_LIVE_API_KEY",
                "--confirm",
                "BINANCE_LIVE_API_KEY",
                "--dry-run",
            ],
            "secrets.rs USAGE: --dry-run after the confirm",
        ),
        pin(
            &[
                "secrets",
                "delete",
                "BINANCE_LIVE_API_KEY",
                "--dry-run",
                "--confirm",
                "BINANCE_LIVE_API_KEY",
            ],
            "secrets.rs USAGE: the documented order",
        ),
        pin(
            &["secrets", "delete", "BINANCE_LIVE_API_KEY", "--confirm=BINANCE_LIVE_API_KEY"],
            "the inline spelling of --confirm",
        ),
        pin(
            &[
                "secrets",
                "delete",
                "--dry-run",
                "--confirm",
                "BINANCE_LIVE_API_KEY",
                "BINANCE_LIVE_API_KEY",
            ],
            "the KEY positional last, after the flags",
        ),
    ]);
}

/// `secrets set-book` and the `secrets account <action>` family (all go to the account table).
#[test]
fn secrets_accounts_and_account_actions() {
    assert_accepted(&[
        pin(
            &["secrets", "set-book", "--id", "7", "--venue-account-id", "1234567"],
            "grammar.rs set-book message: the documented line (1234567 is made up)",
        ),
        pin(
            &["secrets", "set-book", "--id", "7", "--venue-account-id", "1234567", "--dry-run"],
            "set-book --dry-run echoes the row",
        ),
        pin(
            &["secrets", "set-book", "--id", "7", "--venue-account-id", "1234567", "--replace"],
            "set-book --replace",
        ),
        pin(&["secrets", "set-book", "--id", "7", "--clear"], "set-book --clear"),
        pin(
            &["secrets", "set-book", "--id", "7", "--clear", "--dry-run"],
            "set-book --clear rehearsed",
        ),
        pin(
            &["secrets", "set-book", "--id=7", "--venue-account-id=1234567"],
            "inline spellings of --id and --venue-account-id",
        ),
        pin(
            &["secrets", "set-book", "--venue-account-id", "1234567", "--id", "7"],
            "the two flags in the other order",
        ),
        // account <action>
        pin(
            &[
                "secrets", "account", "add", "--venue", "binance", "--tier", "demo", "--label",
                "HEDGE",
            ],
            "scripts/skills/arm-a-venue.md: account add (with a label)",
        ),
        pin(
            &["secrets", "account", "add", "--venue", "binance", "--tier", "demo", "--no-label"],
            "account add --no-label",
        ),
        pin(
            &[
                "secrets",
                "account",
                "add",
                "--venue",
                "binance",
                "--tier",
                "live",
                "--label",
                "HEDGE",
                "--dry-run",
            ],
            "account add rehearsed",
        ),
        pin(
            &["secrets", "account", "add", "--venue=binance", "--tier=LIVE", "--label=HEDGE"],
            "inline forms; --tier is case-insensitive",
        ),
        pin(
            &["secrets", "account", "--venue", "binance", "--tier", "Demo", "--no-label", "add"],
            "the ACTION word after the flags; a mixed-case tier",
        ),
        pin(
            &["secrets", "account", "rename", "--id", "7", "--label", "SWISS"],
            "account_lifecycle.rs usage line",
        ),
        pin(
            &["secrets", "account", "rename", "--id", "7", "--no-label"],
            "account rename --no-label",
        ),
        pin(
            &["secrets", "account", "rename", "--id", "7", "--label", "SWISS", "--dry-run"],
            "account rename rehearsed",
        ),
        pin(
            &["secrets", "account", "set-tier", "--id", "7", "--tier", "demo"],
            "scripts/skills/arm-a-venue.md; docs/ops",
        ),
        pin(
            &["secrets", "account", "set-tier", "--id", "7", "--tier", "live", "--dry-run"],
            "account set-tier rehearsed",
        ),
        pin(
            &["secrets", "account", "set-tier", "--tier", "paper", "--id", "7"],
            "the flags in the other order",
        ),
        pin(
            &["secrets", "account", "set-exposure", "--id", "7", "--max-exposure", "5000"],
            "account_lifecycle.rs usage line",
        ),
        pin(
            &["secrets", "account", "set-exposure", "--id", "7", "--max-exposure", "none"],
            "set-exposure: `none` clears the ceiling",
        ),
        pin(
            &["secrets", "account", "set-exposure", "--id", "7", "--max-exposure", "-5"],
            "a single-dash value is accepted by the reader (refused later by the store)",
        ),
        pin(
            &["secrets", "account", "set-exposure", "--max-exposure=2500.5", "--id=7", "--dry-run"],
            "inline forms and a fractional figure",
        ),
        pin(
            &["secrets", "account", "set-leverage", "--id", "11", "--leverage", "10"],
            "docs/ops/trading-any-symbol.md, the leverage section",
        ),
        pin(
            &["secrets", "account", "set-leverage", "--id", "11", "--leverage", "none"],
            "set-leverage: `none` clears the leverage",
        ),
        pin(&["secrets", "account", "activate", "--id", "7"], "scripts/skills/arm-a-venue.md"),
        pin(&["secrets", "account", "activate", "--id", "7", "--dry-run"], "activate rehearsed"),
        pin(
            &["secrets", "account", "deactivate", "--id", "7"],
            "docs/ops, skills: deactivate --id <N>",
        ),
        pin(
            &["secrets", "account", "deactivate", "--id", "7", "--dry-run"],
            "deactivate rehearsed",
        ),
        pin(
            &["secrets", "account", "remove", "--id", "7", "--confirm", "7"],
            "account remove with its typed confirm (the id)",
        ),
        pin(
            &["secrets", "account", "remove", "--id", "7", "--confirm", "7", "--dry-run"],
            "account remove rehearsed",
        ),
        pin(&["secrets", "account", "remove", "--confirm=7", "--id=7"], "inline forms"),
    ]);
}

/// `secrets copy-hub-keys`, `secrets ibc-start`, `secrets ibkr-cp-login`: the launchers never run
/// anything in a sandbox (the driver or the install is missing), so they answer 1 or 4.
#[test]
fn secrets_copy_hub_keys_and_ibkr_launchers() {
    assert_accepted(&[
        pin(
            &["secrets", "copy-hub-keys", "--from-settings-dir", "old/settings"],
            "docs/ops: copy-hub-keys --from-settings-dir <old ..>",
        ),
        pin(
            &["secrets", "copy-hub-keys", "--from-settings-dir", "old/settings", "--dry-run"],
            "copy-hub-keys rehearsed",
        ),
        pin(
            &[
                "secrets",
                "copy-hub-keys",
                "--from-settings-dir",
                "old/settings",
                "--only",
                "datahub",
            ],
            "docs/ops: --only datahub",
        ),
        pin(
            &[
                "secrets",
                "copy-hub-keys",
                "--from-settings-dir",
                "old/settings",
                "--only",
                "tradehub",
                "--replace",
                "--dry-run",
            ],
            "every copy-hub-keys flag at once",
        ),
        pin(
            &["secrets", "copy-hub-keys", "--from-settings-dir=old/settings", "--only=datahub"],
            "inline forms",
        ),
        pin(
            &[
                "secrets",
                "copy-hub-keys",
                "--from-settings-dir",
                "old/settings",
                "--only",
                "gatewayhub",
            ],
            "docs/ops/datahub-the CI box.md: --only gatewayhub",
        ),
        // ibkr-cp-login
        pin(&["secrets", "ibkr-cp-login"], "deploy/vike-ibkr-cpapi-login.service ExecStart="),
        pin(
            &["secrets", "ibkr-cp-login", "--check"],
            "docs/ops/ibkr-cpapi-gateway.md: ExecCondition",
        ),
        pin(&["secrets", "ibkr-cp-login", "--tier", "live"], "docs/ops/ibkr-cpapi-gateway.md"),
        pin(
            &["secrets", "ibkr-cp-login", "--check", "--tier", "live"],
            "docs/ops/ibkr-cpapi-gateway.md: the live probe",
        ),
        pin(&["secrets", "ibkr-cp-login", "--tier", "demo"], "the explicit default tier"),
        pin(
            &["secrets", "ibkr-cp-login", "--check", "--tier=LIVE"],
            "inline; a tier word is case-insensitive",
        ),
        pin(&["secrets", "ibkr-cp-login", "--tier", "Live", "--check"], "flags in the other order"),
        // ibc-start
        pin(&["secrets", "ibc-start", "--check"], "docs/ops/ibkr-*.md: ibc-start --check"),
        pin(&["secrets", "ibc-start", "--check", "--tier", "live"], "docs/ops/ibkr-*.md"),
        pin(
            &[
                "secrets",
                "ibc-start",
                "--root",
                "bin/ibkr-gateway",
                "--gateway-version",
                "1045",
                "--java-path",
                "jre/bin",
            ],
            "grammar.rs: the three required operands",
        ),
        pin(
            &[
                "secrets",
                "ibc-start",
                "--root",
                "bin/ibkr-gateway",
                "--gateway-version",
                "1045",
                "--java-path",
                "jre/bin",
                "--tier",
                "live",
            ],
            "the same for the live gateway",
        ),
        pin(
            &[
                "secrets",
                "ibc-start",
                "--root=bin/ibkr-gateway",
                "--gateway-version=10.37",
                "--java-path=jre/bin",
                "--tier=demo",
            ],
            "inline forms; a dotted version",
        ),
        pin(
            &[
                "secrets",
                "ibc-start",
                "--java-path",
                "jre/bin",
                "--gateway-version",
                "1045",
                "--root",
                "bin/ibkr-gateway",
            ],
            "the operands in another order",
        ),
    ]);
}

/// `backend setup | admin-key | connect | status | disconnect | ping`, `datahub setup | control-key`
/// and `gatewayhub setup`.
#[test]
fn backend_and_datahub() {
    assert_accepted(&[
        pin(&["backend", "setup"], "scripts/skills/connect-to-a-node.md; cli_mcp_smoke.sh"),
        pin(&["backend", "setup", "--addr", "127.0.0.1:7879"], "connect-to-a-node.md: --addr"),
        pin(&["backend", "setup", "--control"], "docs/ops: --control also arms the write channel"),
        pin(&["backend", "setup", "--rotate"], "docs/ops: replace a minted pair"),
        pin(
            &["backend", "setup", "--addr", "127.0.0.1:7879", "--control", "--rotate"],
            "connect-to-a-node.md: [--addr ..] [--control] [--rotate]",
        ),
        pin(&["backend", "setup", "--addr=127.0.0.1:7879", "--control"], "inline --addr"),
        pin(&["backend", "admin-key"], "node/mod.rs: the THIRD key"),
        pin(&["backend", "admin-key", "--rotate"], "node/mod.rs: rotate the admin key"),
        pin(
            &["backend", "connect", "my-node", "--no-tunnel"],
            "node/mod.rs: --no-tunnel, HOST positional",
        ),
        pin(
            &["backend", "connect", "127.0.0.1", "--port", "7879", "--no-tunnel"],
            "docs/ops: `connect HOST --port N --no-tunnel`",
        ),
        pin(&["backend", "connect", "127.0.0.1", "--port=7879", "--no-tunnel"], "inline --port"),
        pin(
            &["backend", "connect", "--port", "7879", "--no-tunnel", "127.0.0.1"],
            "the HOST positional last, after the flags",
        ),
        pin(
            &["backend", "connect", "the CI box", "--manual"],
            "connect-to-a-node.md: `connect <host> --manual`",
        ),
        pin(
            &["backend", "connect", "the CI box", "--manual", "--replace", "--no-tunnel"],
            "every connect switch",
        ),
        pin(
            &["backend", "connect", "the CI box"],
            "connect-to-a-node.md: `connect <host>` (no store: 1)",
        ),
        pin(
            &["backend", "connect", "user@my-node", "--port", "7879"],
            "an ssh destination with user@",
        ),
        pin(&["backend", "status"], "connect-to-a-node.md; entrypoint-thin.sh"),
        pin(&["backend", "disconnect"], "connect-to-a-node.md: close the forward"),
        pin(
            &["backend", "disconnect", "--port", "65001"],
            "node/mod.rs: --port on disconnect (an unused port: pkill matches nothing)",
        ),
        pin(&["backend", "ping", "--addr", "127.0.0.1:1"], "node/ping.rs: dial an address"),
        pin(&["backend", "ping", "--addr=127.0.0.1:1", "--json"], "ping --json, inline --addr"),
        pin(&["backend", "ping"], "ping with the resolved default address"),
        // datahub
        pin(&["datahub", "setup"], "docs/ops: mint the datahub pair"),
        pin(&["datahub", "setup", "--rotate"], "docs/ops: `datahub setup --rotate`"),
        pin(&["datahub", "control-key"], "docs/ops: hand the CONTROL key to a pipe (just studio)"),
        // gatewayhub
        pin(&["gatewayhub", "setup"], "decision 0164 verdict 6: mint the gatewayhub pair"),
        pin(
            &["gatewayhub", "setup", "--rotate"],
            "decision 0164 verdict 6: `gatewayhub setup --rotate`",
        ),
    ]);
}

/// `mcp` flags. stdin is closed, so a started server answers EOF with exit 0 (a hang would be
/// killed by the harness with no code, which `pin_code` refuses).
#[test]
fn mcp_flags() {
    assert_accepted(&[
        pin_code(&["mcp"], 0, "README.md: the bare server returns at EOF"),
        pin_code(&["mcp", "--addr", "127.0.0.1:7878"], 0, "mcp/config.rs: --addr"),
        pin_code(&["mcp", "--addr=127.0.0.1:7878"], 0, "inline --addr"),
        pin_code(
            &["mcp", "--backtest-addr", "127.0.0.1:7880"],
            0,
            "mcp/config.rs: --backtest-addr",
        ),
        pin_code(&["mcp", "--backtest-addr=127.0.0.1:7880"], 0, "inline --backtest-addr"),
        pin_code(&["mcp", "--node", "127.0.0.1:7879"], 0, "skills/connect-to-a-node.md"),
        pin_code(&["mcp", "--node=127.0.0.1:7879"], 0, "inline --node"),
        pin_code(
            &[
                "mcp",
                "--addr",
                "127.0.0.1:7878",
                "--backtest-addr",
                "127.0.0.1:7880",
                "--node",
                "127.0.0.1:7879",
            ],
            0,
            "docs/ops: all three planes",
        ),
        pin_code(
            &["mcp", "--addr", "127.0.0.1:7878", "--node", "127.0.0.1:7879"],
            0,
            "connect-to-a-node.md: [--addr ..] [--node <host:port>]",
        ),
        pin_code(&["mcp", "--profile", "full"], 0, "mcp/scope.rs: PROFILES"),
        pin_code(&["mcp", "--profile", "read-only"], 0, "mcp/scope.rs: PROFILES"),
        pin_code(&["mcp", "--profile", "offline"], 0, "mcp/scope.rs: PROFILES"),
        pin_code(&["mcp", "--profile=read-only"], 0, "inline --profile"),
        pin_code(&["mcp", "--deny-tool", "submit_order"], 0, "mcp/config.rs: a served tool name"),
        pin_code(
            &[
                "mcp",
                "--deny-tool",
                "submit_order",
                "--deny-tool",
                "delete_series",
                "--deny-tool=set_setting",
            ],
            0,
            "a repeatable flag, both spellings",
        ),
        pin_code(&["mcp", "--trace"], 0, "docs/ops: into <project>/settings/state/agent/"),
        pin_code(
            &["mcp", "--trace-dir", "agent-log"],
            0,
            "docs/ops: --trace-dir <dir> (relative here)",
        ),
        pin_code(&["mcp", "--trace-dir=agent-log"], 0, "inline --trace-dir"),
        pin_code(&["mcp", "--trace", "--trace-dir", "agent-log"], 0, "last wins: the dir"),
        pin_code(
            &["mcp", "--trace-dir", "agent-log", "--trace"],
            0,
            "last wins: the project state dir",
        ),
        pin_code(&["mcp", "--unattended"], 0, "decision 0040: the unattended runner's flag"),
        pin_code(
            &[
                "mcp",
                "--profile",
                "read-only",
                "--trace-dir",
                "agent-record",
                "--unattended",
                "--node",
                "127.0.0.1:7879",
                "--addr",
                "127.0.0.1:7878",
            ],
            0,
            "vike-agent-eval unattended.rs mcp_argv: the exact scheduled-run argv",
        ),
        pin_code(
            &[
                "mcp",
                "--profile",
                "offline",
                "--deny-tool",
                "list_series",
                "--trace",
                "--unattended",
                "--addr",
                "127.0.0.1:7878",
                "--backtest-addr",
                "127.0.0.1:7880",
                "--node",
                "127.0.0.1:7879",
            ],
            0,
            "every mcp flag at once",
        ),
    ]);
}

/// `report` (a stored run, the live node, the schema) and `research study`.
#[test]
fn report_and_research_study() {
    assert_accepted(&[
        pin_code(&["report", "--schema"], 0, "report.rs USAGE: no run, no node, no key"),
        pin(&["report", "@last"], "report.rs USAGE: the @last selector (no run: 1)"),
        pin(&["report", "@last:backtest"], "report.rs USAGE: @last:<kind>"),
        pin(&["report", "20260101-abc-1"], "report.rs USAGE: a run id or unique prefix"),
        pin(&["report", "baseline/momentum"], "a bare mark spelled with `/`"),
        pin(&["report", "@baseline"], "report.rs USAGE: a @mark"),
        pin(&["report", "@last", "--json"], "report.rs USAGE: --json"),
        pin(&["report", "@last", "--breakdown", "day"], "docs: report <run> --breakdown day"),
        pin(&["report", "@last", "--breakdown", "month", "--json"], "report.rs USAGE: month"),
        pin(&["report", "@last", "--breakdown=day"], "inline --breakdown"),
        pin(&["report", "--breakdown", "day", "@last"], "the selector after the flag"),
        pin(&["report", "--json", "@last", "--breakdown", "day"], "flags around the selector"),
        pin(&["report", "run=with-equals"], "a selector containing `=` is put back whole"),
        pin(&["report", "-1"], "a single-dash positional is a selector (no run: 1)"),
        pin(&["report", "help"], "`help` is a run selector here, not a help word"),
        pin(&["report", "--node", "127.0.0.1:1"], "report.rs USAGE: the live source (no key: 1)"),
        pin(&["report", "--node=127.0.0.1:1", "--json"], "inline --node"),
        pin(
            &[
                "report",
                "--node",
                "127.0.0.1:1",
                "--seed",
                "10000",
                "--periods-per-year",
                "252",
                "--json",
            ],
            "report.rs USAGE: every live flag",
        ),
        pin(
            &["report", "--node", "127.0.0.1:1", "--seed=10000", "--periods-per-year=252"],
            "inline forms",
        ),
        pin(&["report", "--node", "127.0.0.1:1", "--seed", "-5"], "a negative number is a value"),
        // research study (the recipe file is missing, so the reader's acceptance ends in 1)
        pin(
            &[
                "research",
                "study",
                "--study",
                "cohort",
                "--recipe",
                "recipe.toml",
                "--from",
                "2026-01-01",
                "--to",
                "2026-02-01",
            ],
            "study.rs USAGE: the four required flags",
        ),
        pin(
            &[
                "research",
                "study",
                "--study",
                "cohort",
                "--recipe",
                "recipe.toml",
                "--from",
                "2026-01-01T00",
                "--to",
                "1767225600",
                "--addr",
                "127.0.0.1:1",
            ],
            "study.rs USAGE: --addr, an hour spelling and bare unix seconds",
        ),
        pin(
            &[
                "research",
                "study",
                "--study",
                "cohort",
                "--recipe",
                "recipe.toml",
                "--from",
                "a",
                "--to",
                "b",
                "--json",
            ],
            "study.rs USAGE: --json",
        ),
        pin(
            &[
                "research",
                "study",
                "--study=cohort",
                "--recipe=recipe.toml",
                "--from=2026-01-01",
                "--to=2026-02-01",
                "--addr=127.0.0.1:1",
            ],
            "inline forms of every valued flag",
        ),
        pin(
            &[
                "research",
                "study",
                "--to",
                "b",
                "--from",
                "a",
                "--recipe",
                "recipe.toml",
                "--study",
                "cohort",
            ],
            "the flags in another order",
        ),
    ]);
}

/// `init`, `indicators`, `surface`: the offline verbs.
#[test]
fn init_indicators_surface() {
    assert_accepted(&[
        pin(&["init"], "README.md; scripts/mirror_readme.md: scaffold user_data/"),
        pin(&["init", "--dry-run"], "init/mod.rs USAGE"),
        pin(&["init", "--reset"], "init/mod.rs USAGE"),
        pin(&["init", "--json"], "init/mod.rs USAGE"),
        pin(&["init", "--dir", "scaffold"], "init/mod.rs USAGE: --dir PATH (relative here)"),
        pin(&["init", "--dir=scaffold"], "inline --dir"),
        pin(
            &["init", "--dir", "scaffold", "--reset", "--dry-run", "--json"],
            "every init flag at once",
        ),
        pin_code(&["indicators"], 0, "indicators.rs USAGE: the grouped listing"),
        pin_code(&["indicators", "--json"], 0, "indicators.rs USAGE: machine rows"),
        pin(&["indicators", "--name", "sma"], "indicators.rs USAGE: one indicator (or why not)"),
        pin(&["indicators", "--name=sma", "--json"], "inline --name, with --json"),
        pin(
            &["indicators", "--category", "Trend"],
            "indicators.rs USAGE: a category (case-insensitive)",
        ),
        pin(&["indicators", "--category=trend", "--json"], "inline --category, lowercase"),
        pin(
            &["indicators", "--category", "not-a-category"],
            "an unknown category is a run failure (1)",
        ),
        pin_code(&["surface", "--list"], 0, "release.yml / surface.rs: the asset names"),
        pin_code(&["surface"], 0, "surface.rs: print every asset to stdout"),
        pin_code(
            &["surface", "--out", "dist"],
            0,
            "release.yml: `surface --out DIR` (relative here)",
        ),
        pin_code(&["surface", "--out=dist"], 0, "inline --out"),
        pin_code(&["surface", "--list", "--out", "dist"], 0, "--list wins and writes nothing"),
    ]);
}

/// Command lines that WERE accepted by the hand-rolled readers and are REFUSED now, each by a row of
/// decision 0155's rule table (verdict 3): a flag given twice that is not repeatable used to
/// overwrite silently (last wins) and is refused with `clap`'s wording; a switch given a value, a
/// single-dash token that is not a number as a value, and every usage error's exit code (2, where
/// `config` used to exit 1) are the other rows. They are pinned here as REFUSED so a rewrite cannot
/// make them silently accepted again. Moved out of the accepted tests above, unchanged otherwise.
#[test]
fn what_decision_0155_refuses_now() {
    assert_accepted(&[
        pin_code(&["config"], 2, "config.rs run: a missing verb is FAILURE (1), not usage"),
        pin_code(&["config", "no-such-verb"], 2, "config.rs run: an unknown verb is FAILURE (1)"),
        pin_code(
            &["config", "show", "--filter", "-policy"],
            2,
            "a single-dash token is a VALUE (args.rs)",
        ),
        pin_code(
            &["config", "show", "--filter", "first", "--filter", "second", "--json", "--json"],
            2,
            "repeated flags: last wins, a repeated switch is fine",
        ),
        pin_code(
            &["config", "show", "--section", "files", "--section", "env"],
            2,
            "repeated --section: last wins",
        ),
        pin_code(&["config", "check", "--json", "--json"], 2, "a repeated switch"),
        pin_code(
            &[
                "config",
                "bootstrap-daemon",
                "last-wins",
                "--venue",
                "okx",
                "--venue",
                "binance",
                "--asset-class",
                "CryptoSpot",
                "--symbol",
                "A",
                "--symbol",
                "BTCUSDT",
                "--dry-run",
            ],
            2,
            "repeated valued flags: last wins",
        ),
        pin_code(
            &[
                "config",
                "bootstrap-recorder",
                "lw",
                "--store",
                "a",
                "--store",
                "hist",
                "--venue",
                "binance",
                "--symbols",
                "X",
                "--symbols",
                "BTCUSDT",
            ],
            2,
            "repeated --store: last wins; a later --symbols replaces an earlier one",
        ),
        pin_code(
            &["secrets", "set", "BINANCE_LIVE_API_KEY", "--from-env", "A", "--from-env", "B"],
            2,
            "a repeated --from-env: last wins",
        ),
        pin_code(
            &["secrets", "account", "deactivate", "--id", "7", "--id", "8"],
            2,
            "a repeated --id: last wins",
        ),
        pin_code(
            &["backend", "setup", "--addr", "127.0.0.1:1", "--addr", "127.0.0.1:7879"],
            2,
            "a repeated --addr: last wins",
        ),
        pin_code(
            &["mcp", "--addr", "127.0.0.1:1", "--addr", "127.0.0.1:7878"],
            2,
            "a repeated --addr: last wins",
        ),
        pin_code(
            &["mcp", "--profile", "full", "--profile", "read-only"],
            2,
            "a repeated --profile: last wins",
        ),
        pin_code(
            &["report", "--seed", "1", "--seed", "2", "--node", "127.0.0.1:1"],
            2,
            "repeated --seed: last wins",
        ),
        pin_code(
            &[
                "research",
                "study",
                "--study",
                "cohort",
                "--recipe",
                "recipe.toml",
                "--from",
                "a",
                "--to",
                "b",
                "--json=1",
            ],
            2,
            "ACCIDENTAL: study's --json ignores an inline value (no no_value call)",
        ),
        pin_code(
            &[
                "research",
                "study",
                "--study",
                "x",
                "--study",
                "cohort",
                "--recipe",
                "recipe.toml",
                "--from",
                "a",
                "--to",
                "b",
            ],
            2,
            "a repeated --study: last wins",
        ),
        pin_code(&["init", "--dir", "a", "--dir", "scaffold"], 2, "a repeated --dir: last wins"),
        pin_code(&["init", "--json", "--json"], 2, "a repeated switch"),
        pin_code(
            &["indicators", "--name", "rsi", "--name", "sma"],
            2,
            "a repeated --name: last wins",
        ),
        pin_code(&["surface", "--out", "a", "--out", "dist"], 2, "a repeated --out: last wins"),
    ]);
}
