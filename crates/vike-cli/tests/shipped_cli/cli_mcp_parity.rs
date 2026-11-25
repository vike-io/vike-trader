//! CLI <-> MCP PARITY: one operation has one shape, whichever door an operator or an agent uses.
//!
//! `vike-cli` and `vike-mcp` are two doors over one client and may not name each other, dev edges
//! included (`docs/decisions/0252-the-cli-and-the-mcp-server-are-two-doors-over-one-client-and-every-hub-has-its-own-client.md`).
//! So this compares what each door PUBLISHES, the way its reader sees it:
//!
//! * the CLI's `cli.json`, rendered in-process by `vike_cli::surface::rendered_files`: its `commands`
//!   are the `clap` tree, its `flags` and `rosters` the `backtest` plane's defaults, words and which
//!   sub-verb takes which flag (`applies_to`);
//! * the MCP server's `tools/list` and its refusals, from the goldens `tools_list.full.json` and
//!   `tools.errors.json` (`crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot.rs` holds them equal to
//!   what the server serves; `xtask/src/ci/tables/gate_crates.rs`'s `MCP_INSTRUCTIONS_INPUTS` selects
//!   this crate when they change);
//! * the CLI's refusals, from the shipped binary.
//!
//! The comparison is a TABLE, written out: [`OPERATIONS`] (operation -> CLI command -> MCP tool) and,
//! per operation, its [`Pair`]s (field -> CLI flag -> MCP argument). It checks three things:
//!
//! 1. the same SET of operations: every MCP tool and every CLI command is an operation, or a declared
//!    one-door row with its reason ([`MCP_ONLY_TOOLS`], [`CLI_ONLY_COMMANDS`], [`CLI_ONLY_SCOPES`]);
//! 2. the same FIELDS: every MCP argument and every CLI flag of an operation is a pair or a declared
//!    one-door row, and a pair agrees on its word (`--reduce-only` is `reduce_only`), its kind (a bare
//!    switch is a boolean), whether it is required, its default and its value words ([`Aspect`]);
//! 3. the same REFUSAL, by exact text once each door's own prefix and an argument's spelling
//!    (`--symbol`, `` `symbol` ``) are set aside ([`REFUSALS`]); by id once the refusal ids land.
//!
//! Every difference that exists today is DECLARED with its reason, and a declaration that stops being
//! true fails as stale, so the harness is tightened by DELETING rows: P1 (#1 the MCP argument structs
//! parse the call, #2 and #3 `profile.json` and `cli.json` from the types) and P3 (the naming pass of
//! decision 0252) each delete the rows they settle. P4 #11b deleted its whole table, the decisions both
//! programs made in copies of their own, with its last row: each is now made once below both, in
//! `vike-client` or `vike-model`, and pinned there.
//!
//! shortcut: a default is compared only where a [`Pair`] declares one, and a value word only where it
//! names the typed vocabulary; a door that publishes a default or a word nobody declared is not seen.
//! Declare the field's default or vocabulary when one is added.
//! shortcut: the `data history` verbs share ONE flag set ([`SHARED_FLAG_SETS`]), so their CLI flags are
//! held to the table only where a pair or a row names them; `applies_to` narrows the `backtest` ones.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::common;

/// The published `tools/list` under the `full` scope, and every refusal the goldens pin.
const TOOLS_LIST: &str =
    include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/tools_list.full.json");
const TOOL_ERRORS: &str =
    include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/tools.errors.json");

/// A node no CLI refusal below ever reaches: each one refuses before anything is dialled.
const DEAD: &str = "127.0.0.1:1";

// ---- the table ------------------------------------------------------------------------------------

/// What a pair's two sides can disagree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Aspect {
    /// The CLI's word, its dashes read as underscores, is not the MCP argument's.
    Name,
    /// One side is a bare switch and the other not a boolean, or the reverse.
    Kind,
    /// One door requires it and the other does not.
    Required,
    /// One door publishes the declared default and the other does not.
    Default,
    /// One door publishes a word of the typed vocabulary the other does not, or a word outside it.
    Words,
    /// The same word takes a different thing (a path and the text it holds). Declared, not measured.
    Meaning,
}

/// One field both doors take for one operation.
struct Pair {
    /// A flag's long name (`reduce-only`), a positional's id (`ids`), or ONE SLOT of a positional token
    /// list as `id#slot`, the slot spelled as that positional's help spells it (`tokens#<account>`).
    cli: &'static str,
    /// The MCP argument.
    mcp: &'static str,
    /// The default both doors should publish, in MCP's spelling.
    default: Option<&'static str>,
    /// The typed vocabulary of the field's value words, in the model's spelling.
    words: Option<fn() -> Vec<String>>,
    /// Every aspect on which the two disagree TODAY, with why. A difference not listed here fails, and
    /// so does one listed here that no longer differs.
    differs: &'static [(Aspect, &'static str)],
}

const fn pair(
    cli: &'static str,
    mcp: &'static str,
    differs: &'static [(Aspect, &'static str)],
) -> Pair {
    Pair { cli, mcp, default: None, words: None, differs }
}

/// One operation: the MCP tool, the `vike-cli` command (its words), and the fields they share. A tool
/// may have several commands (`submit_order_list` is `oco`, `oto` and `ladder`) and a command several
/// tools (`backtest run` is a backtest, a sweep and a walk-forward).
struct Operation {
    tool: &'static str,
    command: &'static str,
    pairs: &'static [Pair],
}

fn order_types() -> Vec<String> {
    vike_model::OrderType::KNOWN.iter().map(|kind| kind.as_str().to_owned()).collect()
}
fn lifetimes() -> Vec<String> {
    vike_model::TimeInForce::ALL.iter().map(|tif| tif.as_word().to_owned()).collect()
}
fn peg_anchors() -> Vec<String> {
    vike_model::OrderPegAnchor::ALL.iter().map(|anchor| anchor.as_word().to_owned()).collect()
}
fn rank_metrics() -> Vec<String> {
    vike_model::run_request::flag_vocab::RANK_METRICS.iter().map(|w| (*w).to_owned()).collect()
}
fn search_methods() -> Vec<String> {
    vike_model::run_request::search_methods::SEARCH_METHODS
        .iter()
        .map(|w| (*w).to_owned())
        .collect()
}

const BOOK: &str =
    "the CLI's `<account>` is `<venue>[/<LABEL>]`: ONE token for MCP's `venue` and `account`";
const BOOK_LABEL: &str =
    "the label is the optional half of the CLI's one required `<account>` token";
const SIDE: &str = "the CLI's side is a word (`buy|sell`), MCP's an integer (1 buy / -1 sell)";
const ONE_ID: &str = "the CLI reads the id as a positional list (`clap` requires none) and refuses \
                      anything but one after the parse; MCP names the argument and requires it";
const NEW_VALUE: &str =
    "MCP prefixes what a modify changes with `new_`; on the CLI the verb says it";
const OFFSET: &str = "ONE CLI flag, `--offset`, for two MCP arguments: a trailing-limit's \
                      `limit_offset` and a pegged order's `peg_offset`";
const RELEASE: &str = "the CLI's `--at` family holds an order until a moment; MCP prefixes the same \
                       three `release_`";
const INSTANT: &str =
    "the CLI takes a UTC instant or epoch milliseconds, MCP epoch milliseconds only";
const LIST_PRICE: &str =
    "MCP names an order list's leg prices `*_price`; the CLI's flag names the leg";
const ENTRY: &str = "the CLI's entry limit is the `@<price>` token; MCP names it `entry_price`";
const LEG_IDS: &str = "the CLI repeats `--order-client-id`, one leg's id each; MCP's \
                       `order_client_ids` is one comma-separated string";
const LEGS: &str = "the CLI repeats `--leg <symbol>:<ratio>[@<limit>]` and refuses fewer than two \
                    after the parse; MCP's `legs` is one required array of leg objects";
const PROFILE: &str = "the CLI's `--profile` is a PATH to a base the other flags override, and \
                       optional (flags BUILD a profile); MCP's `profile` is the whole TOML text, required";
const SCRIPT_FILE: &str = "the `backtest` read verbs share one flag set, so `clap` takes `--script` \
                           as optional and the verb refuses its absence; and it is a FILE to read, \
                           where MCP's `script` is the Rhai source itself";
const MOUNT_FILE: &str = "`clap` takes `--strategy` as optional and the verb refuses its absence \
                          after the parse (`crates/vike-cli/src/cmd/trade/strategy.rs`'s \
                          `mount_verb`); MCP requires it";
const ACCOUNT_SOURCE: &str = "the CLI's live source is ONE token, `account:<venue>/<LABEL>`, for MCP's \
                              `venue` and `account`";
const DAY_OR_MS: &str =
    "the CLI takes epoch-ms or a YYYY-MM-DD day (its start; `--to` its end), MCP epoch-ms only";
const HIST_SHARED: &str = "every `data history` verb reads ONE flag set, so `clap` takes it as optional \
                           and `rm` refuses its absence after the parse; MCP requires it";

/// The fields every write carries on both doors, wherever both sides have them.
const DOOR_PAIRS: &[Pair] = &[
    pair("reason", "reason", &[]),
    pair(
        "yes",
        "confirm",
        &[(
            Aspect::Name,
            "one confirmation, two gates: `--yes` skips the human's y/N prompt, `confirm: true` \
             executes only with the preview's token",
        )],
    ),
];

/// The CLI flags that belong to the CLI door itself, on whichever command has them.
const CLI_DOOR: &[(&str, &str)] = &[
    (
        "node",
        "MCP dials the ONE node named at its launch (`vike-mcp --node`, else `config.node_addr`), never one per \
         call",
    ),
    (
        "addr",
        "MCP dials the daemons named at its launch (`--addr`, `--backtest-addr`), never one per call",
    ),
    ("json", "MCP answers JSON, always"),
    (
        "no-wait",
        "MCP's answer is the node's acknowledgement: it never looks for the event after it, so \
         there is nothing to skip",
    ),
];

