//! The operator, deploy and documentation command lines of `config`, `secrets`, `account`,
//! `backend`, `gatewayhub`, `report`, `research`, `init`, `indicators`, `surface` and the
//! top-level help/version tokens, pinned as ACCEPTED (decision 0155).
//!
//! Written against the hand-rolled `Flags` parsers and run UNCHANGED against the `clap` ones: the
//! only question each line asks is whether the argument reader let it through (`common::accepted`
//! explains the sandbox, and why "not exit 2" is that question). A sandbox has an EMPTY settings
//! directory, so most lines go on to a clean `1` ("no store") or `3` (a refused loopback dial) AFTER
//! the reader accepted them; that is fine, and it is why each argv here is also chosen to be fully
//! valid, so that no post-parse refusal answers `2`. The lines that are REFUSED on purpose (the
//! retired top-level verbs, a removed flag, a value on argv, a spelling stage 5 renamed or moved —
//! `secrets list`, `secrets accounts`, `access list`, ..) are pinned as refused so a rewrite keeps
//! refusing them, and `config` with no or an unknown verb is pinned at `1`, not `2`.

use crate::common;

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
/// unknown verb and a bare `vike-cli` are usage errors, and the removed and renamed spellings are
/// refused by name. `config` with no verb or an unknown verb is `1`, not `2` (the old dispatcher's
/// rung).
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
        pin_code(&["mcp"], 2, "decision 0252: the server is `vike-mcp`, with no alias here"),
        pin_code(&["mcp", "--help"], 2, "decision 0252: unknown as any other word"),
        pin_code(&["--json"], 2, "dispatch: a flag where the verb belongs is an unknown command"),
        pin_code(&["secrets"], 2, "secrets grammar: a subcommand is required"),
        pin_code(
            &["secrets", "template"],
            2,
            "TEMPLATE_REMOVED: refused by name (cli_mcp_smoke.sh)",
        ),
        pin_code(&["secrets", "ls", "--file", "x"], 2, "FILE_FLAG_REMOVED: refused by name"),
        // Stage 5 (the owner's ruling 2 moved the account table to `account`): every retired
        // spelling is a hidden word refused by name, naming what replaced it
        // (`crates/vike-cli/src/refusals.rs`'s `RetiredSpelling`); the word disables its own help,
        // so `--help` after it is refused too.
        pin_code(&["secrets", "list"], 2, "SecretsList: refused naming `secrets ls`"),
        pin_code(&["secrets", "list", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(
            &["secrets", "delete", "BINANCE_LIVE_API_KEY", "--confirm", "BINANCE_LIVE_API_KEY"],
            2,
            "SecretsDelete: refused naming `secrets rm`, quoting nothing",
        ),
        pin_code(&["secrets", "delete", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(&["secrets", "accounts"], 2, "SecretsAccounts: refused naming `account ls`"),
        pin_code(&["secrets", "accounts", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(
            &["secrets", "account", "add", "--venue", "binance", "--tier", "demo", "--no-label"],
            2,
            "SecretsAccount: refused naming `account add`",
        ),
        pin_code(
            &["secrets", "account", "remove", "--id", "7", "--confirm", "7"],
            2,
            "SecretsAccount: refused naming `account rm`",
        ),
        pin_code(&["secrets", "account", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(&["secrets", "account", "add", "--help"], 2, "everything after it is ignored"),
        pin_code(
            &["secrets", "set-book", "--id", "7", "--venue-account-id", "1234567"],
            2,
            "SecretsSetBook: refused naming `account set-book`",
        ),
        pin_code(&["secrets", "set-book", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(&["secrets", "confirm"], 2, "SecretsConfirm: refused naming `account confirm`"),
        pin_code(&["secrets", "confirm", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(
            &["secrets", "set", "--account", "7", "API_KEY"],
            2,
            "SecretsSetAccountFlag: refused naming `secrets set --id`, quoting nothing",
        ),
        pin_code(
            &["secrets", "set", "--account=7", "API_KEY", "--from-env=SECRET_VAR"],
            2,
            "the inline `--account=7` is refused the same way",
        ),
        pin_code(&["secrets", "ls", "--account", "7"], 2, "--account is refused on every sub-verb"),
        pin_code(&["account", "ls", "--account", "7"], 2, "...and on every `account` verb"),
        pin_code(
            &["account", "remove", "--id", "7", "--confirm", "7"],
            2,
            "AccountRemove: refused naming `account rm`",
        ),
        pin_code(&["account", "remove", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(&["access", "list"], 2, "AccessList: refused naming `access ls`"),
        pin_code(&["access", "list", "--help"], 2, "a retired word answers 2 for --help"),
        pin_code(&["account"], 2, "account grammar: a subcommand is required"),
        pin_code(
            &["account", "--venue", "binance", "--tier", "Demo", "--no-label", "add"],
            2,
            "the ACTION is a sub-verb now, no longer a positional: a flag before it is unknown",
        ),
        pin_code(
            &["secrets", "set", "BINANCE_LIVE_API_KEY", "notavalue"],
            2,
            "ARGV_VALUE_REFUSAL: a value on argv is refused",
        ),
        pin_code(&["backend"], 2, "node/mod.rs parse: a subcommand is required"),
        pin_code(&["datahub"], 2, "decision 0288: the `datahub` verb is retired, an unknown verb"),
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
        pin_code(&["config", "recorder"], 2, "RetiredConfigSpelling: `profile ls recorder`"),
        pin_code(&["config", "activate", "--help"], 2, "RetiredConfigSpelling, --help too"),
        pin_code(
            &["config", "deactivate", "run"],
            2,
            "RetiredConfigSpelling: `profile deactivate`",
        ),
        pin_code(&["config", "delete-profile", "run", "a", "--yes"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "bootstrap-daemon", "x"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "bootstrap-recorder", "x"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "bootstrap-run", "x"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "set-mount-param", "--help"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "set-mount-max-leverage", "--help"], 2, "RetiredConfigSpelling"),
        pin_code(&["config", "delete-operator-instrument", "x"], 2, "RetiredConfigSpelling"),
        pin_code(
            &["config", "rename-symbol", "--account", "4", "--from", "A", "--to", "B"],
            2,
            "RetiredConfigSpelling: `--id`",
        ),
        pin_code(
            &["config", "profile", "create", "daemon", "x", "--strategy-file", "a.rs"],
            2,
            "RetiredConfigSpelling: `--strategy`",
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
        pin_code(
            &["config", "profile", "ls", "recorder", "--help"],
            0,
            "config profile ls recorder",
        ),
        pin_code(&["config", "profile", "activate", "--help"], 0, "config profile activate"),
        pin_code(&["config", "profile", "activate", "-h"], 0, "config profile activate, short"),
        pin_code(&["config", "profile", "deactivate", "--help"], 0, "config profile deactivate"),
        pin_code(&["config", "profile", "rm", "--help"], 0, "config profile rm"),
        pin_code(
            &["config", "profile", "create", "daemon", "--help"],
            0,
            "config profile create daemon",
        ),
        pin_code(
            &["config", "profile", "create", "recorder", "--help"],
            0,
            "config profile create recorder",
        ),
        pin_code(&["config", "profile", "create", "run", "--help"], 0, "config profile create run"),
        pin_code(&["config", "profile", "--help"], 0, "config profile: the group"),
        pin_code(&["config", "profile", "create", "--help"], 0, "config profile create"),
        pin_code(&["config", "profile", "mount", "--help"], 0, "config profile mount"),
        pin_code(&["config", "profile", "mount", "set-param", "--help"], 0, "mount set-param"),
        pin_code(&["config", "profile", "mount", "set-max-leverage", "-h"], 0, "set-max-leverage"),
        pin_code(&["config", "operator-instrument", "rm", "--help"], 0, "operator-instrument rm"),
        pin_code(&["secrets", "--help"], 0, "secrets grammar: the roster"),
        pin_code(&["secrets", "-h"], 0, "secrets grammar: short"),
        pin_code(&["secrets", "help"], 0, "secrets grammar: the word form"),
        pin_code(&["secrets", "ls", "--help"], 0, "secrets ls"),
        pin_code(&["secrets", "path", "-h"], 0, "secrets path"),
        pin_code(
            &["secrets", "set", "--help"],
            0,
            "secrets set (the help arm precedes the KEY arm)",
        ),
        pin_code(&["secrets", "set", "-h"], 0, "secrets set, short"),
        pin_code(&["secrets", "rm", "--help"], 0, "secrets rm"),
        pin_code(&["secrets", "init", "--help"], 0, "secrets init"),
        pin_code(&["secrets", "copy-hub-keys", "--help"], 0, "secrets copy-hub-keys"),
        pin_code(&["secrets", "ibc-start", "--help"], 0, "docs/ops: `secrets ibc-start --help`"),
        pin_code(&["secrets", "ibkr-cp-login", "--help"], 0, "secrets ibkr-cp-login"),
        pin_code(&["account", "--help"], 0, "account grammar: the roster"),
        pin_code(&["account", "-h"], 0, "account grammar: short"),
        pin_code(&["account", "help"], 0, "account grammar: the word form"),
        pin_code(&["account", "ls", "--help"], 0, "account ls"),
        pin_code(&["account", "add", "--help"], 0, "account add"),
        pin_code(&["account", "rename", "--help"], 0, "account rename"),
        pin_code(&["account", "set-tier", "--help"], 0, "account set-tier"),
        pin_code(&["account", "set-exposure", "--help"], 0, "account set-exposure"),
        pin_code(&["account", "set-leverage", "--help"], 0, "account set-leverage"),
        pin_code(&["account", "set-symbol", "--help"], 0, "account set-symbol"),
        pin_code(&["account", "set-fact", "--help"], 0, "account set-fact"),
        pin_code(&["account", "deactivate", "--help"], 0, "account deactivate"),
        pin_code(&["account", "activate", "--help"], 0, "account activate"),
        pin_code(&["account", "rm", "--help"], 0, "account rm"),
        pin_code(&["account", "copy-credentials", "--help"], 0, "account copy-credentials"),
        pin_code(&["account", "set-book", "--help"], 0, "account set-book"),
        pin_code(&["account", "confirm", "--help"], 0, "account confirm"),
        pin_code(&["access", "ls", "--help"], 0, "access ls, the spelling `access list` became"),
        pin_code(&["backend", "--help"], 0, "node/mod.rs: skills/connect-to-a-node.md"),
        pin_code(&["backend", "-h"], 0, "backend, short"),
        pin_code(&["backend", "help"], 0, "backend, the word form"),
        pin_code(&["backend", "mint-missing", "--help"], 0, "backend mint-missing"),
        pin_code(&["backend", "probe-copy", "--help"], 0, "backend probe-copy"),
        pin_code(&["backend", "connect", "--help"], 0, "backend connect, no HOST needed for help"),
        pin_code(&["backend", "status", "--help"], 0, "backend status"),
        pin_code(&["backend", "disconnect", "--help"], 0, "backend disconnect"),
        pin_code(&["backend", "ping", "--help"], 0, "backend ping"),
        pin_code(&["gatewayhub", "--help"], 0, "gatewayhub"),
        pin_code(&["gatewayhub", "-h"], 0, "gatewayhub, short"),
        pin_code(&["gatewayhub", "help"], 0, "gatewayhub, the word form"),
        pin_code(&["gatewayhub", "setup", "--help"], 0, "gatewayhub setup"),
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

/// `config show`, `config check`, `config profile ls recorder`: the read verbs.
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
        // recorder
        pin(
            &["config", "profile", "ls", "recorder"],
            "docs/ops/recorder-deploy.md: what the store holds",
        ),
        pin(&["config", "profile", "ls", "recorder", "--name", "default"], "profile/ls.rs: --name"),
        pin(
            &["config", "profile", "ls", "recorder", "--name=default"],
            "profile/ls.rs: inline --name",
        ),
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
            "docs/ops: the operator's door",
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
        // The gateway ADDRESSES are `venue_url` rows since decision 0265 (`config url set`).
        pin(
            &[
                "config",
                "url",
                "set",
                "ibkr",
                "live",
                "trade",
                "socket",
                "127.0.0.1:4101",
                "--product",
                "gateway",
            ],
            "docs/ops: the live port",
        ),
        pin(
            &[
                "config",
                "url",
                "set",
                "ibkr",
                "demo",
                "trade",
                "rest",
                "--product",
                "cpapi",
                "https://127.0.0.1:5000",
            ],
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

/// `config profile activate`, `config profile deactivate`: the profile-selection verbs.
#[test]
fn config_activate_deactivate() {
    assert_accepted(&[
        pin(
            &["config", "profile", "activate", "daemon", "default"],
            "docs/ops: `config profile activate daemon <name>`",
        ),
        pin(
            &["config", "profile", "activate", "run", "run-live", "--dry-run"],
            "activate.rs: run + --dry-run (refused after the parse: the kind is retired, 0297)",
        ),
        pin(
            &["config", "profile", "activate", "recorder", "default"],
            "docs/ops/recorder-deploy.md",
        ),
        pin(
            &["config", "profile", "activate", "--dry-run", "daemon", "default"],
            "flags interleaved before the positionals",
        ),
        pin(&["config", "profile", "deactivate", "run"], "activate.rs DEACTIVATE_USAGE"),
        pin(
            &["config", "profile", "deactivate", "daemon", "--dry-run"],
            "deactivate with --dry-run",
        ),
        pin(&["config", "profile", "deactivate", "--dry-run", "recorder"], "flag before the kind"),
    ]);
}

/// `config profile rm <kind> <name>`: `--yes` deletes, `--dry-run` asks the verdict, and
/// neither is a usage refusal. The sandbox's empty store holds no profile, so a dry run of any
/// name is refused on the usage rung AFTER the reader let it through.
#[test]
fn config_delete_profile() {
    assert_accepted(&[
        pin(
            &["config", "profile", "rm", "daemon", "default", "--yes"],
            "docs/ops/tradehub-container.md: the daemon profile that took the recorder's name",
        ),
        pin(&["config", "profile", "rm", "--yes", "recorder", "old"], "flag before the kind"),
        pin_code(
            &["config", "profile", "rm", "run", "old", "--dry-run"],
            2,
            "profile/rm.rs: the store holds no `run` profile called `old`",
        ),
        pin_code(&["config", "profile", "rm", "daemon", "default"], 2, "--yes is required"),
        pin_code(&["config", "profile", "rm", "daemon", "--yes"], 2, "the NAME is required"),
    ]);
}

/// `config profile create daemon <name> --venue .. --asset-class .. (--symbol | --token-id)
/// --strategy .. ..` (the file is REQUIRED: there is no default strategy).
#[test]
fn config_bootstrap_daemon() {
    assert_accepted(&[
        pin(
            &[
                "config",
                "profile",
                "create",
                "daemon",
                "default",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "default",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "binance-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "bybit-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "okx-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "aster-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "deribit-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "alpaca-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "oanda-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "oanda-data",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "ctrader-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "ig-live",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "cli-mcp-smoke",
                "--strategy",
                "nothing.rhai",
                "--venue",
                "polymarket",
                "--asset-class",
                "PredictionMarket",
                "--token-id",
                "123456",
            ],
            "scripts/cli_mcp_smoke.sh and vike-agent-evaluation: --token-id",
        ),
        pin_code(
            &[
                "config",
                "profile",
                "create",
                "daemon",
                "all-flags",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "inline",
                "--strategy=nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "daemon",
                "neg",
                "--strategy",
                "nothing.rhai",
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

/// `config profile create recorder <name> --store .. --venue .. (--family | --symbols) ..`.
#[test]
fn config_bootstrap_recorder() {
    assert_accepted(&[
        pin(
            &[
                "config",
                "profile",
                "create",
                "recorder",
                "flip-dry-run",
                "--store",
                "history",
                "--venue",
                "binance",
                "--family",
                "spot",
            ],
            "docs/ops/recorder-deploy.md: `profile create recorder <name> --store <root> ..`",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "recorder",
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
                "profile",
                "create",
                "recorder",
                "syms",
                "--store",
                "history",
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
                "profile",
                "create",
                "recorder",
                "all-flags",
                "--store",
                "history",
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
                "profile",
                "create",
                "recorder",
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
                "profile",
                "create",
                "recorder",
                "--store",
                "history",
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

/// `config profile create run <name> [--<dotted.path> <value> ..]` (decision 0156 item 7: the run profile
/// states no `mode`, so the line each of these once began with `--mode <word>` no longer does).
#[test]
fn config_bootstrap_run() {
    assert_accepted(&[
        pin(
            &["config", "profile", "create", "run", "run-paper", "--dry-run"],
            "the minimal line, rehearsed (no stored daemon profile in the sandbox: 1, decision 0297)",
        ),
        pin(
            &["config", "profile", "create", "run", "run-paper"],
            "the same, applied (no store: 1)",
        ),
        // REFUSED since decision 0156 item 7: a run profile has no `mode` (it was `--mode
        // <backtest|paper|live>`, REQUIRED), whatever its word.
        pin_code(
            &["config", "profile", "create", "run", "run-paper", "--mode", "paper"],
            2,
            "0156: `--mode` is an unknown option, the profile states no tier",
        ),
        pin_code(
            &["config", "profile", "create", "run", "run-bt", "--mode", "backtest"],
            2,
            "0156: `--mode backtest` is refused too",
        ),
        pin_code(
            &["config", "profile", "create", "run", "run-live", "--mode=live", "--dry-run"],
            2,
            "0156: the inline `--mode=live` spelling is refused as well",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
                "tradehub",
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
                "--risk.required_free_buying_power_percent",
                "0.05",
                "--risk.block_reduce_only_overshoot",
                "true",
                "--dry-run",
            ],
            "the documented live run-profile line, rehearsed",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
                "tradehub",
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
                "--risk.required_free_buying_power_percent",
                "0.05",
                "--risk.block_reduce_only_overshoot",
                "true",
            ],
            "the documented live run-profile line, applied",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
                "run-live",
                "--risk.max_notional_per_order",
                "5000",
                "--risk.max_total_exposure",
                "25000",
            ],
            "profile/create/run.rs: the two ceilings a demo/live mount needs (a body without them is only WARNED about)",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
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
                "profile",
                "create",
                "run",
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
                "profile",
                "create",
                "run",
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
                "--risk.required_free_buying_power_percent",
                "0.05",
                "--dry-run",
            ],
            "every [risk] key once (ProfileRisk::keys)",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
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
                "--guards.margin_call.maintenance_margin_requirement",
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
                "profile",
                "create",
                "run",
                "words",
                "--guards.initial_trading_state",
                "reducing",
                "--dry-run",
            ],
            "a closed-vocabulary word",
        ),
        pin(
            &[
                "config",
                "profile",
                "create",
                "run",
                "neg",
                "--risk.max_leverage",
                "-1",
                "--dry-run",
            ],
            "a negative number is a value to a dotted flag (the daemon refuses it later)",
        ),
    ]);
}

/// `secrets ls | path | init`, the reads and the one creator.
#[test]
fn secrets_reads_and_init() {
    assert_accepted(&[
        pin(&["secrets", "ls"], "README.md; docs/ops: key NAMES only"),
        pin(&["secrets", "ls", "--json"], "docs/ops; cli_mcp_smoke.sh: secrets ls --json"),
        pin(&["secrets", "path"], "README.md; every ops page: where the store is"),
        pin(
            &["secrets", "init"],
            "deploy/docker/vikectl's pre-flight; README: create the empty store",
        ),
        pin(&["secrets", "init", "--dry-run"], "docs/ops: rehearse init"),
    ]);
}

/// `secrets set` (FIELD or NAME; value on stdin or `--from-env`, never argv) and `secrets rm KEY`.
#[test]
fn secrets_set_and_delete() {
    assert_accepted(&[
        // Decision 0156 item 6: an account's credential is a FIELD of an EXISTING account row.
        pin(
            &["secrets", "set", "--id", "7", "API_KEY"],
            "secrets.rs USAGE: the value on stdin, aimed at an account row",
        ),
        pin(
            &["secrets", "set", "--id", "7", "API_SECRET", "--from-env", "SECRET_VAR"],
            "secrets.rs USAGE: the CI/deploy form",
        ),
        pin(
            &["secrets", "set", "--id=7", "API_KEY", "--from-env=SECRET_VAR"],
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
            &["secrets", "set", "--id", "7", "--venue", "binance", "API_KEY"],
            2,
            "0156: an id IS an account; a venue beside it is refused",
        ),
        pin_code(
            &["secrets", "set", "--label", "HEDGE", "API_KEY"],
            2,
            "0156: a label names an account of a venue, so it needs --venue",
        ),
        pin_code(&["secrets", "set", "--id", "seven", "API_KEY"], 2, "0156: --id takes a row id"),
        pin_code(&["secrets", "ls", "--id", "7"], 2, "0156: --id names no row on `secrets ls`"),
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
        // rm
        pin(
            &["secrets", "rm", "BINANCE_LIVE_API_KEY", "--confirm", "BINANCE_LIVE_API_KEY"],
            "secrets.rs USAGE: the typed confirm",
        ),
        pin(
            &[
                "secrets",
                "rm",
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
                "rm",
                "BINANCE_LIVE_API_KEY",
                "--dry-run",
                "--confirm",
                "BINANCE_LIVE_API_KEY",
            ],
            "secrets.rs USAGE: the documented order",
        ),
        pin(
            &["secrets", "rm", "BINANCE_LIVE_API_KEY", "--confirm=BINANCE_LIVE_API_KEY"],
            "the inline spelling of --confirm",
        ),
        pin(
            &[
                "secrets",
                "rm",
                "--dry-run",
                "--confirm",
                "BINANCE_LIVE_API_KEY",
                "BINANCE_LIVE_API_KEY",
            ],
            "the KEY positional last, after the flags",
        ),
    ]);
}

/// The `account` group: `ls`, `confirm`, `set-book` and the row verbs (all go to the account
/// table).
#[test]
fn account_table_verbs() {
    assert_accepted(&[
        pin(&["account", "ls"], "scripts/skills/arm-a-venue.md: the account table"),
        pin(&["account", "confirm"], "secrets.rs USAGE: fold the parked confirmations"),
        pin(&["account", "confirm", "--dry-run"], "secrets.rs USAGE: rehearse the fold"),
        // set-book
        pin(
            &["account", "set-book", "--id", "7", "--venue-account-id", "1234567"],
            "grammar.rs set-book message: the documented line (1234567 is made up)",
        ),
        pin(
            &["account", "set-book", "--id", "7", "--venue-account-id", "1234567", "--dry-run"],
            "set-book --dry-run echoes the row",
        ),
        pin(
            &["account", "set-book", "--id", "7", "--venue-account-id", "1234567", "--replace"],
            "set-book --replace",
        ),
        pin(&["account", "set-book", "--id", "7", "--clear"], "set-book --clear"),
        pin(
            &["account", "set-book", "--id", "7", "--clear", "--dry-run"],
            "set-book --clear rehearsed",
        ),
        pin(
            &["account", "set-book", "--id=7", "--venue-account-id=1234567"],
            "inline spellings of --id and --venue-account-id",
        ),
        pin(
            &["account", "set-book", "--venue-account-id", "1234567", "--id", "7"],
            "the two flags in the other order",
        ),
        // the row verbs
        pin(
            &["account", "add", "--venue", "binance", "--tier", "demo", "--label", "HEDGE"],
            "scripts/skills/arm-a-venue.md: account add (with a label)",
        ),
        pin(
            &["account", "add", "--venue", "binance", "--tier", "demo", "--no-label"],
            "account add --no-label",
        ),
        pin(
            &[
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
            &["account", "add", "--venue=binance", "--tier=LIVE", "--label=HEDGE"],
            "inline forms; --tier is case-insensitive",
        ),
        pin(
            &["account", "add", "--no-label", "--tier", "Demo", "--venue", "binance"],
            "the flags in another order; a mixed-case tier",
        ),
        pin(&["account", "rename", "--id", "7", "--label", "SWISS"], "account.rs usage line"),
        pin(&["account", "rename", "--id", "7", "--no-label"], "account rename --no-label"),
        pin(
            &["account", "rename", "--id", "7", "--label", "SWISS", "--dry-run"],
            "account rename rehearsed",
        ),
        pin(
            &["account", "set-tier", "--id", "7", "--tier", "demo"],
            "scripts/skills/arm-a-venue.md; docs/ops",
        ),
        pin(
            &["account", "set-tier", "--id", "7", "--tier", "live", "--dry-run"],
            "account set-tier rehearsed",
        ),
        pin(
            &["account", "set-tier", "--tier", "paper", "--id", "7"],
            "the flags in the other order",
        ),
        pin(
            &["account", "set-exposure", "--id", "7", "--max-exposure", "5000"],
            "account.rs usage line",
        ),
        pin(
            &["account", "set-exposure", "--id", "7", "--max-exposure", "none"],
            "set-exposure: `none` clears the ceiling",
        ),
        pin(
            &["account", "set-exposure", "--id", "7", "--max-exposure", "-5"],
            "a single-dash value is accepted by the reader (refused later by the store)",
        ),
        pin(
            &["account", "set-exposure", "--max-exposure=2500.5", "--id=7", "--dry-run"],
            "inline forms and a fractional figure",
        ),
        pin(
            &["account", "set-leverage", "--id", "11", "--leverage", "10"],
            "docs/ops/trading-any-symbol.md, the leverage section",
        ),
        pin(
            &["account", "set-leverage", "--id", "11", "--leverage", "none"],
            "set-leverage: `none` clears the leverage",
        ),
        pin(&["account", "activate", "--id", "7"], "scripts/skills/arm-a-venue.md"),
        pin(&["account", "activate", "--id", "7", "--dry-run"], "activate rehearsed"),
        pin(&["account", "deactivate", "--id", "7"], "docs/ops, skills: deactivate --id <N>"),
        pin(&["account", "deactivate", "--id", "7", "--dry-run"], "deactivate rehearsed"),
        pin(
            &["account", "rm", "--id", "7", "--confirm", "7"],
            "account rm with its typed confirm (the id)",
        ),
        pin(&["account", "rm", "--id", "7", "--confirm", "7", "--dry-run"], "account rm rehearsed"),
        pin(&["account", "rm", "--confirm=7", "--id=7"], "inline forms"),
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
                "gatewayhub",
                "--replace",
                "--dry-run",
            ],
            "every copy-hub-keys flag at once",
        ),
        pin_code(
            &[
                "secrets",
                "copy-hub-keys",
                "--from-settings-dir",
                "old/settings",
                "--only",
                "tradehub",
            ],
            2,
            "decision 0288: the tradehub verifies no hub key of its own",
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

/// `backend mint-missing | connect | status | disconnect | ping` and `gatewayhub setup`; the verbs
/// decision 0288 retired (`backend setup | admin-key`, `datahub setup | control-key`) are refused.
#[test]
fn backend_and_datahub() {
    assert_accepted(&[
        pin(&["backend", "mint-missing"], "deploy/docker/entrypoint.sh: at every server start"),
        pin(
            &[
                "backend",
                "probe-copy",
                "--into",
                "probe/settings",
                "--drop-key",
                "VIKE_STRATEGY_BUILDER_KEY",
                "--paper-accounts",
                "--numeric-key",
                "VIKE_TELEGRAM_ALLOWED_CHAT_IDS",
            ],
            "deploy/docker/vikectl's probe_copy: a pre-flight's settings copy, no sqlite3",
        ),
        pin_code(&["backend", "setup"], 2, "decision 0288: retired, `mint-missing` mints"),
        pin_code(&["backend", "admin-key"], 2, "decision 0288: no hub reads an admin key"),
        pin_code(&["datahub", "setup"], 2, "decision 0288: the `datahub` verb is retired"),
        pin_code(&["datahub", "control-key"], 2, "decision 0288: no key is handed to a pipe"),
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
            &["backend", "connect", "the CI box", "--server-project", "/srv/vike", "--no-tunnel"],
            "connect-to-a-node.md: `connect <host> --server-project DIR`",
        ),
        pin(&["backend", "connect", "--invitation"], "a guest's paste, the line on stdin"),
        pin_code(&["backend", "client-keys"], 2, "decision 0288: no shared key is handed over"),
        pin_code(
            &["backend", "connect", "the CI box", "--manual"],
            2,
            "decision 0288: --manual is gone",
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
        // gatewayhub
        pin(&["gatewayhub", "setup"], "decision 0164 verdict 6: mint the gatewayhub pair"),
        pin(
            &["gatewayhub", "setup", "--rotate"],
            "decision 0164 verdict 6: `gatewayhub setup --rotate`",
        ),
    ]);
}

/// `report` (a stored run, the schema; the two retired spellings) and `research study`.
#[test]
fn report_and_research_study() {
    assert_accepted(&[
        pin_code(&["report", "--schema"], 0, "report.rs USAGE: no run, no node, no key"),
        pin(&["report", "run:@last"], "report.rs USAGE: the @last selector (no run: 1)"),
        pin(&["report", "run:@last:backtest"], "report.rs USAGE: @last:<kind>"),
        pin(&["report", "run:20260101-abc-1"], "report.rs USAGE: a run id or unique prefix"),
        pin(&["report", "run:baseline/momentum"], "a bare mark spelled with `/`"),
        pin(&["report", "run:@baseline"], "report.rs USAGE: a @mark"),
        pin(&["report", "run:@last", "--json"], "report.rs USAGE: --json"),
        pin(
            &["report", "run:@last", "--breakdown", "day"],
            "docs: report run:<run> --breakdown day",
        ),
        pin(&["report", "run:@last", "--breakdown", "month", "--json"], "report.rs USAGE: month"),
        pin(&["report", "run:@last", "--breakdown=day"], "inline --breakdown"),
        pin(&["report", "--breakdown", "day", "run:@last"], "the selector after the flag"),
        pin(&["report", "--json", "run:@last", "--breakdown", "day"], "flags around the selector"),
        pin(&["report", "run:run=with-equals"], "a selector containing `=` is put back whole"),
        pin(&["report", "run:@last", "--html", "out.html"], "report.rs USAGE: a run's HTML page"),
        pin(
            &["report", "run:@last:forward", "--cash", "5000", "--without-foreign"],
            "a forward run takes the account knobs (a stored one refuses them once read)",
        ),
        // The ACCOUNT source (2026-11-18). A sandbox holds no `config.node_addr`, so each accepted line
        // ends in 1 before it dials anything.
        pin(&["report", "account:binance/MAIN"], "docs: report account:<venue>/<LABEL>"),
        pin(
            &["report", "account:binance"],
            "report.rs USAGE: <venue> alone, the unlabelled account",
        ),
        pin(
            &["report", "account:binance/DEFAULT", "--from", "2026-11-01", "--to", "2026-11-18"],
            "report.rs USAGE: the window, as dates",
        ),
        pin(
            &[
                "report",
                "account:binance/MAIN",
                "--cash",
                "5000",
                "--periods-per-year",
                "365",
                "--mark-to-market",
                "--interval",
                "5m",
                "--without-foreign",
                "--name",
                "mine",
                "--html",
                "out.html",
            ],
            "every account knob",
        ),
        pin(&["report", "account:binance/MAIN", "--json"], "report.rs USAGE: --json"),
        // `--compare <A> <B>` (2026-11-18): a sandbox holds no run, so each accepted line ends in 1.
        pin_code(&["report", "--schema", "--compare"], 0, "the comparison document's shape"),
        pin(
            &["report", "--compare", "run:@last", "account:binance/MAIN"],
            "docs: report --compare run:<backtest> account:<venue>/<LABEL>",
        ),
        pin(
            &["report", "--compare", "run:@last", "run:@last:forward", "--json"],
            "docs: report --compare run:<backtest> run:<forward-run>",
        ),
        pin(
            &[
                "report",
                "--compare",
                "account:binance/MAIN",
                "account:bybit/DEMO",
                "--from",
                "2026-11-01",
                "--to",
                "2026-11-18",
                "--html",
                "c.html",
            ],
            "two accounts over one window",
        ),
        // RETIRED 2026-11-18 (owner): a bare selector and the live-journal `--node` are refused on the
        // usage rung, naming the spelling that replaced each, in every form they used to be accepted in.
        pin_code(&["report", "@last"], 2, "report.rs: a bare selector is retired"),
        pin_code(&["report", "-1"], 2, "a single-dash positional is a (retired) bare selector"),
        pin_code(&["report", "help"], 2, "`help` is a (retired) bare selector, not a help word"),
        pin_code(
            &["report", "--node", "127.0.0.1:1"],
            2,
            "report.rs: the live-journal door is retired",
        ),
        pin_code(&["report", "--node=127.0.0.1:1", "--json"], 2, "inline --node"),
        pin_code(
            &["report", "--node", "127.0.0.1:1", "--cash=10000", "--periods-per-year=252"],
            2,
            "every live flag beside the retired door",
        ),
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
                "profile",
                "create",
                "daemon",
                "last-wins",
                "--strategy",
                "nothing.rhai",
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
                "profile",
                "create",
                "recorder",
                "lw",
                "--store",
                "a",
                "--store",
                "history",
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
            &["account", "deactivate", "--id", "7", "--id", "8"],
            2,
            "a repeated --id: last wins",
        ),
        pin_code(
            &["backend", "ping", "--addr", "127.0.0.1:1", "--addr", "127.0.0.1:7879"],
            2,
            "a repeated --addr: last wins",
        ),
        pin_code(
            &["report", "--cash", "1", "--cash", "2", "--node", "127.0.0.1:1"],
            2,
            "repeated --cash: last wins",
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