/// The MCP arguments that belong to the MCP door itself.
const MCP_DOOR: &[(&str, &str)] = &[(
    "preview_token",
    "the preview gate (`crates/vike-mcp/src/preview.rs`): an agent confirms by sending back the token \
     its preview returned; a human confirms at the CLI's y/N prompt",
)];

const OPERATIONS: &[Operation] = &[
    // ---- offline and backtest ----
    Operation {
        tool: "validate_strategy",
        command: "backtest script-check",
        pairs: &[pair(
            "script",
            "script",
            &[(Aspect::Required, SCRIPT_FILE), (Aspect::Meaning, SCRIPT_FILE)],
        )],
    },
    Operation {
        tool: "discover_params",
        command: "backtest params",
        pairs: &[pair(
            "script",
            "script",
            &[(Aspect::Required, SCRIPT_FILE), (Aspect::Meaning, SCRIPT_FILE)],
        )],
    },
    Operation { tool: "list_templates", command: "backtest templates", pairs: &[] },
    Operation {
        tool: "list_indicators",
        command: "indicators",
        pairs: &[pair("name", "name", &[]), pair("category", "category", &[])],
    },
    Operation {
        tool: "run_backtest",
        command: "backtest run",
        pairs: &[
            pair("profile", "profile", &[(Aspect::Required, PROFILE), (Aspect::Meaning, PROFILE)]),
            pair("fresh", "fresh", &[]),
        ],
    },
    Operation {
        tool: "run_sweep",
        command: "backtest run",
        pairs: &[
            pair("profile", "profile", &[(Aspect::Required, PROFILE), (Aspect::Meaning, PROFILE)]),
            pair("fresh", "fresh", &[]),
            Pair {
                default: Some("sharpe"),
                words: Some(rank_metrics),
                ..pair("rank-by", "rank_by", &[])
            },
            Pair {
                default: Some("grid"),
                words: Some(search_methods),
                ..pair("optimizer", "optimizer", &[])
            },
            Pair {
                default: Some("64"),
                ..pair(
                    "trials",
                    "trials",
                    &[(Aspect::Default, "MCP's description does not state tpe's 64 trials")],
                )
            },
            Pair {
                default: Some("0"),
                ..pair(
                    "seed",
                    "seed",
                    &[(
                        Aspect::Default,
                        "MCP's description does not state tpe's seed 0 (genetic requires one)",
                    )],
                )
            },
            Pair {
                default: Some("3"),
                ..pair(
                    "euler-depth",
                    "euler_depth",
                    &[(Aspect::Default, "MCP's description does not state euler's depth 3")],
                )
            },
        ],
    },
    Operation {
        tool: "run_walk_forward",
        command: "backtest run",
        pairs: &[
            pair("profile", "profile", &[(Aspect::Required, PROFILE), (Aspect::Meaning, PROFILE)]),
            pair("fresh", "fresh", &[]),
        ],
    },
    Operation { tool: "list_strategies", command: "backtest strategies", pairs: &[] },
    Operation { tool: "list_series", command: "data history ls", pairs: &[] },
    Operation {
        tool: "delete_series",
        command: "data history rm",
        pairs: &[
            pair("kind", "kind", &[(Aspect::Required, HIST_SHARED)]),
            pair("venue", "venue", &[(Aspect::Required, HIST_SHARED)]),
            pair("symbol", "symbol", &[]),
            pair("group", "group", &[]),
            pair("interval", "interval", &[]),
            pair(
                "produced-by",
                "produced_by",
                &[(
                    Aspect::Required,
                    "MCP requires a provenance assertion on EVERY call; the CLI's is optional for \
                     a fully named series (MCP's description says why)",
                )],
            ),
        ],
    },
    // ---- node reads ----
    Operation { tool: "node_snapshot", command: "trade status", pairs: &[] },
    Operation { tool: "strategy_status", command: "trade strategy ls", pairs: &[] },
    Operation { tool: "health_node", command: "trade health", pairs: &[] },
    Operation {
        tool: "report",
        command: "report",
        pairs: &[
            pair("sources#<venue>", "venue", &[(Aspect::Meaning, ACCOUNT_SOURCE)]),
            pair(
                "sources#<LABEL>",
                "account",
                &[
                    (Aspect::Name, ACCOUNT_SOURCE),
                    (
                        Aspect::Required,
                        "the CLI's help spells `<LABEL>` unbracketed, yet `account:<venue>` alone names \
                         the unlabelled account, as MCP's omitted `account` does",
                    ),
                ],
            ),
            pair("from", "from", &[(Aspect::Meaning, DAY_OR_MS)]),
            pair("to", "to", &[(Aspect::Meaning, DAY_OR_MS)]),
            pair("cash", "cash", &[]),
            Pair { default: Some("365"), ..pair("periods-per-year", "periods_per_year", &[]) },
        ],
    },
    Operation {
        tool: "decode_order_id",
        command: "trade order decode",
        pairs: &[pair(
            "id",
            "order_client_id",
            &[(
                Aspect::Name,
                "the CLI's positional id; `submit` spells the same thing `--order-client-id`",
            )],
        )],
    },
    // ---- node writes ----
    Operation {
        tool: "submit_order",
        command: "trade order submit",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<symbol>", "symbol", &[]),
            pair("tokens#<buy|sell>", "side", &[(Aspect::Name, SIDE)]),
            pair("tokens#<qty>", "qty", &[]),
            pair("tokens#@<price>", "price", &[]),
            Pair {
                default: Some("market"),
                words: Some(order_types),
                ..pair(
                    "type",
                    "order_type",
                    &[
                        (Aspect::Name, "`--type` on the CLI, `order_type` on MCP"),
                        (
                            Aspect::Default,
                            "MCP states `market`; on the CLI no `@<price>` is a market order, and \
                             `--type` names the trigger kinds only",
                        ),
                        (
                            Aspect::Words,
                            "the CLI spells `market` and `limit` by its `@market` / `@<price>` token, \
                             never `--type`",
                        ),
                    ],
                )
            },
            pair(
                "trigger",
                "trigger_price",
                &[(Aspect::Name, "`--trigger` on the CLI, `trigger_price` on MCP")],
            ),
            pair("trail", "trail", &[]),
            pair("trail-percent", "trail_percent", &[]),
            pair("offset", "limit_offset", &[(Aspect::Name, OFFSET)]),
            Pair { default: Some("0"), ..pair("offset", "peg_offset", &[(Aspect::Name, OFFSET)]) },
            Pair { words: Some(peg_anchors), ..pair("peg", "peg", &[]) },
            Pair {
                default: Some("gtc"),
                words: Some(lifetimes),
                ..pair(
                    "time-in-force",
                    "time_in_force",
                    &[(
                        Aspect::Words,
                        "the CLI's maker-only lifetime is its own flag, `--post-only`; MCP's is \
                         the `time_in_force` word `post_only`",
                    )],
                )
            },
            pair(
                "expires",
                "gtd_expiry",
                &[
                    (Aspect::Name, "`--expires` on the CLI, `gtd_expiry` on MCP"),
                    (Aspect::Meaning, INSTANT),
                ],
            ),
            pair(
                "display",
                "display_qty",
                &[(Aspect::Name, "`--display` on the CLI, `display_qty` on MCP")],
            ),
            pair("all-or-none", "all_or_none", &[]),
            pair("sweep-to-fill", "sweep_to_fill", &[]),
            pair(
                "discretion",
                "discretionary_amount",
                &[(Aspect::Name, "`--discretion` on the CLI, `discretionary_amount` on MCP")],
            ),
            pair("adjust-at", "adjust_at", &[]),
            pair("adjust-to", "adjust_to", &[]),
            pair("adjust-stop", "adjust_stop", &[]),
            pair("adjust-limit", "adjust_limit", &[]),
            pair("adjust-trail", "adjust_trail", &[]),
            pair("adjust-trail-percent", "adjust_trail_percent", &[]),
            pair("at", "release_at", &[(Aspect::Name, RELEASE), (Aspect::Meaning, INSTANT)]),
            pair("when-price", "release_when_price", &[(Aspect::Name, RELEASE)]),
            pair("when-price-on", "release_when_price_on", &[(Aspect::Name, RELEASE)]),
            pair("reduce-only", "reduce_only", &[]),
            pair("order-client-id", "order_client_id", &[]),
        ],
    },
    Operation {
        tool: "submit_order_list",
        command: "trade order oco",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<symbol>", "symbol", &[]),
            pair("tokens#<buy|sell>", "side", &[(Aspect::Name, SIDE)]),
            pair("tokens#<qty>", "qty", &[]),
            pair("limit", "limit_price", &[(Aspect::Name, LIST_PRICE)]),
            pair("stop", "stop_price", &[(Aspect::Name, LIST_PRICE)]),
            pair("stop-limit", "stop_limit_price", &[(Aspect::Name, LIST_PRICE)]),
            pair("trail", "trail", &[]),
            pair("reduce-only", "reduce_only", &[]),
            pair("order-client-id", "order_client_ids", &[(Aspect::Name, LEG_IDS)]),
        ],
    },
    Operation {
        tool: "submit_order_list",
        command: "trade order oto",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<symbol>", "symbol", &[]),
            pair("tokens#<buy|sell>", "side", &[(Aspect::Name, SIDE)]),
            pair("tokens#<qty>", "qty", &[]),
            pair("tokens#@<price>", "entry_price", &[(Aspect::Name, ENTRY)]),
            pair("stop", "stop_price", &[(Aspect::Name, LIST_PRICE)]),
            pair("limit", "limit_price", &[(Aspect::Name, LIST_PRICE)]),
            pair("trail", "trail", &[]),
            pair("order-client-id", "order_client_ids", &[(Aspect::Name, LEG_IDS)]),
        ],
    },
    Operation {
        tool: "submit_order_list",
        command: "trade order ladder",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<symbol>", "symbol", &[]),
            pair("tokens#<buy|sell>", "side", &[(Aspect::Name, SIDE)]),
            pair("tokens#<qty>", "qty", &[]),
            pair("tokens#@<price>", "entry_price", &[(Aspect::Name, ENTRY)]),
            pair("stop-loss", "stop_loss", &[]),
            pair("trail", "trail", &[]),
            pair(
                "take-profit",
                "take_profit",
                &[(
                    Aspect::Meaning,
                    "the CLI repeats `--take-profit`, one PRICE:QTY target each; MCP's is one \
                     `price:qty,price:qty` string",
                )],
            ),
            pair("order-client-id", "order_client_ids", &[(Aspect::Name, LEG_IDS)]),
        ],
    },
    Operation {
        tool: "submit_combo",
        command: "trade order combo",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<buy|sell>", "side", &[(Aspect::Name, SIDE)]),
            pair("tokens#<qty>", "qty", &[]),
            pair("leg", "legs", &[(Aspect::Name, LEGS), (Aspect::Required, LEGS)]),
            pair("net", "net_limit", &[(Aspect::Name, "`--net` on the CLI, `net_limit` on MCP")]),
            pair("order-client-id", "order_client_id", &[]),
            pair("time-in-force", "time_in_force", &[]),
        ],
    },
    Operation {
        tool: "cancel_order",
        command: "trade order cancel",
        pairs: &[pair(
            "ids",
            "order_client_id",
            &[(Aspect::Name, ONE_ID), (Aspect::Required, ONE_ID)],
        )],
    },
    Operation {
        tool: "modify_order",
        command: "trade order modify",
        pairs: &[
            pair("ids", "order_client_id", &[(Aspect::Name, ONE_ID), (Aspect::Required, ONE_ID)]),
            pair("qty", "new_qty", &[(Aspect::Name, NEW_VALUE)]),
            pair("price", "new_price", &[(Aspect::Name, NEW_VALUE)]),
            pair("trigger", "new_trigger_price", &[(Aspect::Name, NEW_VALUE)]),
        ],
    },
    Operation {
        tool: "flatten",
        command: "trade position flatten",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("tokens#<symbol>", "symbol", &[]),
        ],
    },
    Operation {
        tool: "close_all",
        command: "trade position close-all",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[]),
        ],
    },
    Operation {
        tool: "mass_cancel",
        command: "trade order mass-cancel",
        pairs: &[
            pair("tokens#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("tokens#<account>", "account", &[]),
            pair("tokens#<symbol>", "symbol", &[]),
        ],
    },
    Operation { tool: "set_trading_state", command: "trade halt", pairs: &[] },
    Operation { tool: "set_trading_state", command: "trade resume", pairs: &[] },
    Operation {
        tool: "mount_strategy",
        command: "trade strategy mount",
        pairs: &[
            pair("positionals#<account>", "venue", &[(Aspect::Name, BOOK)]),
            pair("positionals#<account>", "account", &[(Aspect::Required, BOOK_LABEL)]),
            pair("positionals#<symbol>", "symbol", &[]),
            pair("positionals#<interval>", "interval", &[]),
            pair(
                "id",
                "mount_id",
                &[(Aspect::Name, "the mount id: `--id` on the CLI, `mount_id` on MCP")],
            ),
            pair("strategy", "strategy", &[(Aspect::Required, MOUNT_FILE)]),
            pair("params", "params", &[]),
        ],
    },
    Operation {
        tool: "unmount_strategy",
        command: "trade strategy unmount",
        pairs: &[pair(
            "ids",
            "mount_id",
            &[
                (Aspect::Name, "the CLI's positional mount id; MCP calls the mount id `mount_id`"),
                (Aspect::Required, ONE_ID),
            ],
        )],
    },
];

/// MCP tools with no `vike-cli` command.
const MCP_ONLY_TOOLS: &[(&str, &str)] = &[
    (
        "profile_template",
        "the backtest profile's tables and keys, read from the published `profile.json`, the release asset \
         beside this binary's `cli.json`; no `vike-cli` command prints it",
    ),
    (
        "settings_show",
        "the CLI reads THIS box's settings (`vike-cli config show`), never a running node's; its \
         `trade` REPL reads the node's only to preview a write \
         (`crates/vike-cli/src/cmd/trade/write.rs`'s `run_set_setting`)",
    ),
    (
        "set_setting",
        "on the CLI only the `trade` REPL's `set-setting` word, which no `clap` tree publishes \
         (`crates/vike-cli/src/cmd/trade/repl/grammar.rs`'s `parse_lifecycle`)",
    ),
    ("run_status", BACKGROUND),
    ("cancel_run", BACKGROUND),
];

const BACKGROUND: &str = "a CLI run is a process of its shell, which backgrounds it (`&`) and stops it (Ctrl-C); \
                          MCP's background run outlives the call that started it, not the server";

/// The arguments of a tool that no command of its operations takes.
const MCP_ONLY_ARGUMENTS: &[(&str, &[&str], &str)] = &[
    ("run_backtest", &["script"], SCRIPT_RETIRED),
    ("run_sweep", &["script"], SCRIPT_RETIRED),
    ("run_walk_forward", &["script"], SCRIPT_RETIRED),
    ("run_backtest", &["background"], BACKGROUND),
    ("run_sweep", &["background"], BACKGROUND),
    ("run_walk_forward", &["background"], BACKGROUND),
    (
        "run_walk_forward",
        &["include_curve"],
        "the CLI's `--json` prints a walk-forward's whole report, its stitched curve in it (its table \
         prints none); MCP leaves the curve out unless asked, as `run_sweep` answers none",
    ),
    (
        "submit_order_list",
        &["shape"],
        "the CLI names the shape by its verb: `trade order oco`, `oto`, `ladder`",
    ),
    (
        "set_trading_state",
        &["state"],
        "the CLI names the state by its verb, `trade halt` (halted) and `trade resume` (active); a \
         node-wide `reducing` has no verb (`trade account reduce` is one account's)",
    ),
    ("delete_series", &["reason"], "`data history rm` takes no rationale"),
];

const SCRIPT_RETIRED: &str = "both doors refuse an inline source (`backtest.script_unsupported`): the \
                              CLI's `--script` is hidden on `backtest run` and refused there, and MCP \
                              still publishes `script` to refuse it by name";

/// The flags of a command that no tool of its operations takes.
const CLI_ONLY_FLAGS: &[(&str, &[&str], &str)] = &[
    (
        "backtest run",
        &[
            "set",
            "venue",
            "symbol",
            "interval",
            "from",
            "to",
            "kind",
            "strategy",
            "sdk",
            "cash",
            "decide",
            "param",
            "preset",
            "explain-data",
            "require-coverage",
            "max-gap",
            "on-gap",
            "universe",
        ],
        "writes one profile key (`cli.json`'s `flags[].profile_key`); MCP takes the whole profile \
         TOML, which writes it",
    ),
    (
        "backtest run",
        &["local", "engine"],
        "runs the engine on THIS box; MCP runs every backtest on the backtesthub",
    ),
    (
        "backtest run",
        &["whole"],
        "the CLI answers a summary unless `--whole`; MCP's answer is each tool's own (`run_backtest` \
         the whole results, `run_sweep` the summarized search)",
    ),
    (
        "backtest run",
        &["write-profile", "show-effective"],
        "writes or prints the resolved profile instead of running: an authoring aid of the CLI's",
    ),
    ("backtest script-check", &["positionals"], READ_VERBS),
    ("backtest params", &["positionals"], READ_VERBS),
    (
        "backtest params",
        &["strategy"],
        "reads a strategy FILE's declared params; MCP's `discover_params` reads Rhai source only",
    ),
    (
        "backtest templates",
        &["positionals"],
        "one starter, by id; MCP's `list_templates` answers every starter with its code",
    ),
    (
        "backtest templates",
        &["write-strategy"],
        "writes the starter to a file on this box; MCP returns the code",
    ),
    ("backtest strategies", &["positionals"], READ_VERBS),
    (
        "report",
        &[
            "compare",
            "breakdown",
            "mark-to-market",
            "interval",
            "without-foreign",
            "name",
            "html",
            "schema",
        ],
        "the CLI also reports a saved run (`run:<selector>`), compares two sources, marks an account \
         to market, leaves its foreign fills out, retitles it, writes HTML and prints the schema; MCP's \
         `report` answers one live account's realized document as JSON \
         (`crates/vike-mcp/src/node_reads.rs`'s `tool_report`)",
    ),
    (
        "trade order submit",
        &["post-only"],
        "MCP's maker-only lifetime is `time_in_force: post_only`",
    ),
    (
        "data history rm",
        &["dry-run"],
        "MCP's preview IS the dry run: without `confirm` and its token nothing is deleted",
    ),
    (
        "data history rm",
        &["lane"],
        "narrows the delete to one `source=` lane; MCP's selector has no lane dimension",
    ),
    (
        "data history ls",
        &["kind", "venue", "symbol", "limit"],
        "MCP's `list_series` takes no filter: it answers every series, cut at \
         `crates/vike-mcp/src/data_tools.rs`'s `LIST_SERIES_LIMIT`",
    ),
];

const READ_VERBS: &str =
    "the `backtest` read verbs share one positional list; this verb reads none";

/// The commands whose sub-verbs read ONE flag set and refuse a foreign flag after the parse, so a flag
/// is held to the table only where a pair or a row names it.
const SHARED_FLAG_SETS: &[&str] = &["data history"];

/// `vike-cli` commands with no MCP tool, inside the planes MCP serves.
const CLI_ONLY_COMMANDS: &[(&str, &str)] = &[
    (
        "trade",
        "the interactive REPL over one node session (its `set-setting` word is MCP's `set_setting`); \
         an MCP session is one session already",
    ),
    ("trade links", "every venue's link sessions; no MCP tool reads them"),
    ("trade venue resume", "releases ONE venue's link dead-man halt early; no MCP tool"),
    ("trade account halt", ONE_ACCOUNT_STATE),
    ("trade account reduce", ONE_ACCOUNT_STATE),
    ("trade account resume", ONE_ACCOUNT_STATE),
    (
        "trade account remove-symbol",
        "gives back one admitted symbol of an account's engine; no MCP tool",
    ),
    (
        "trade order ls",
        "the working orders of the node's frame, which `node_snapshot` answers whole",
    ),
    ("trade position ls", "the positions of the node's frame, which `node_snapshot` answers whole"),
    ("trade order stops", "the reduce-only stops the NODE holds; no MCP tool lists them"),
    (
        "trade order bracket",
        "no MCP bracket tool: `submit_order_list`'s shapes are oco, oto and ladder",
    ),
    ("trade strategy reset-budget", "clears one mount's budget latch; no MCP tool"),
    ("trade reconcile ls", RECONCILE),
    ("trade reconcile confirm", RECONCILE),
    ("trade reconcile cancel", RECONCILE),
    ("trade venue balance", VENUE_READ),
    ("trade venue open-orders", VENUE_READ),
    ("trade venue positions", VENUE_READ),
    ("backtest ls", SAVED_RUNS),
    ("backtest show", SAVED_RUNS),
    ("backtest path", SAVED_RUNS),
    ("backtest tag", SAVED_RUNS),
    ("backtest diff", SAVED_RUNS),
    ("backtest gate", SAVED_RUNS),
    ("backtest prune", SAVED_RUNS),
    ("backtest script-api", "prints the scripting API reference; no MCP tool"),
    ("data history fetch", HISTORY),
    ("data history running", HISTORY),
    ("data history cancel", HISTORY),
    ("data history import", HISTORY),
    ("data history export", HISTORY),
    ("data history get", HISTORY),
    ("data history gaps", HISTORY),
    ("data history coverage", HISTORY),
    ("data history health", HISTORY),
    ("data history universe", HISTORY),
    ("data history gate", HISTORY),
    ("data history repair", HISTORY),
    ("data history rename", HISTORY),
];

const ONE_ACCOUNT_STATE: &str = "ONE account's trading state; MCP's `set_trading_state` sets every \
                                 account on the node and takes no account";
const VENUE_READ: &str = "asks an account's VENUE what it holds or has open; no MCP tool";
const RECONCILE: &str = "a held reconcile divergence is an operator's decision about the venue's real \
                         account; no MCP tool";
const SAVED_RUNS: &str = "reads, marks, compares or prunes the runs saved in this project; MCP answers \
                          each run in its tool's reply and reads no run directory";
const HISTORY: &str = "fills, inspects, renames or repairs the history store; MCP reads \
                       `list_series` and deletes with `delete_series` only";

/// Whole `vike-cli` planes or groups with no MCP tool: every command under them.
const CLI_ONLY_SCOPES: &[(&str, &str)] = &[
    (
        "access",
        "the hubs' access lists of device keys: who may read or command a hub is decided on the box",
    ),
    (
        "account",
        "THIS box's account table, the rows a credential is filed against: \
         `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` keeps every account \
         write off MCP",
    ),
    ("backend", "starts and sets up this box's daemons"),
    (
        "config",
        "THIS box's settings database; MCP reads a running node's (`settings_show`) and writes one row \
         there (`set_setting`)",
    ),
    ("data catalog", NOT_OFFERED),
    ("data instruments", NOT_OFFERED),
    ("data realtime", NOT_OFFERED),
    ("data source", NOT_OFFERED),
    ("data splits", NOT_OFFERED),
    ("gatewayhub", "sets up this box's gatewayhub"),
    ("init", "creates a project on this box"),
    ("research", NOT_OFFERED),
    (
        "secrets",
        "the credential store: `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` \
         keeps every credential verb off MCP",
    ),
    (
        "strategy",
        "`strategy migrate` REWRITES the user's strategy file and `sdk-diff` reads this box's SDKs",
    ),
    ("surface", "writes this binary's own reference, `cli.json`"),
    (
        "completions",
        "a shell's tab completion of this binary's own verbs; an agent reads `tools/list`",
    ),
];

const NOT_OFFERED: &str = "not offered to agents: no MCP tool was built for it";

// ---- refusals -------------------------------------------------------------------------------------

/// One refusal both doors make: an MCP call `tools.errors.json` pins, and the same ask of the CLI.
struct Refusal {
    tool: &'static str,
    /// The call's `arguments` as JSON, as the golden sends them (compared parsed).
    arguments: &'static str,
    /// The same ask of the shipped CLI.
    argv: &'static [&'static str],
    /// A fragment of the CLI's refusal: proof the ask reached the rule this row is about, rather
    /// than a refusal that differs only because it is about something else.
    cli_says: &'static str,
    /// Why the two say different things today; `None`: they must say the same.
    differs: Option<&'static str>,
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        tool: "submit_order",
        arguments: r#"{"qty": 0.0, "side": 1, "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &["trade", "order", "submit", "sim", "BTCUSDT", "buy", "0", "--node", DEAD],
        cli_says: "qty must be > 0",
        differs: None,
    },
    Refusal {
        tool: "submit_order",
        arguments: r#"{"order_type": "no_such_type", "qty": 1.0, "side": 1, "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &[
            "trade",
            "order",
            "submit",
            "sim",
            "BTCUSDT",
            "buy",
            "1",
            "--type",
            "no_such_type",
            "--trigger",
            "100",
            "--node",
            DEAD,
        ],
        cli_says: "is not a trigger kind",
        differs: Some(
            "the CLI's `--type` takes the trigger kinds only and names them; MCP reads every kind \
             through `vike_model::OrderType::parse` and quotes its refusal",
        ),
    },
    Refusal {
        tool: "submit_order",
        arguments: r#"{"account": "not a label!", "qty": 1.0, "side": 1, "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &[
            "trade",
            "order",
            "submit",
            "sim/not a label!",
            "BTCUSDT",
            "buy",
            "1",
            "--node",
            DEAD,
        ],
        cli_says: "an account label is A-Z and 0-9 only",
        differs: Some("the CLI refuses the whole `<account>` token, MCP the `account` argument"),
    },
    // The rules of a built submit bite in ONE order (`vike_client::verbs::submit_rules_refusal`): the CLI judged
    // the lifetime at its flag, so this market order was refused for its `ioc` there and for its trail on MCP.
    Refusal {
        tool: "submit_order",
        arguments: r#"{"qty": 1.0, "side": 1, "symbol": "BTCUSDT", "time_in_force": "ioc", "trail": 5.0, "venue": "sim"}"#,
        argv: &[
            "trade",
            "order",
            "submit",
            "sim",
            "BTCUSDT",
            "buy",
            "1",
            "--trail",
            "5",
            "--time-in-force",
            "ioc",
            "--node",
            DEAD,
        ],
        cli_says: "order.submit.trail_off_kind",
        differs: None,
    },
    // One peg anchor reader, any case (`vike_client::verbs::peg_of_word`): the CLI took `MID`, MCP refused it.
    Refusal {
        tool: "submit_order",
        arguments: r#"{"order_type": "pegged", "peg": "near", "qty": 1.0, "side": 1, "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &[
            "trade", "order", "submit", "sim", "BTCUSDT", "buy", "1", "--type", "pegged", "--peg",
            "near", "--node", DEAD,
        ],
        cli_says: "order.submit.peg_anchor",
        differs: None,
    },
    // One modify builder (`vike_client::verbs::modify_of`).
    Refusal {
        tool: "modify_order",
        arguments: r#"{"order_client_id": "c-1"}"#,
        argv: &["trade", "order", "modify", "c-1", "--node", DEAD],
        cli_says: "order.modify.nothing_to_change",
        differs: None,
    },
    Refusal {
        tool: "mount_strategy",
        arguments: r#"{"interval": "1m", "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &["trade", "strategy", "mount", "sim", "BTCUSDT", "1m", "--node", DEAD],
        cli_says: "--strategy",
        differs: Some(
            "one rule, two sentences: the CLI's says to pass `--strategy` and prints the usage, \
             MCP's says what the file is not (script source, a path on this machine)",
        ),
    },
    Refusal {
        tool: "mount_strategy",
        arguments: r#"{"interval": "1m", "params": 5, "strategy": "spread_maker.rs", "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &[
            "trade",
            "strategy",
            "mount",
            "sim",
            "BTCUSDT",
            "1m",
            "--strategy",
            "s.rs",
            "--params",
            "5",
            "--node",
            DEAD,
        ],
        cli_says: "must be a JSON OBJECT",
        differs: Some("two copies of the params rule, worded apart"),
    },
    Refusal {
        tool: "mount_strategy",
        arguments: r#"{"file": "s.rs", "interval": "1m", "symbol": "BTCUSDT", "venue": "sim"}"#,
        argv: &[
            "trade", "strategy", "mount", "sim", "BTCUSDT", "1m", "--file", "s.rs", "--node", DEAD,
        ],
        cli_says: "is now",
        differs: None,
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"group": "G", "kind": "bar", "produced_by": "klines:", "symbol": "BTCUSDT", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT",
            "--group",
            "G",
            "--produced-by",
            "klines:",
            "--addr",
            DEAD,
        ],
        cli_says: "ALTERNATIVES",
        differs: None,
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"group": "G", "interval": "1h", "kind": "bar", "produced_by": "klines:", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--group",
            "G",
            "--interval",
            "1h",
            "--produced-by",
            "klines:",
            "--addr",
            DEAD,
        ],
        cli_says: "does not apply to --group",
        differs: None,
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"kind": "bar", "produced_by": "klines:", "symbol": "BTC*", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "BTC*",
            "--produced-by",
            "klines:",
            "--addr",
            DEAD,
        ],
        cli_says: "glob character",
        differs: None,
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"kind": "bar", "produced_by": "klines:", "symbol": "", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--symbol",
            "",
            "--produced-by",
            "klines:",
            "--addr",
            DEAD,
        ],
        cli_says: "needs a non-empty value",
        differs: Some(
            "the CLI's command-line reader refuses ANY blank value in one sentence of its own \
             (`vike_utility::cli`), before `rm`'s rule; MCP's rule says to omit it to wildcard",
        ),
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"kind": "bar", "produced_by": "crates/vike-data/src/klines.rs", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "bar",
            "--venue",
            "binance",
            "--produced-by",
            // The golden's path names no tracked file, so a repo-anchored one would be a dead path key
            // (`crates/vike-ops/tests/hygiene/path_key_gate.rs`); the CLI refuses any value with a `/`.
            "producer/klines.rs",
            "--addr",
            DEAD,
        ],
        cli_says: "PRODUCER PATH",
        differs: Some(
            "one rule and one text (`vike_data::store::removal::produced_by_refusal`), each quoting the \
             path it was given, and the two rows give two paths for the reason above",
        ),
    },
    Refusal {
        tool: "delete_series",
        arguments: r#"{"kind": "no_such_kind", "produced_by": "klines:", "venue": "binance"}"#,
        argv: &[
            "data",
            "history",
            "rm",
            "--kind",
            "no_such_kind",
            "--venue",
            "binance",
            "--produced-by",
            "klines:",
            "--addr",
            DEAD,
        ],
        cli_says: "unknown kind",
        differs: None,
    },
];

/// MCP refusals with no CLI ask that means the same, each with why.
const NO_CLI_TWIN: &[(&str, &str, &str)] = &[
    ("no_such_tool", "{}", PROTOCOL),
    ("", "null", PROTOCOL),
    ("validate_strategy", "5", "a JSON value of the wrong type has no argv form"),
    ("submit_order", "{}", ABSENT),
    ("cancel_order", "{}", ABSENT),
    ("modify_order", "{}", ABSENT),
    ("flatten", "{}", ABSENT),
    ("set_trading_state", "{}", ABSENT),
    ("mount_strategy", "{}", ABSENT),
    ("unmount_strategy", "{}", ABSENT),
    ("set_setting", "{}", ABSENT),
    ("delete_series", "{}", ABSENT),
    (
        "submit_order",
        r#"{"qty": 1.0, "side": "buy", "symbol": "BTCUSDT", "venue": "sim"}"#,
        "a JSON type: the CLI's side IS a word (`buy|sell`), MCP's an integer",
    ),
    (
        "submit_order",
        r#"{"order_type": "limit", "qty": 1.0, "side": 1, "symbol": "BTCUSDT", "venue": "sim"}"#,
        "the CLI has no `limit` word to send without a price: `@<price>` IS the limit",
    ),
    (
        "set_trading_state",
        r#"{"state": "no_such_state"}"#,
        "the CLI names the state by its verb, so there is no word to refuse",
    ),
    (
        "mount_strategy",
        r#"{"interval": "1m", "name": "spread_maker", "rhai": "a.rhai", "symbol": "BTCUSDT", "venue": "sim"}"#,
        "the CLI has no `--name` / `--rhai` any more, so `clap` refuses either as an unknown \
         argument; MCP's argument struct takes neither and asks for the missing `strategy`",
    ),
    (
        "unmount_strategy",
        r#"{"controller_id": "sim__BTCUSDT__1m"}"#,
        "MCP's mount id was `controller_id` before it was `mount_id`; the CLI's was always the \
         positional, so it has no old spelling to refuse",
    ),
    ("run_status", "{}", BACKGROUND),
    ("run_status", r#"{"run_id": "run-0-1"}"#, BACKGROUND),
    ("cancel_run", "{}", BACKGROUND),
    ("cancel_run", r#"{"run_id": "run-0-1"}"#, BACKGROUND),
    ("run_backtest", r#"{"background": "yes", "profile": "x"}"#, BACKGROUND),
];

const PROTOCOL: &str = "a tool name is the MCP protocol's own; the CLI's unknown verb is `clap`'s";
const ABSENT: &str = "an absent required argument: MCP names it in a sentence of its own, the CLI \
                      answers its verb's usage; one sentence per argument is the refusal catalog's";

// ---- the one order gate, both doors (the owner's D1-D5 of 2026-11-24) ------------------------------

/// What the MCP server answered each row of [`GATE_ROWS`]: the golden `order_gate.json`, recorded by
/// `crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot/gate.rs`'s `order_gate` against a paper node.
const ORDER_GATE: &str = include_str!("../../../vike-mcp/tests/fixtures/mcp_wire/order_gate.json");

/// The paper node both doors are asked against: the golden's venue and token, so a row's argv and its MCP
/// arguments name one book.
const GATE_VENUE: &str = "polymarket";
const GATE_TOKEN: &str = "MCP_WIRE_TOKEN";
const GATE_OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-order-gate-rows";
const GATE_CONTROL_KEY: &str = "DUMMY-control-key-for-the-order-gate-rows";

/// **One input, both doors** (`vike_client::order_gate::judge` is the one rule; its own tests pin it verb by verb). A
/// row is the MCP tool and its arguments as the golden records them (`gate.rs`'s `ORDER_GATE_CALLS`, but the golden
/// scrubs the resting order's pinned id `Rest1` as the second id it saw, `<minted-order-client-id-2>`), the same ask of
/// the shipped CLI one-shot (after `trade`, before `--node`), and the id BOTH must refuse with, `None` where both pass.
/// Both run against a paper node at this box's `policy.max_notional_per_order` of 1, after the same two orders: a
/// market warm-up (so the node publishes the book and its value) and the resting limit `Rest1`, 1 at 0.5.
///
/// Pinned by the unit tests only, because a paper node cannot show them: a combo over the cap by a leg limit (the
/// paper book trades one token, a combo needs two), an exit list under a covered position (no paper fill makes one),
/// a bracket and the account, venue, symbol and reconcile verbs (one door: `NOT_ON_THE_MCP`), and a refused delete
/// provenance (no datahub here; `vike-data`'s `a_refused_provenance_plan_is_one_typed_refusal` and the MCP's
/// `a_refused_provenance_plan_issues_no_token_and_the_confirm_asks_again`).
struct GateRow {
    tool: &'static str,
    arguments: &'static str,
    argv: &'static [&'static str],
    id: Option<&'static str>,
}

const OVER: Option<&str> = Some("order.edge.over_max_notional");
const CHARSET: Option<&str> = Some("order.edge.order_client_id_charset");

const GATE_ROWS: &[GateRow] = &[
    GateRow {
        tool: "submit_order",
        arguments: r#"{"order_type": "limit", "price": 0.5, "qty": 10.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
        argv: &["order", "submit", GATE_VENUE, GATE_TOKEN, "buy", "10", "@0.5"],
        id: OVER,
    },
    GateRow {
        tool: "submit_order",
        arguments: r#"{"order_client_id": "BAD-ID-1", "qty": 1.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
        argv: &[
            "order",
            "submit",
            GATE_VENUE,
            GATE_TOKEN,
            "buy",
            "1",
            "--order-client-id",
            "BAD-ID-1",
        ],
        id: CHARSET,
    },
    GateRow {
        tool: "submit_order",
        arguments: r#"{"order_type": "limit", "price": 0.5, "qty": 1.0, "side": 1, "symbol": "MCP_WIRE_TOKEN", "venue": "nosuchvenue"}"#,
        argv: &["order", "submit", "nosuchvenue", GATE_TOKEN, "buy", "1", "@0.5"],
        id: Some("order.edge.venue_not_mounted"),
    },
    GateRow {
        tool: "submit_order_list",
        arguments: r#"{"limit_price": 0.5, "qty": 10.0, "shape": "oco", "side": 1, "stop_price": 0.6, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
        argv: &[
            "order", "oco", GATE_VENUE, GATE_TOKEN, "buy", "10", "--limit", "0.5", "--stop", "0.6",
        ],
        id: OVER,
    },
    GateRow {
        tool: "submit_order_list",
        arguments: r#"{"limit_price": 0.5, "order_client_ids": "BAD-1,Good2", "qty": 1.0, "shape": "oco", "side": 1, "stop_price": 0.6, "symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
        argv: &[
            "order",
            "oco",
            GATE_VENUE,
            GATE_TOKEN,
            "buy",
            "1",
            "--limit",
            "0.5",
            "--stop",
            "0.6",
            "--order-client-id",
            "BAD-1",
            "--order-client-id",
            "Good2",
        ],
        id: CHARSET,
    },
    GateRow {
        tool: "submit_combo",
        arguments: r#"{"legs": [{"limit": 0.5, "ratio": 1, "symbol": "MCP_WIRE_TOKEN"}, {"limit": 0.4, "ratio": -1, "symbol": "OTHER"}], "order_client_id": "BAD-ID-2", "qty": 1.0, "side": 1, "venue": "polymarket"}"#,
        argv: &[
            "order",
            "combo",
            GATE_VENUE,
            "buy",
            "1",
            "--leg",
            "MCP_WIRE_TOKEN:1@0.5",
            "--leg",
            "OTHER:-1@0.4",
            "--order-client-id",
            "BAD-ID-2",
        ],
        id: CHARSET,
    },
    // D3: a modify measured from the resting order, its qty raised with no price, and its price alone.
    GateRow {
        tool: "modify_order",
        arguments: r#"{"new_qty": 10.0, "order_client_id": "<minted-order-client-id-2>"}"#,
        argv: &["order", "modify", "Rest1", "--qty", "10"],
        id: OVER,
    },
    GateRow {
        tool: "modify_order",
        arguments: r#"{"new_price": 2.0, "order_client_id": "<minted-order-client-id-2>"}"#,
        argv: &["order", "modify", "Rest1", "--price", "2"],
        id: OVER,
    },
    // An id that already exists is warned of on both doors, never refused.
    GateRow {
        tool: "cancel_order",
        arguments: r#"{"order_client_id": "BAD-ID-3"}"#,
        argv: &["order", "cancel", "BAD-ID-3"],
        id: None,
    },
    // No ceiling blocks an exit, at a cap of 1 with a position open or not.
    GateRow {
        tool: "flatten",
        arguments: r#"{"symbol": "MCP_WIRE_TOKEN", "venue": "polymarket"}"#,
        argv: &["position", "flatten", GATE_VENUE, GATE_TOKEN],
        id: None,
    },
    GateRow {
        tool: "close_all",
        arguments: r#"{"venue": "polymarket"}"#,
        argv: &["position", "close-all", GATE_VENUE],
        id: None,
    },
    GateRow { tool: "close_all", arguments: "{}", argv: &["position", "close-all"], id: None },
    GateRow {
        tool: "mass_cancel",
        arguments: r#"{"venue": "polymarket"}"#,
        argv: &["order", "mass-cancel", GATE_VENUE],
        id: None,
    },
];

/// The id a refusal is rendered under (`error [<id>]: ...`), wherever the door's prefix put it.
fn refusal_id(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("error [")?;
    Some(rest.split_once(']')?.0.to_string())
}

/// The paper node the CLI rows run against, at this box's ceiling 1, after the two orders the golden stands on. One
/// per test process: no row sends anything (a refused one is not sent, a passing one stops at its confirm).
fn gate_node() -> &'static (String, String) {
    static NODE: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    NODE.get_or_init(|| {
        let (mount, addr) =
            common::spawn_paper_node(GATE_VENUE, GATE_TOKEN, GATE_OBSERVE_KEY, GATE_CONTROL_KEY);
        // The node and its settings live as long as this test process.
        std::mem::forget(mount);
        let dir = common::device_settings_dir(GATE_CONTROL_KEY);
        let settings = dir.path().to_str().expect("a UTF-8 tempdir").to_string();
        std::mem::forget(dir);
        let env = [("VIKE_SETTINGS_DIR", settings.as_str())];
        let cap = common::run(&["config", "set", "policy.max_notional_per_order", "1"], &env);
        assert!(cap.status.success(), "the cap row: {}", String::from_utf8_lossy(&cap.stderr));
        let node = addr.to_string();
        for order in [
            &["order", "submit", GATE_VENUE, GATE_TOKEN, "buy", "1"][..],
            &[
                "order",
                "submit",
                GATE_VENUE,
                GATE_TOKEN,
                "buy",
                "1",
                "@0.5",
                "--order-client-id",
                "Rest1",
            ],
        ] {
            let argv: Vec<&str> = ["trade"]
                .into_iter()
                .chain(order.iter().copied())
                .chain(["--node", &node, "--yes"])
                .collect();
            let out = common::run(&argv, &env);
            assert!(out.status.success(), "{order:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        assert!(
            vike_log::poll::wait_until(std::time::Duration::from_secs(30), || {
                node_order_ids(addr).iter().any(|id| id == "Rest1")
            }),
            "the resting order never reached the node's frame"
        );
        (node, settings)
    })
}

/// The orders the node's frame holds, by id, on a fresh observe connection.
fn node_order_ids(addr: impl std::net::ToSocketAddrs) -> Vec<String> {
    let observer =
        vike_tradehub_client::TradehubCoreHandle::connect(addr, GATE_OBSERVE_KEY.as_bytes())
            .expect("observe connect");
    let folded = || observer.snapshot().sequence > 0;
    vike_log::poll::wait_until(std::time::Duration::from_secs(10), folded);
    observer.snapshot().orders.iter().map(|o| o.order_client_id.clone()).collect()
}

/// The MCP's answer to `row`, off the golden: its refusal's text, or its preview's `structuredContent`.
fn mcp_answer(row: &GateRow) -> Result<Value, String> {
    let golden: Value = serde_json::from_str(ORDER_GATE).expect("the golden is JSON");
    let wanted: Value = serde_json::from_str(row.arguments).expect("a row's arguments are JSON");
    let steps = golden["transcript"].as_array().expect("the golden is a transcript");
    let id = steps
        .iter()
        .map(|step| &step["send"])
        .find(|send| {
            send["method"] == "tools/call"
                && send["params"]["name"] == row.tool
                && send["params"]["arguments"] == wanted
        })
        .unwrap_or_else(|| panic!("order_gate.json never calls `{}` {}", row.tool, row.arguments))
        ["id"]
        .clone();
    let result = &steps.iter().find(|step| step["recv"]["id"] == id).unwrap_or_else(|| {
        panic!("order_gate.json never answers `{}` {}", row.tool, row.arguments)
    })["recv"]["result"];
    if result["isError"] == true {
        return Err(result["content"][0]["text"].as_str().unwrap_or_default().to_string());
    }
    Ok(result["structuredContent"].clone())
}

/// Both doors, one row: the same id refused, or both past the gate; the node's order list unchanged.
fn check_gate_row(row: &GateRow) {
    let (node, settings) = gate_node();
    let mcp = mcp_answer(row);
    let mcp_id = mcp.as_ref().err().and_then(|text| refusal_id(text));
    assert_eq!(mcp_id.as_deref(), row.id, "MCP `{}` {}: {mcp:?}", row.tool, row.arguments);
    let before = node_order_ids(node.as_str());
    let argv: Vec<&str> = ["trade"]
        .into_iter()
        .chain(row.argv.iter().copied())
        .chain(["--node", node.as_str()])
        .collect();
    let out = common::run(&argv, &[("VIKE_SETTINGS_DIR", settings.as_str())]);
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(refusal_id(&text).as_deref(), row.id, "`vike-cli {}`: {text}", argv.join(" "));
    if row.id.is_none() {
        // Past the gate on the CLI is its confirm, which a closed stdin declines.
        assert!(
            text.contains("aborted — nothing was sent"),
            "`vike-cli {}`: {text}",
            argv.join(" ")
        );
        assert!(mcp.as_ref().is_ok_and(|preview| preview["preview_token"].is_string()), "{mcp:?}");
    }
    assert_eq!(
        node_order_ids(node.as_str()),
        before,
        "`vike-cli {}` changed the book",
        argv.join(" ")
    );
    if row.tool == "cancel_order" {
        let note =
            mcp.as_ref().ok().and_then(|p| p["order_client_id_note"].as_str()).unwrap_or_default();
        assert!(
            note.contains("could have minted") && text.contains(note),
            "MCP {note:?}, CLI {text}"
        );
    }
}

/// One `#[test]` per row, so a red names the verb.
macro_rules! gate_row_tests {
    ($($test:ident => $index:expr),* $(,)?) => {
        $(#[test] fn $test() { check_gate_row(&GATE_ROWS[$index]); })*
        const GATE_ROW_TESTS: usize = [$($index),*].len();
    };
}

gate_row_tests! {
    a_submit_over_the_ceiling_is_refused_on_both_doors => 0,
    a_submits_pinned_id_outside_the_charset_is_refused_on_both_doors => 1,
    a_submit_to_a_venue_the_node_does_not_mount_is_refused_on_both_doors => 2,
    an_order_list_over_the_ceiling_is_refused_on_both_doors => 3,
    an_order_lists_leg_id_outside_the_charset_is_refused_on_both_doors => 4,
    a_combos_pinned_id_outside_the_charset_is_refused_on_both_doors => 5,
    a_modify_raising_the_qty_with_no_price_is_refused_on_both_doors => 6,
    a_modify_of_the_price_alone_is_refused_on_both_doors => 7,
    a_cancel_of_an_id_outside_the_charset_is_warned_and_passes_on_both_doors => 8,
    a_flatten_passes_the_ceiling_on_both_doors => 9,
    a_close_all_of_a_venue_passes_the_ceiling_on_both_doors => 10,
    a_close_all_of_every_venue_passes_on_both_doors => 11,
    a_mass_cancel_passes_the_ceiling_on_both_doors => 12,
}

#[test]
fn every_gate_row_has_its_test() {
    assert_eq!(GATE_ROW_TESTS, GATE_ROWS.len());
}

// ---- the published surfaces -----------------------------------------------------------------------

/// One argument as a door publishes it.
#[derive(Clone, Debug)]
struct Arg {
    /// A bare switch (`cli.json`'s `value: none`), or a `boolean` MCP argument.
    switch: bool,
    required: bool,
    positional: bool,
    /// The CLI's `help`, MCP's `description`.
    text: String,
    /// A CLOSED word list published as data: `possible_values`, a `backtest` roster's `members`, MCP's
    /// `enum`. Empty where only the text names the words.
    words: Vec<String>,
    /// A default published as data: `cli.json`'s `default`, or a `backtest` flag's `default.value`.
    default: Option<String>,
}

/// Each command (its words) or tool, and its arguments by name.
type Surface = BTreeMap<String, BTreeMap<String, Arg>>;

fn strings(value: &Value) -> Vec<String> {
    value.as_array().map_or_else(Vec::new, |items| {
        items.iter().filter_map(Value::as_str).map(str::to_owned).collect()
    })
}

/// The CLI's `cli.json`, as the binary renders it.
fn cli_surface() -> Surface {
    let files = vike_cli::surface::rendered_files();
    let doc: Value = serde_json::from_str(files.get("cli.json").expect("the CLI renders cli.json"))
        .expect("cli.json is JSON");
    let flags = doc["flags"].as_array().expect("cli.json carries the backtest plane's flags");
    let rosters = doc["rosters"].as_array().expect("cli.json carries the backtest plane's rosters");
    let commands = doc["commands"].as_array().expect("cli.json carries every command");
    let mut out = Surface::new();
    for command in commands {
        let path = strings(&command["path"]);
        // The `backtest` plane publishes which sub-verb takes which flag: its read verbs share one set.
        let backtest_sub = match path.as_slice() {
            [plane, sub, ..] if plane == "backtest" => Some(sub.clone()),
            _ => None,
        };
        let mut args = BTreeMap::new();
        for arg in command["args"].as_array().expect("a command row lists its args") {
            let name = arg["name"].as_str().expect("an arg row is named").to_owned();
            let positional = arg["positional"] == true;
            let row =
                flags.iter().find(|f| f["long"].as_str() == Some(format!("--{name}").as_str()));
            let mut words = strings(&arg["possible_values"]);
            let mut default = strings(&arg["default"]).into_iter().next();
            if let (Some(sub), Some(row), false) = (&backtest_sub, row, positional) {
                if !strings(&row["applies_to"]).contains(sub) {
                    continue;
                }
                if words.is_empty()
                    && let Some(id) = row["roster_id"].as_str()
                {
                    let roster = rosters.iter().find(|r| r["id"] == id);
                    words = roster.map_or_else(Vec::new, |r| strings(&r["members"]));
                }
                if default.is_none() {
                    default = row["default"]["value"].as_str().map(str::to_owned);
                }
            }
            let text = arg["help"].as_str().unwrap_or_default().to_owned();
            let required = arg["required"] == true;
            let switch = arg["value"] == "none";
            args.insert(name, Arg { switch, required, positional, text, words, default });
        }
        out.insert(path.join(" "), args);
    }
    out
}

/// MCP's `tools/list`, as the golden holds it.
fn mcp_surface() -> Surface {
    let golden: Value = serde_json::from_str(TOOLS_LIST).expect("the golden is JSON");
    let tools = golden["transcript"]
        .as_array()
        .expect("the golden is a transcript")
        .iter()
        .find_map(|step| step["recv"]["result"]["tools"].as_array())
        .expect("the golden carries the tools/list answer");
    let mut out = Surface::new();
    for tool in tools {
        let schema = &tool["inputSchema"];
        let required = strings(&schema["required"]);
        let mut args = BTreeMap::new();
        for (name, property) in schema["properties"].as_object().into_iter().flatten() {
            args.insert(
                name.clone(),
                Arg {
                    switch: property["type"] == "boolean",
                    required: required.contains(name),
                    positional: false,
                    text: property["description"].as_str().unwrap_or_default().to_owned(),
                    words: strings(&property["enum"]),
                    default: None,
                },
            );
        }
        out.insert(tool["name"].as_str().expect("a named tool").to_owned(), args);
    }
    out
}

// ---- the comparison -------------------------------------------------------------------------------

/// The CLI side of a pair: the argument, and the slot of a token list when it names one.
fn cli_side<'a>(
    args: &'a BTreeMap<String, Arg>,
    cli: &'a str,
) -> Option<(&'a Arg, Option<&'a str>)> {
    match cli.split_once('#') {
        Some((id, slot)) => args
            .get(id)
            .filter(|arg| arg.positional && arg.text.contains(slot))
            .map(|arg| (arg, Some(slot))),
        None => args.get(cli).map(|arg| (arg, None)),
    }
}

/// The argument a pair's CLI side is, with its slot set aside.
fn cli_argument(cli: &str) -> &str {
    cli.split_once('#').map_or(cli, |(id, _)| id)
}

/// The words of a text a value word is compared against: runs of letters, digits, `_` and `-`.
fn words_of(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .filter(|w| !w.is_empty())
}

/// A value word, in either door's spelling: `stop-limit` is `stop_limit`.
fn spelled(word: &str) -> String {
    word.to_ascii_lowercase().replace('-', "_")
}

/// Does the door name `word` for this argument: in its closed list when it publishes one, else in its
/// text?
fn names(arg: &Arg, word: &str) -> bool {
    let want = spelled(word);
    if arg.words.is_empty() {
        words_of(&arg.text).any(|w| spelled(w) == want)
    } else {
        arg.words.iter().any(|w| spelled(w) == want)
    }
}

/// Does the door publish `value` as this argument's default: as data, or in a sentence that says so?
fn publishes_default(arg: &Arg, value: &str) -> bool {
    let text = arg.text.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    arg.default.as_deref().is_some_and(|d| d.to_ascii_lowercase().starts_with(&value))
        || [
            format!("default {value}"),
            format!("{value} (default"),
            format!("{value} (the default"),
            format!("default: {value}"),
            format!("default is {value}"),
            format!("omitted: {value}"),
        ]
        .iter()
        .any(|says| text.contains(says.as_str()))
}

/// The aspects on which the two sides of `pair` differ, measured; `Err` when a side is not there.
fn measured(
    pair: &Pair,
    cli_args: &BTreeMap<String, Arg>,
    mcp_args: &BTreeMap<String, Arg>,
) -> Result<BTreeSet<Aspect>, String> {
    let (cli, slot) = cli_side(cli_args, pair.cli).ok_or_else(|| {
        format!("the CLI has no `{}` (a slot must appear in the positional's help)", pair.cli)
    })?;
    let mcp = mcp_args.get(pair.mcp).ok_or_else(|| format!("MCP has no `{}`", pair.mcp))?;
    let mut out = BTreeSet::new();
    let word: String = match slot {
        Some(slot) => slot.chars().filter(|c| !"<>@[]".contains(*c)).collect(),
        None => pair.cli.to_owned(),
    };
    if word.replace('-', "_") != pair.mcp {
        out.insert(Aspect::Name);
    }
    if slot.is_none() && cli.switch != mcp.switch {
        out.insert(Aspect::Kind);
    }
    let cli_required = match slot {
        Some(slot) => !cli.text.contains(&format!("[{slot}")),
        None => cli.required,
    };
    if cli_required != mcp.required {
        out.insert(Aspect::Required);
    }
    if let Some(value) = pair.default {
        match (publishes_default(cli, value), publishes_default(mcp, value)) {
            (false, false) => {
                return Err(format!("neither door publishes the declared default `{value}`"));
            }
            (a, b) if a != b => {
                out.insert(Aspect::Default);
            }
            _ => {}
        }
    }
    if let Some(vocabulary) = pair.words {
        let vocabulary = vocabulary();
        if let Some(word) = vocabulary.iter().find(|w| !names(cli, w) && !names(mcp, w)) {
            return Err(format!(
                "neither door publishes `{word}`, a word of the declared vocabulary"
            ));
        }
        let outside = |arg: &Arg| {
            arg.words.iter().any(|w| !vocabulary.iter().any(|v| spelled(v) == spelled(w)))
        };
        if vocabulary.iter().any(|w| names(cli, w) != names(mcp, w)) || outside(cli) || outside(mcp)
        {
            out.insert(Aspect::Words);
        }
    }
    Ok(out)
}

/// The pairs that hold for one operation: its own, and every door pair both of its sides have.
fn pairs_of<'a>(
    operation: &'a Operation,
    cli_args: &BTreeMap<String, Arg>,
    mcp_args: &BTreeMap<String, Arg>,
) -> Vec<&'a Pair> {
    let door =
        DOOR_PAIRS.iter().filter(|p| cli_args.contains_key(p.cli) && mcp_args.contains_key(p.mcp));
    operation.pairs.iter().chain(door).collect()
}

fn under(command: &str, scope: &str) -> bool {
    command == scope || command.starts_with(&format!("{scope} "))
}

/// One finding per line, for the assertion message.
fn report(findings: &[String]) -> String {
    format!("{} finding(s):\n  {}", findings.len(), findings.join("\n  "))
}

// ---- the checks -----------------------------------------------------------------------------------

#[test]
fn every_operation_is_on_both_doors_or_declared_on_one() {
    let (cli, mcp) = (cli_surface(), mcp_surface());
    let mut findings = Vec::new();

    for tool in mcp.keys() {
        let operation = OPERATIONS.iter().any(|o| o.tool == tool);
        let only = MCP_ONLY_TOOLS.iter().any(|(t, _)| t == tool);
        if operation == only {
            findings.push(format!(
                "MCP tool `{tool}`: {}",
                if only {
                    "an operation AND declared MCP-only"
                } else {
                    "no operation and not declared MCP-only"
                }
            ));
        }
    }
    for (command, args) in &cli {
        let group =
            args.is_empty() && cli.keys().any(|other| other.starts_with(&format!("{command} ")));
        if command.is_empty() || group {
            continue;
        }
        let homes = [
            OPERATIONS.iter().any(|o| o.command == command),
            CLI_ONLY_COMMANDS.iter().any(|(c, _)| c == command),
            CLI_ONLY_SCOPES.iter().any(|(scope, _)| under(command, scope)),
        ];
        if homes.iter().filter(|home| **home).count() != 1 {
            findings.push(format!(
                "`vike-cli {command}` must be exactly one of: an operation, a CLI-only command, under a \
                 CLI-only scope (it is {homes:?})"
            ));
        }
    }
    // A declaration that names nothing is stale.
    for operation in OPERATIONS {
        if !cli.contains_key(operation.command) {
            findings.push(format!(
                "operation `{}`: no `vike-cli {}`",
                operation.tool, operation.command
            ));
        }
        if !mcp.contains_key(operation.tool) {
            findings.push(format!("operation `{}`: no such MCP tool", operation.tool));
        }
    }
    for (tool, _) in MCP_ONLY_TOOLS {
        if !mcp.contains_key(*tool) {
            findings.push(format!("MCP_ONLY_TOOLS names `{tool}`, which MCP does not serve"));
        }
    }
    for (command, _) in CLI_ONLY_COMMANDS {
        if !cli.contains_key(*command) {
            findings.push(format!(
                "CLI_ONLY_COMMANDS names `vike-cli {command}`, which does not exist"
            ));
        }
    }
    for (scope, _) in CLI_ONLY_SCOPES {
        if !cli.keys().any(|command| under(command, scope)) {
            findings
                .push(format!("CLI_ONLY_SCOPES names `vike-cli {scope}`, which holds no command"));
        }
    }
    assert!(findings.is_empty(), "the two doors' operations: {}", report(&findings));
}

#[test]
fn every_field_agrees_or_its_difference_is_declared() {
    let (cli, mcp) = (cli_surface(), mcp_surface());
    let empty = BTreeMap::new();
    let mut findings = Vec::new();
    let mut compared = 0usize;

    for operation in OPERATIONS {
        let cli_args = cli.get(operation.command).unwrap_or(&empty);
        let mcp_args = mcp.get(operation.tool).unwrap_or(&empty);
        let at = format!("`{}` / `vike-cli {}`", operation.tool, operation.command);
        for pair in pairs_of(operation, cli_args, mcp_args) {
            let declared: BTreeSet<Aspect> = pair
                .differs
                .iter()
                .map(|(aspect, _)| *aspect)
                .filter(|a| *a != Aspect::Meaning)
                .collect();
            match measured(pair, cli_args, mcp_args) {
                Err(why) => findings.push(format!("{at} `{}` <-> `{}`: {why}", pair.cli, pair.mcp)),
                Ok(observed) => {
                    compared += 1;
                    for aspect in observed.difference(&declared) {
                        findings.push(format!(
                            "{at} `{}` <-> `{}`: they differ on {aspect:?}, and no reason is declared",
                            pair.cli, pair.mcp
                        ));
                    }
                    for aspect in declared.difference(&observed) {
                        findings.push(format!(
                            "{at} `{}` <-> `{}`: declared to differ on {aspect:?}, and they agree now — \
                             delete the declaration",
                            pair.cli, pair.mcp
                        ));
                    }
                }
            }
        }
    }

    // Every MCP argument of a tool is a pair of one of its operations, the door's, or declared MCP-only.
    for (tool, args) in &mcp {
        let operations: Vec<&Operation> = OPERATIONS.iter().filter(|o| o.tool == tool).collect();
        if operations.is_empty() {
            continue;
        }
        for name in args.keys() {
            let paired = operations.iter().any(|o| {
                let cli_args = cli.get(o.command).unwrap_or(&empty);
                pairs_of(o, cli_args, args).iter().any(|p| p.mcp == name)
            });
            let homes = [
                paired,
                MCP_DOOR.iter().any(|(n, _)| n == name),
                MCP_ONLY_ARGUMENTS
                    .iter()
                    .any(|(t, names, _)| t == tool && names.contains(&name.as_str())),
            ];
            if homes.iter().filter(|home| **home).count() != 1 {
                findings.push(format!(
                    "MCP `{tool}`'s `{name}` must be exactly one of: paired, the door's, MCP-only \
                     (it is {homes:?}); CLI args: {:?}",
                    operations
                        .iter()
                        .map(|o| (
                            o.command,
                            cli.get(o.command).map(|a| a.keys().collect::<Vec<_>>())
                        ))
                        .collect::<Vec<_>>()
                ));
            }
        }
    }
    // Every CLI flag of a command is a pair of one of its operations, the door's, or declared CLI-only.
    for (command, args) in &cli {
        let operations: Vec<&Operation> =
            OPERATIONS.iter().filter(|o| o.command == command).collect();
        if operations.is_empty() || SHARED_FLAG_SETS.iter().any(|scope| under(command, scope)) {
            continue;
        }
        for name in args.keys() {
            let paired = operations.iter().any(|o| {
                let mcp_args = mcp.get(o.tool).unwrap_or(&empty);
                pairs_of(o, args, mcp_args).iter().any(|p| cli_argument(p.cli) == name)
            });
            let homes = [
                paired,
                CLI_DOOR.iter().any(|(n, _)| n == name),
                CLI_ONLY_FLAGS
                    .iter()
                    .any(|(c, names, _)| c == command && names.contains(&name.as_str())),
            ];
            if homes.iter().filter(|home| **home).count() != 1 {
                findings.push(format!(
                    "`vike-cli {command} --{name}` must be exactly one of: paired, the door's, CLI-only \
                     (it is {homes:?}); help: {:?}",
                    args[name].text
                ));
            }
        }
    }
    // A one-door row that names nothing is stale.
    for (tool, names, _) in MCP_ONLY_ARGUMENTS {
        for name in *names {
            if !mcp.get(*tool).is_some_and(|args| args.contains_key(*name)) {
                findings.push(format!(
                    "MCP_ONLY_ARGUMENTS names `{tool}`'s `{name}`, which MCP does not take"
                ));
            }
        }
    }
    for (command, names, _) in CLI_ONLY_FLAGS {
        for name in *names {
            if !cli.get(*command).is_some_and(|args| args.contains_key(*name)) {
                findings.push(format!(
                    "CLI_ONLY_FLAGS names `vike-cli {command} --{name}`, which the CLI does not take"
                ));
            }
        }
    }
    assert!(findings.is_empty(), "the two doors' fields: {}", report(&findings));
    // Non-vacuity: a surface that stopped reading would compare nothing and pass.
    assert!(compared >= 100, "only {compared} pairs were compared: a surface stopped reading");
}

/// What a refusal says once its door is set aside: the `vike-cli` and `<command>:` / `<tool>:`
/// prefixes each door writes before the sentence, then an argument's spelling (`--produced-by`,
/// `` `produced_by` `` and `produced_by` are one word). Its first line only.
fn body(text: &str, door: &[&str]) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut rest = line.strip_prefix("vike-cli ").unwrap_or(line);
    while let Some((head, tail)) = rest.split_once(": ") {
        if !head.split(' ').all(|word| door.contains(&word)) {
            break;
        }
        rest = tail;
    }
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => {}
            '-' if chars.peek() == Some(&'-') && !out.ends_with(|p: char| p.is_alphanumeric()) => {
                chars.next();
                while let Some(&next) = chars.peek() {
                    match next {
                        'a'..='z' | '0'..='9' => out.push(next),
                        '-' => out.push('_'),
                        _ => break,
                    }
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[test]
fn the_body_of_a_refusal_sets_each_door_aside() {
    assert_eq!(
        body(
            "vike-cli trade order submit: submit: qty must be > 0\nusage: ...",
            &["trade", "order", "submit"]
        ),
        "qty must be > 0"
    );
    assert_eq!(body("submit_order: qty must be > 0", &["submit_order"]), "qty must be > 0");
    assert_eq!(
        body(
            "vike-cli data: --interval does not apply to --group: no `interval=` segment",
            &["data"]
        ),
        "interval does not apply to group: no interval= segment"
    );
    assert_eq!(
        body("`interval` does not apply to `group`: no `interval=` segment", &["delete_series"]),
        "interval does not apply to group: no interval= segment"
    );
    assert_eq!(body("--produced-by \"a\" looks", &[]), "produced_by \"a\" looks");
}

#[test]
fn every_mcp_refusal_says_what_the_cli_says_or_is_declared() {
    let golden: Value = serde_json::from_str(TOOL_ERRORS).expect("the golden is JSON");
    let mut calls = BTreeMap::new();
    let mut refused = Vec::new();
    for step in golden["transcript"].as_array().expect("the golden is a transcript") {
        let send = &step["send"];
        if send["method"] == "tools/call" {
            let tool = send["params"]["name"].as_str().unwrap_or_default().to_owned();
            calls.insert(send["id"].to_string(), (tool, send["params"]["arguments"].clone()));
        }
        let result = &step["recv"]["result"];
        if result["isError"] == true {
            let text = result["content"][0]["text"].as_str().unwrap_or_default().to_owned();
            refused.push((step["recv"]["id"].to_string(), text));
        }
    }
    let parsed =
        |json: &str| -> Value { serde_json::from_str(json).expect("a row's arguments are JSON") };
    let mut findings = Vec::new();
    let mut matched = BTreeSet::new();
    for (id, mcp_text) in &refused {
        let Some((tool, arguments)) = calls.get(id) else { continue };
        let twins: Vec<&Refusal> = REFUSALS
            .iter()
            .filter(|r| r.tool == tool && parsed(r.arguments) == *arguments)
            .collect();
        let excused =
            NO_CLI_TWIN.iter().filter(|(t, a, _)| t == tool && parsed(a) == *arguments).count();
        if twins.len() + excused != 1 {
            findings.push(format!(
                "MCP refuses `{tool}` {arguments} ({mcp_text:?}): it must be exactly one REFUSALS or \
                 NO_CLI_TWIN row ({} + {excused})",
                twins.len()
            ));
        }
        for twin in twins {
            matched.insert((twin.tool, twin.arguments));
            let out = common::run(twin.argv, &[]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let door: Vec<&str> = twin
                .argv
                .iter()
                .copied()
                .take_while(|w| {
                    !w.starts_with('-') && w.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                })
                .collect();
            let (cli_body, mcp_body) = (body(&stderr, &door), body(mcp_text, &[tool.as_str()]));
            let at = format!("`{tool}` {arguments} / `vike-cli {}`", twin.argv.join(" "));
            if out.status.success() {
                findings.push(format!(
                    "{at}: the CLI ACCEPTED it (stdout {:?})",
                    String::from_utf8_lossy(&out.stdout)
                ));
            } else if !stderr.contains(twin.cli_says) {
                findings.push(format!(
                    "{at}: the CLI refused something else (no {:?}): {stderr:?}",
                    twin.cli_says
                ));
            } else if twin.differs.is_none() && cli_body != mcp_body {
                findings.push(format!(
                    "{at}: the two refuse apart\n    CLI {cli_body:?}\n    MCP {mcp_body:?}"
                ));
            } else if twin.differs.is_some() && cli_body == mcp_body {
                findings.push(format!(
                    "{at}: declared to differ, and both now say {cli_body:?}: delete `differs`"
                ));
            }
        }
    }
    for refusal in REFUSALS {
        if !matched.contains(&(refusal.tool, refusal.arguments)) {
            findings.push(format!(
                "REFUSALS names `{}` {}, which tools.errors.json does not refuse",
                refusal.tool, refusal.arguments
            ));
        }
    }
    for (tool, arguments, _) in NO_CLI_TWIN {
        let pinned = refused.iter().any(|(id, _)| {
            calls.get(id).is_some_and(|(t, a)| t == tool && *a == parsed(arguments))
        });
        if !pinned {
            findings.push(format!(
                "NO_CLI_TWIN names `{tool}` {arguments}, which tools.errors.json does not refuse"
            ));
        }
    }
    assert!(findings.is_empty(), "the two doors' refusals: {}", report(&findings));
}
