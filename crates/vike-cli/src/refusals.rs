//! The refusals this program gives for a bad flag or flag value, and for a retired `config` word
//! ([`RetiredConfigSpelling`]), declared once (`vike_utility::refusal_enum!`, decision 0301): each
//! variant carries its ONE text and its id.
//!
//! The site that refuses builds the variant and hands its text on as `.rendered()` (`error [<id>]: <text>`,
//! the id a person looks up in the release's `refusals.json`), so what a person reads is this literal and its
//! id and nothing else; words a CARRIER forwards are the bare text (`.to_string()`), since the carrier shows
//! the id. `cli.json` publishes each one from here
//! (`crate::surface`'s `Refusal`): its id beside its template, every hole a row knows filled in.
//! Decide on the variant, never on its words.
//!
//! An id is `cli.<plane>.<flag>.<words>`, or `cli.<plane>.<words>` for a rule several flags share; it
//! is published, so it stays when the text is reworded. A CARRIER forwards words another surface owns
//! (`vike_runs::valid_mark_name`, the preset checks) under its own prefix; `cli.json` publishes those
//! words where they are raised, so a carrier has no row of its own.

use crate::cmd::backtest::read::where_expr::OP_HELP;

vike_utility::refusal_enum! {
    /// A refusal of a flag or a flag value: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FlagRefusal with ids FlagRefusalId {
        /// A flag of `ls` (it narrows, orders or shapes the listing) on another reading verb.
        OnlyOnLs { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_ls":
               "{flag} does not apply to `backtest {sub}` — \
                that flag narrows, orders or shapes a LISTING, and `ls` is the listing",
        /// A flag of `show` that selects a section of one run, on another reading verb.
        OnlyOnShow { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_show":
               "{flag} does not apply to `backtest {sub}` — \
                that flag selects a SECTION of one stored run, which is what `show` renders",
        /// `--html` on a reading verb other than `show`.
        HtmlOnlyOnShow { sub: &'static str }
            => "cli.backtest.html.only_on_show":
               "--html does not apply to `backtest {sub}` — it renders ONE stored run's whole tearsheet \
                as an HTML page, which is `show`'s answer beside its --metrics: `backtest show <run> \
                --html --out sheet.html`",
        /// `--metrics-list` on a reading verb other than `show`.
        MetricsListOnlyOnShow { sub: &'static str }
            => "cli.backtest.metrics_list.only_on_show":
               "--metrics-list does not apply to `backtest {sub}` — it prints the METRIC CATALOG (every \
                id this tree can measure, with what each one means), which is `show`'s answer to \
                \"what can I ask for\" beside its --metrics. It names no run and reads no runs \
                directory: `backtest show --metrics-list`",
        /// `--trades` on a reading verb other than `show` and `diff`.
        TradesOnlyOnShowOrDiff { sub: &'static str }
            => "cli.backtest.trades.only_on_show_or_diff":
               "--trades does not apply to `backtest {sub}` — it names the stored trade ledger, which \
                `show` renders and `diff` compares",
        /// A flag of `tag` (it writes onto a stored run) on another reading verb.
        OnlyOnTag { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_tag":
               "{flag} does not apply to `backtest {sub}` — \
                that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
        /// A flag of `gate` (the baseline or the criteria) on another reading verb.
        OnlyOnGate { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_gate":
               "{flag} does not apply to `backtest {sub}` — \
                that flag names the BASELINE or the criteria a verdict is computed from, and `gate` is \
                the verb whose product is that verdict",
        /// A flag of `diff` (it shapes a comparison) on another reading verb.
        OnlyOnDiff { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_diff":
               "{flag} does not apply to `backtest {sub}` — \
                that flag shapes a two-run COMPARISON, which is what `diff` renders",
        /// A flag of `params` (the strategy file it lists) on another reading verb.
        OnlyOnParams { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_params":
               "{flag} does not apply to `backtest {sub}` — \
                that flag names the strategy file whose keys `params` lists",
        /// `--script` on a reading verb other than `params` and `script-check`.
        ScriptOnlyOnParamsOrScriptCheck { sub: &'static str }
            => "cli.backtest.script.only_on_params_or_script_check":
               "--script does not apply to `backtest {sub}` — it names a .rhai FILE, whose knobs \
                `params` lists and which `script-check` compiles",
        /// A flag of `templates` (it saves a starter) on another reading verb.
        OnlyOnTemplates { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_templates":
               "{flag} does not apply to `backtest {sub}` — \
                that flag SAVES a shipped starter to a new file, and `templates` is the verb that \
                emits one",
        /// A flag of `prune` on another reading verb.
        OnlyOnPrune { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_prune":
               "{flag} does not apply to `backtest {sub}` — \
                that flag lists what the saved-run retention would remove, which is what `prune` does",
        /// A flag of `strategies` (the backtesthub it dials) on another reading verb.
        OnlyOnStrategies { flag: &'static str, sub: &'static str }
            => "cli.backtest.only_on_strategies":
               "{flag} does not apply to `backtest {sub}` — \
                that flag names the backtesthub, and `strategies` is the one reading verb that \
                dials one — every other reads the run directory on this machine",
        /// `--out` on a reading verb that prints a line rather than a rendered document.
        OutNotARendering { sub: &'static str }
            => "cli.backtest.out.not_a_rendering":
               "--out names a FILE to write a rendered document to, and `backtest {sub}` prints a line \
                the shell can already redirect",
        /// `--out` on `templates`, whose starter is saved with `--write-strategy`.
        OutOnTemplates
            => "cli.backtest.out.on_templates":
               "--out names a FILE to write a rendered document to, and `backtest templates` prints a line \
                the shell can already redirect — a starter is SAVED with --write-strategy, which refuses an existing path rather \
                than overwriting one",
        /// `prune` without `--dry-run`: it only lists, and says so.
        PruneWithoutDryRun
            => "cli.backtest.dry_run.required_on_prune":
               "`backtest prune` only LISTS what the saved-run retention would remove, and says so \
                with --dry-run: `backtest prune --dry-run`. The backtesthub removes those runs itself, \
                after its next save, when config.saved_runs_per_strategy is set.",
        /// A hidden flag of the reading verbs that none of them takes.
        UnknownOption { flag: String }
            => "cli.backtest.unknown_option":
               "unknown option '{flag}'",
        /// A flag of `run` typed on `params`, which builds no profile.
        RunFlagOnParams { flag: String }
            => "cli.backtest.run_flag_on_params":
               "{flag} does not apply to `params`, which reads the script or the strategy file on this \
                machine and lists its knobs — no server, no store, no engine. It belongs to `vike-cli \
                backtest run`",
        /// `--attribution`: the engine computes no attribution.
        AttributionUnbuilt
            => "cli.backtest.attribution.unbuilt":
               "--attribution is not available yet — it needs \
                a per-source attribution the engine does not compute at all. What `show` can render today: --metrics, \
                --trades, --config, --export trades|equity, --json, --out.",
        /// `--breakdown`: this guard reads argv alone, so it cannot tell whether the run was thinned.
        BreakdownUnbuilt
            => "cli.backtest.breakdown.unbuilt":
               "--breakdown is not available yet — it needs \
                the PER-BAR RETURNS, and a run to test the reason against. The stored equity curve is \
                DECIMATED once a run exceeds the sample cap (series.json's `stride` above 1 means \
                samples were dropped), so a period breakdown computed from it would disagree with \
                report.json's own numbers — but this guard reads the command line alone, before any \
                run is resolved, so it cannot tell whether YOUR run was thinned. `vike-cli report \
                run:<run> --breakdown day|month` computes the breakdown from the same series.json and \
                refuses only the runs that really were. What `show` can render today: --metrics, \
                --trades, --config, --export trades|equity, --json, --out.",
        /// `--drawdowns`: the renderer exists, the wiring on this verb does not.
        DrawdownsUnbuilt
            => "cli.backtest.drawdowns.unbuilt":
               "--drawdowns is not available yet — it needs \
                its WIRING here, not a renderer: `vike_analytics::periods::drawdown_table` folds the \
                episodes and `drawdown_table_text` renders the table, both linked into this binary, \
                and the EQUITY CURVE they read is in the run record (series.json). What is missing is \
                a `--drawdowns N` value on this verb and a call to them. `--export equity` hands you \
                the curve today, and `vike-cli report run:<run>` recomputes its single WORST drawdown. What `show` can render today: --metrics, \
                --trades, --config, --export trades|equity, --json, --out.",
        /// `--html` beside a flag that selects or exports part of the run.
        HtmlBesideASection { flag: &'static str }
            => "cli.backtest.html.beside_a_section":
               "--html and {flag} ask for two different documents: --html renders this run's whole \
                tearsheet as one HTML page, and {flag} selects or exports part of it. Which one \
                wins would be a guess, so neither does — run `backtest show <run> --html --out \
                sheet.html`, then `backtest show <run> {flag}`.",
        /// `--html` beside `--json`: two serializations of one run.
        HtmlBesideJson
            => "cli.backtest.html.beside_json":
               "--html and --json are two SERIALIZATIONS of the same run, and this verb writes one \
                document: --html is the rendered tearsheet a human opens, --json is the machine \
                shape (`{{run_id, dir, manifest, report}}`). Run it twice, or take the JSON and \
                render it elsewhere — every number in the HTML comes from the same report.json \
                --json prints.",
        /// `--metrics-list` beside a flag that renders part of a run.
        MetricsListBesideARunRendering { flag: &'static str }
            => "cli.backtest.metrics_list.beside_a_run_rendering":
               "--metrics-list and {flag} ask for two different documents: the first lists what this \
                tree CAN measure (the catalog, with no run involved at all), the second renders part \
                of one stored run. Which of the two wins would be a guess, so neither does — run \
                `backtest show --metrics-list` on its own, then `backtest show <run> {flag}`.",
        /// `--metrics-list` beside `--json`: the catalog has no JSON rendering.
        MetricsListBesideJson
            => "cli.backtest.metrics_list.beside_json":
               "--metrics-list has no --json rendering: it is an aligned table for a human, and \
                nothing in this tree serializes the metric catalog. For the machine-readable answer \
                use the published surface asset — `vike-cli surface --out DIR` writes cli.json, whose \
                `rosters` carry the metric ids. `backtest show --metrics-list` prints the table.",
        /// `--export` given a comma list.
        ExportList { value: String }
            => "cli.backtest.export.one_value_at_a_time":
               "--export '{value}': one value at a time. Each export is a single document and `--out` \
                names a single file, so a list would either be truncated to its first value or written \
                as two documents nothing can parse. Run `show` once per value.",
        /// `--export fills`: fills are not stored.
        ExportFills
            => "cli.backtest.export.fills_not_stored":
               "--export fills is not available — fills are not stored at ANY size. A backtest \
                computes them, consumes them into report.json's scalars and drops them; only the trade \
                LEDGER and the equity curve survive. `--export trades` and `--export equity` both work \
                today.",
        /// `--export` given a value this verb does not know.
        ExportUnknownValue { other: String, list: String }
            => "cli.backtest.export.unknown_value":
               "--export '{other}' is not a value this verb knows — §6.2's set is trades, equity and \
                fills, of which {list} are stored (fills are not: see `--export fills`)",
        /// `--export trades` on a run whose producer wrote no trade ledger.
        ExportNoTradeLedger { run_id: String, file: &'static str }
            => "cli.backtest.export.no_trade_ledger":
               "{run_id} wrote no {file} — this producer keeps no trade ledger (a study, or a run minted \
                before the run artifact existed)",
        /// `--export equity` on a run whose producer wrote no equity curve.
        ExportNoEquityCurve { run_id: String, file: &'static str }
            => "cli.backtest.export.no_equity_curve":
               "{run_id} wrote no {file} — this producer keeps no equity curve",
        /// The `--out` file cannot be written.
        OutCannotWrite { file: String, io_error: String }
            => "cli.backtest.out.cannot_write":
               "cannot write {file}: {io_error}",
        /// `--cols` naming no column.
        ColsNoNames
            => "cli.backtest.cols.no_names":
               "--cols was given no column names",
        /// `--limit` that is not a whole number.
        LimitNotAWholeNumber { n: String }
            => "cli.backtest.limit.not_a_whole_number":
               "--limit '{n}' is not a whole number of rows",
        /// `--sort` with a direction other than asc or desc.
        SortDirection { other: String }
            => "cli.backtest.sort.bad_direction":
               "--sort direction '{other}' is not one of: asc | desc",
        /// `--sort` on a field no stored run carries.
        SortNoRunCarriesTheField { key: String }
            => "cli.backtest.sort.no_run_carries_the_field":
               "--sort '{key}': no run carries that field. Manifest fields are run_id, kind, \
                produced_by, started_at, finished_at, git_sha, config.path, config.name, report and \
                detail.<path>; any other name is read as a top-level key of report.json.",
        /// `--where` with a `|`: the grammar has no OR.
        WhereHasNoOr { expr: String }
            => "cli.backtest.where.no_or":
               "--where '{expr}': this grammar has no OR — terms are joined by a comma, which means \
                AND (`kind=backtest,sharpe>1`). Run `ls` twice for a disjunction.",
        /// `--where` with an empty term.
        WhereEmptyTerm { expr: String }
            => "cli.backtest.where.empty_term":
               "--where '{expr}': an empty term — terms are joined by a single comma \
                (FIELD OP VALUE, with OP one of: {OP_HELP})",
        /// `--where` with a term that has no operator.
        WhereNoOperator { term: String }
            => "cli.backtest.where.no_operator":
               "--where '{term}': no operator — a term is FIELD OP VALUE, with OP one of: \
                {OP_HELP}",
        /// `--where` with a term that has no field before its operator.
        WhereNoField { term: String, op: &'static str }
            => "cli.backtest.where.no_field":
               "--where '{term}': no field before '{op}' — a term is FIELD OP VALUE, with OP one \
                of: {OP_HELP}",
        /// `--fail-if` with no criterion.
        FailIfEmpty
            => "cli.backtest.fail_if.empty":
               "--fail-if takes at least one criterion, e.g. `sharpe:-5%,max_dd:+10%` — METRIC, a \
                colon, a SIGN saying which direction is bad, the tolerance, and an optional `%`",
        /// `--fail-if` with an empty term between two commas.
        FailIfEmptyCriterion
            => "cli.backtest.fail_if.empty_criterion":
               "--fail-if: an empty criterion — terms are joined by a single comma, e.g. \
                `sharpe:-5%,max_dd:+10%`",
        /// A `--fail-if` criterion with no colon.
        FailIfNoColon { term: String }
            => "cli.backtest.fail_if.no_colon":
               "--fail-if `{term}`: a criterion is METRIC:SIGN NUMBER[%], e.g. `sharpe:-5%` — the \
                colon is missing",
        /// A `--fail-if` criterion with nothing before its colon.
        FailIfNoMetric { term: String }
            => "cli.backtest.fail_if.no_metric":
               "--fail-if `{term}`: the metric before the colon is empty",
        /// A `--fail-if` tolerance with no sign.
        FailIfNoSign { term: String, metric: String }
            => "cli.backtest.fail_if.no_sign":
               "--fail-if `{term}`: the tolerance needs a SIGN saying which direction is bad — \
                `-` fails on a FALL (`{metric}:-5%`), `+` fails on a RISE (`{metric}:+5%`). \
                Without one this gate would check a side of the number you did not choose.",
        /// A `--fail-if` tolerance that is not a number.
        FailIfNotANumber { term: String, number: String }
            => "cli.backtest.fail_if.not_a_number":
               "--fail-if `{term}`: `{number}` is not a number — the tolerance is the MAGNITUDE",
        /// A `--fail-if` tolerance that is not finite.
        FailIfNotFinite { term: String, number: String }
            => "cli.backtest.fail_if.not_finite":
               "--fail-if `{term}`: `{number}` is not a finite tolerance",
        /// A `--fail-if` tolerance written negative.
        FailIfNegative { term: String, metric: String }
            => "cli.backtest.fail_if.negative":
               "--fail-if `{term}`: the tolerance is negative — the DIRECTION is the sign's job and \
                the number is the magnitude, so `{metric}:-5%` is what `{term}` was probably meant to \
                be",
        /// Two `--fail-if` criteria judging one report key.
        FailIfMetricTwice { prev: String, c: String, key: String }
            => "cli.backtest.fail_if.metric_twice":
               "`{prev}` and `{c}` both judge `{key}` — name each metric once; a silently-dropped \
                criterion is a gate checking something other than what it was given",
        /// `gate` without `--against`.
        AgainstRequired
            => "cli.backtest.against.required":
               "`backtest gate` needs `--against <run|mark>` — every criterion is RELATIVE to a \
                baseline, so there is nothing to judge without one. Make a stable baseline with \
                `vike-cli backtest tag <run> --as baseline/<name>`, then gate `--against \
                @baseline/<name>`. (For an ABSOLUTE threshold this grammar cannot spell, pipe \
                `vike-cli backtest show <run> --json` through `jq`.)",
        /// `gate` without `--fail-if`.
        FailIfRequired
            => "cli.backtest.fail_if.required":
               "`backtest gate` needs `--fail-if EXPR` — a gate with no criteria would exit 0 having \
                checked nothing, which is the one answer a CI step must never get. For example: \
                `--fail-if 'sharpe:-5%,max_dd:+10%'`.",
        /// `tag` with none of `--add`, `--note` and `--as`.
        TagWritesNothing { selector: String }
            => "cli.backtest.tag.writes_nothing":
               "`backtest tag {selector}` would write nothing — give it at least one of `--add TAG` (a label \
                on the run), `--note TEXT` (a note beside it) or `--as NAME` (a MARK, the stable name \
                `gate --against` takes). A tag command that changed nothing and exited 0 would be \
                positive confirmation of something that did not happen.",
        /// CARRIER: an `--as` name `vike_runs::valid_mark_name` refuses, in that function's words.
        MarkName { why: String }
            => "cli.backtest.as.bad_mark_name":
               "--as {why}",
        /// `--list-params` was a MODE FLAG on `backtest` for months. Decision 11 gives it a sub-verb, and
        /// the old spelling answers with the new one rather than with "unknown argument": the same reason
        /// `crate::RETIRED_COMMANDS` exists one level up.
        ListParamsRetired
            => "cli.backtest.list_params.retired":
               "--list-params is now a subcommand: `vike-cli backtest params --script <s.rhai>`. A discovery \
                runs no backtest at all, and decision 11 of the backtest-CLI-surface design gives every \
                action inside this plane exactly one spelling.",
        /// `--fee`: commission is a strategy parameter now.
        FeeRetired { units: String }
            => "cli.backtest.fee.retired":
               "--fee is no longer read: commission is a strategy parameter now. Set --param \
                commission=<amount> with --param commission_unit={units} (percent is of the fill's value: 0.05 \
                is 0.05%, so a fee fraction f is commission = 100 x f), or --set \
                strategy.params.commission=<amount>; left out, the strategy's own commission applies, else \
                none",
        /// `--slippage`: slippage is a strategy parameter now.
        SlippageRetired { units: String }
            => "cli.backtest.slippage.retired":
               "--slippage is no longer read: slippage is a strategy parameter now. Set --param \
                slippage=<amount> with --param slippage_unit={units} (percent is of the price: 0.05 is 0.05%; \
                ticks also need --param slippage_tick=<the price of one tick>), or --set \
                strategy.params.slippage=<amount>; left out, the strategy's own slippage applies, else none",
        /// `--set` with no `=`.
        SetNotKeyValue { pair: String }
            => "cli.backtest.set.not_key_value":
               "--set takes key=value, got {pair:?} (e.g. --set engine.cash=10000)",
        /// `--set` whose first segment is not a profile table.
        SetUnknownTable { key: String, head: String, tables: String }
            => "cli.backtest.set.unknown_table":
               "--set {key}: a profile has no `{head}` table — the top-level keys are {tables}",
        /// `--symbol` with an empty element.
        SymbolEmptyElement { raw: String }
            => "cli.backtest.symbol.empty_element":
               "--symbol takes SYM[,SYM…] with no empty element, got {raw:?}",
        /// `--interval` that is not a digit count and one unit.
        IntervalInvalid { v: String }
            => "cli.backtest.interval.invalid":
               "--interval {v:?} is not a valid interval — a digit count and one \
                unit of s|m|h|d (e.g. 30s, 5m, 1h, 1d)",
        /// `--kind` or `--decide` given a value outside its roster.
        ValueNotInRoster { flag: &'static str, roster: String, value: String }
            => "cli.backtest.value_not_in_roster":
               "{flag} must be {roster}, got {value:?}",
        /// `--param` with no `=`.
        ParamNotKeyValue { pair: String }
            => "cli.backtest.param.not_key_value":
               "--param takes k=v, got {pair:?} (e.g. --param size=1.5)",
        /// `--param` with an empty key.
        ParamEmptyKey { pair: String }
            => "cli.backtest.param.empty_key":
               "--param takes k=v with a non-empty key, got {pair:?} (e.g. --param size=1.5)",
        /// `--param` whose key holds a dot.
        ParamNestedKey { key: String }
            => "cli.backtest.param.nested_key":
               "--param {key}: [strategy.params] is a FLAT knob table — use \
                --set strategy.params.{key}=… if you really mean a nested key",
        /// `--param` whose value is a search range.
        ParamRange { key: String, raw: String }
            => "cli.backtest.param.range":
               "--param {key}={raw}: a RANGE declares a search axis, which this command \
                cannot build yet — declare it as a [paramscan] table in a --profile, or pass \
                a single value. (--set strategy.params.{key}={raw} sets it as a literal \
                value if that is what you meant.)",
        /// `run` with no `--profile` and no flag that builds one.
        NothingToRun
            => "cli.backtest.profile.nothing_to_run":
               "nothing to run: pass --profile <run.toml>, or build one from flags\n       \
                e.g. --venue binance --symbol BTCUSDT --from 2026-01-01T00 --to 2026-02-01T00 \
                --strategy trend/buy_hold.rs --cash 10000\n       any profile key is reachable as \
                --set <table>.<key>=<value>",
        /// `--addr` beside `--local`.
        AddrBesideLocal
            => "cli.backtest.addr.beside_local":
               "--addr names a remote backtest daemon, so it cannot be combined with --local\n\
                drop one: --local runs the engine on this machine, --addr ships the \
                profile to a server",
        /// `--engine` on a remote run.
        EngineWithoutLocal
            => "cli.backtest.engine.without_local":
               "--engine applies to --local only — a remote run reads the SERVER's store",
        /// `--max-gap` with a blank value.
        MaxGapBlank
            => "cli.data.max_gap.blank":
               "--max-gap requires a duration, e.g. --max-gap 1d (or 4h, 900000)",
        /// `--on-gap` outside its roster.
        OnGapNotAMember { value: String, roster: String }
            => "cli.data.on_gap.not_a_member":
               "--on-gap {value:?} is not one of {roster}. It says what --require-coverage DOES about a \
                window the store does not cover: refuse stops the run (the default), warn logs the \
                findings and runs unchanged, run leaves the gate inert",
        /// `--universe` outside its roster.
        UniverseNotAMember { value: String, roster: String }
            => "cli.data.universe.not_a_member":
               "--universe {value:?} is not one of {roster}. declared takes the symbol list verbatim (the \
                default), covered names every member whose tape does not span the window and runs \
                anyway, strict refuses that run. None of the three drops a member — a universe a run \
                narrowed silently would not be the one your profile names",
        /// `--local` cannot run a walk-forward, and the reason is a property of the ENGINE rather than of
        /// this verb: `crates/vike-backtesthub/src/backtest_cli.rs` has one profile path, branching on
        /// `ProfileBacktest::is_paramscan` and nothing else, so NEITHER driver
        /// (`vike_backtest::walkforward::runner::run_walkforward` or its optimizing sibling) is reachable from any
        /// binary. There is nothing to spawn. `crates/vike-cli/src/cmd/backtest/render/walkforward.rs`'s module doc
        /// carries the condition that would change it.
        LocalHasNoWalkforward
            => "cli.backtest.local.no_walkforward":
               "--local cannot run a walk-forward: this profile declares a [walkforward] table, and the \
                standalone engine has ONE profile path — it branches on the [paramscan] grid and nothing else, so \
                neither walk-forward driver is reachable from any binary and there is nothing local to \
                spawn.\n\
                Drop --local to run it on the backtesthub, or drop the [walkforward] table to backtest \
                this profile here.",
        /// `--local` cannot run a permutation test: the standalone engine has no permutation arm, as it has
        /// no walk-forward arm (`LocalHasNoWalkforward`).
        LocalHasNoPermutation
            => "cli.backtest.local.no_permutation":
               "--local cannot run a permutation test: this profile declares a [permutation] table, and the \
                standalone engine has no permutation arm, so there is nothing local to spawn.\n\
                Drop --local to run it on the backtesthub, or drop the [permutation] table to backtest this \
                profile here.",
        /// `--rank-by` or `--optimizer` on a walk-forward run.
        FlagOnWalkforward { flag: &'static str }
            => "cli.backtest.walkforward_ignores_flag":
               "{flag} configures nothing on a walk-forward run: the wire verb it goes over \
                carries the profile TOML and nothing else. A window's ranking is \
                [walkforward].rank_by, in the profile, read by the one parser that runs it — and \
                what each window searches is that same [walkforward] table's business, never a \
                flag on this side.",
        /// `--rank-by` or `--optimizer` on a permutation test.
        FlagOnPermutation { flag: &'static str }
            => "cli.backtest.permutation_ignores_flag":
               "{flag} configures nothing on a permutation test: the wire verb it goes over \
                carries the profile TOML and nothing else. A history's score is \
                [permutation].rank_by and whether every history re-runs the grid is \
                [permutation].train, in the profile, read by the one parser that runs it.",
        /// `--local` with a profile to stage and no project to stage it in.
        NoProjectToStage { staging_reason: String }
            => "cli.backtest.local.no_project_to_stage":
               "--local runs the engine on this machine from a profile FILE, and there is no \
                project above the working directory to stage one in.\n{staging_reason}\nRun inside your \
                project (or set $VIKE_SETTINGS_DIR), pass an already-merged profile to \
                --profile, or write one first with --write-profile <path>",
        /// The `--preset` file cannot be read.
        PresetUnreadable { preset_path: String, e: String }
            => "cli.backtest.preset.unreadable":
               "cannot read preset {preset_path}: {e}",
        /// CARRIER: a `--preset` refused by `merge_preset_params`, prefixed with its path.
        PresetRefused { preset_path: String, why: String }
            => "cli.backtest.preset.refused":
               "preset {preset_path}: {why}",
        /// The `--profile` file cannot be read.
        ProfileUnreadable { path: String, e: String }
            => "cli.backtest.profile.unreadable":
               "cannot read profile {path}: {e}",
        /// `--write-profile` naming a path that exists.
        WriteProfileExists { path: String }
            => "cli.backtest.write_profile.exists":
               "--write-profile {path} already exists, and this command will not overwrite a \
                profile — delete it, or name a different path",
        /// The `--write-profile` file cannot be written.
        WriteProfileFailed { path: String, e: String }
            => "cli.backtest.write_profile.failed":
               "cannot write the profile to {path}: {e}",
        /// A spawned tool (the engine, the store tool, the tradehub) cannot be started.
        CannotSpawn { title: &'static str, engine: String, e: String, missing_hint: &'static str }
            => "cli.engine.cannot_spawn":
               "cannot run {title} ({engine}): {e}\n{missing_hint}",
        /// A `--preset` that is not TOML (shown under `PresetRefused`).
        PresetNotToml { e: String }
            => "cli.backtest.preset.not_toml":
               "not valid TOML: {e}",
        /// A `--preset` that is not a table (shown under `PresetRefused`).
        PresetNotATable
            => "cli.backtest.preset.not_a_table":
               "a preset must be a table of parameters",
        /// A `--preset` table that is not strategy parameters (shown under `PresetRefused`).
        PresetNotParameters { e: String }
            => "cli.backtest.preset.not_parameters":
               "not a table of strategy parameters: {e}",
        /// A `--profile` that is not TOML.
        ProfileNotToml { e: String }
            => "cli.backtest.profile.not_toml":
               "profile is not valid TOML: {e}",
        /// A `--set` key with an empty segment.
        SetEmptySegment { dotted_key: String }
            => "cli.backtest.set.empty_segment":
               "--set key {dotted_key:?} has an empty segment — a key is spelled `table.key` (or \
                `table.sub.key`), with no leading, trailing or doubled dot",
        /// A profile whose root is not a table.
        ProfileRootNotATable
            => "cli.backtest.set.root_not_a_table":
               "profile root is not a TOML table",
        /// A `--set` key that runs through a plain value.
        SetThroughAPlainValue { prefix: String, dotted_key: String }
            => "cli.backtest.set.through_a_plain_value":
               "`{prefix}` is a plain value in the profile, so `{dotted_key}` cannot nest under it",
        /// A `--set` key naming a whole table.
        SetNamesATable { dotted_key: String }
            => "cli.backtest.set.names_a_table":
               "`{dotted_key}` names a whole table in the profile, not a single value — set its \
                leaves instead (`{dotted_key}.<key>`)",
        /// `params` with both `--script` and `--strategy`.
        ScriptAndStrategy
            => "cli.backtest.params.two_sources":
               "--script and --strategy name two different sources for the same question — give \
                exactly one",
        /// `params` with neither `--script` nor `--strategy`.
        ParamsNeedsASource
            => "cli.backtest.params.no_source":
               "`backtest params` needs a source: --script <s.rhai> for a script's param() knobs, or \
                --strategy <file> for a strategy file's declared keys",
        /// The `--script` file cannot be read.
        ScriptUnreadable { path: String, io_error: String }
            => "cli.backtest.script.unreadable":
               "cannot read script {path}: {io_error}",
        /// The `--script` file does not compile.
        ScriptDoesNotCompile { err: String }
            => "cli.backtest.script.does_not_compile":
               "rhai compile error: {err}",
        /// `--strategy` with no project above the working directory.
        StrategyWithoutProject
            => "cli.backtest.strategy.no_project":
               "--strategy names a file of the project's strategies/ folder, and there is no project \
                above the working directory",
        /// The `--strategy` file cannot be read.
        StrategyUnreadable { path: String, io_error: String }
            => "cli.backtest.strategy.unreadable":
               "cannot read {path}: {io_error}",
        /// `--write-strategy` with no starter id.
        WriteStrategyNeedsAStarter { path: String, roster: String }
            => "cli.backtest.write_strategy.needs_a_starter":
               "--write-strategy {path} needs a starter to write — name one: `vike-cli backtest \
                templates <id> --write-strategy {path}`. The ids are: {roster}",
        /// `--write-strategy` naming a path that exists.
        WriteStrategyExists { path: String }
            => "cli.backtest.write_strategy.exists":
               "--write-strategy {path} already exists, and this command will not overwrite a \
                strategy — delete it, or name a different path",
        /// The `--write-strategy` file cannot be written.
        WriteStrategyFailed { path: String, io_error: String }
            => "cli.backtest.write_strategy.failed":
               "cannot write the strategy to {path}: {io_error}",
    }
}

vike_utility::refusal_enum! {
    /// A spelling this program USED to read, renamed in stage 5 of the CLI naming plan: each one
    /// answers with the spelling that replaced it, never as an unknown word and never as an alias.
    /// The account table moved out of `secrets` into its own `account` group (the owner's ruling 2).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RetiredSpelling with ids RetiredSpellingId {
        /// `secrets list`.
        SecretsList => "cli.secrets.list.retired": "`secrets list` is now `secrets ls`",
        /// `secrets delete`.
        SecretsDelete => "cli.secrets.delete.retired": "`secrets delete` is now `secrets rm`",
        /// `secrets accounts`.
        SecretsAccounts
            => "cli.secrets.accounts.moved":
               "`secrets accounts` is now `account ls`: the account table has its own group \
                (`vike-cli account --help`)",
        /// `secrets account ACTION`.
        SecretsAccount
            => "cli.secrets.account.moved":
               "`secrets account ACTION` is now `account ACTION` (`secrets account add` is `account \
                add`, and `remove` is `account rm`): the account table has its own group \
                (`vike-cli account --help`)",
        /// `secrets set-book`.
        SecretsSetBook
            => "cli.secrets.set_book.moved":
               "`secrets set-book` is now `account set-book`: the account table has its own group \
                (`vike-cli account --help`)",
        /// `secrets confirm`.
        SecretsConfirm
            => "cli.secrets.confirm.moved":
               "`secrets confirm` is now `account confirm`: the account table has its own group \
                (`vike-cli account --help`)",
        /// `secrets set --account ID`: the row is named by `--id`, as on every account verb.
        SecretsSetAccountFlag
            => "cli.secrets.set.account.retired":
               "`secrets set --account ID` is now `secrets set --id ID`: one flag names an account \
                row on every verb. What you typed is deliberately not quoted back here.",
        /// `account remove`.
        AccountRemove => "cli.account.remove.retired": "`account remove` is now `account rm`",
        /// `access list`.
        AccessList => "cli.access.list.retired": "`access list` is now `access ls`",
    }
}

vike_utility::refusal_enum! {
    /// An old spelling of a `trade` word (a group, a verb, a flag, a REPL word), refused with the
    /// spelling that replaced it: no old spelling is kept as an alias (root `CLAUDE.md`'s one-name
    /// rule), and none is answered as "unknown". `crate::cmd::trade::plane`'s `renamed` shows it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RetiredTradeSpelling with ids RetiredTradeSpellingId {
        /// `was` is what was typed, `now` the spelling that does that job now.
        Renamed { was: String, now: String }
            => "cli.trade.renamed":
               "`{was}` is now `{now}`: use the new spelling",
    }
}

vike_utility::refusal_enum! {
    /// A `config` word this program retired for another. A symbol has one name (root `CLAUDE.md`), so the old
    /// spelling is no alias: it refuses, naming the word that replaced it, and does nothing else. The profile
    /// verbs are one family, `config profile …` (the naming audit's G3).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RetiredConfigSpelling with ids RetiredConfigSpellingId {
        /// `config url reset`: one word for "back to the default", as `config unset` says it.
        UrlReset
            => "cli.config.url.reset.retired":
               "`config url reset` is now `config url unset`",
        /// `config url check`: it checks nothing, it prints what the gatewayhub's probe found.
        UrlCheck
            => "cli.config.url.check.retired":
               "`config url check` is now `config url probes`: it prints what the gatewayhub's probe \
                last found and dials nothing",
        /// `config ibkr-cash-settled-roots list`: every other listing verb is `ls`.
        IbkrCashSettledRootsList
            => "cli.config.ibkr_cash_settled_roots.list.retired":
               "`config ibkr-cash-settled-roots list` is now `config ibkr-cash-settled-roots ls`",
        /// `config ibkr-cash-settled-roots remove`: every other removal verb is `rm`.
        IbkrCashSettledRootsRemove
            => "cli.config.ibkr_cash_settled_roots.remove.retired":
               "`config ibkr-cash-settled-roots remove` is now `config ibkr-cash-settled-roots rm`",
        /// `config recorder`: the recorder profile listing.
        Recorder
            => "cli.config.recorder.retired":
               "`config recorder` is now `config profile ls recorder`",
        /// `config activate`.
        Activate
            => "cli.config.activate.retired":
               "`config activate` is now `config profile activate`",
        /// `config deactivate`.
        Deactivate
            => "cli.config.deactivate.retired":
               "`config deactivate` is now `config profile deactivate`",
        /// `config delete-profile`: every other removal verb is `rm`.
        DeleteProfile
            => "cli.config.delete_profile.retired":
               "`config delete-profile` is now `config profile rm`",
        /// `config bootstrap-daemon`.
        BootstrapDaemon
            => "cli.config.bootstrap_daemon.retired":
               "`config bootstrap-daemon` is now `config profile create daemon`, and its \
                `--strategy-file` is `--strategy`",
        /// `config bootstrap-recorder`.
        BootstrapRecorder
            => "cli.config.bootstrap_recorder.retired":
               "`config bootstrap-recorder` is now `config profile create recorder`",
        /// `config bootstrap-run`.
        BootstrapRun
            => "cli.config.bootstrap_run.retired":
               "`config bootstrap-run` is now `config profile create run`",
        /// `config set-mount-param`.
        SetMountParam
            => "cli.config.set_mount_param.retired":
               "`config set-mount-param` is now `config profile mount set-param`",
        /// `config set-mount-max-leverage`.
        SetMountMaxLeverage
            => "cli.config.set_mount_max_leverage.retired":
               "`config set-mount-max-leverage` is now `config profile mount set-max-leverage`",
        /// `config delete-operator-instrument`: every other removal verb is `rm`.
        DeleteOperatorInstrument
            => "cli.config.delete_operator_instrument.retired":
               "`config delete-operator-instrument` is now `config operator-instrument rm`",
        /// `config rename-symbol --account ID`: an account row is named by `--id`, as on every account verb.
        RenameSymbolAccount
            => "cli.config.rename_symbol.account.retired":
               "`config rename-symbol --account ID` is now `config rename-symbol --id ID`",
        /// `config profile create daemon --strategy-file`: the strategy file is `--strategy` on every verb.
        StrategyFile
            => "cli.config.profile.create.daemon.strategy_file.retired":
               "`--strategy-file` is now `--strategy`: `config profile create daemon <name> --strategy \
                <path>`",
    }
}

vike_utility::refusal_enum! {
    /// A `data` word or flag this program retired for another. A symbol has one name (root `CLAUDE.md`), so the
    /// old spelling is no alias: it refuses, naming the one that replaced it, and does nothing else.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RetiredDataSpelling with ids RetiredDataSpellingId {
        /// `data hist`: the group is `history` now (no abbreviations), its verbs and flags unchanged.
        HistGroup
            => "cli.data.hist.retired":
               "`data hist` is now `data history`, with the same verbs: `vike-cli data history --help` \
                lists them",
        /// `--source` naming a store lane on `rm`, `export`, `get` or `rename`: the lane is `--lane` now.
        SourceAsLane { verb: &'static str }
            => "cli.data.source.lane_retired":
               "--source on `data history {verb}` is now --lane: --source names only where a `fetch` gets \
                its rows, and --lane names a `source=` lane of the store (`recorder`, `aggtrades`, \
                `user-<label>`, as `data history ls` lists them)",
        /// `--label` on a csv-bars import: the lane it writes is named whole by `--lane` now.
        ImportLabel
            => "cli.data.label.retired":
               "--label is now --lane, which names the whole lane a csv-bars import writes: `--lane \
                user-<label>`, the same lane `get --lane` and `export --lane` read back",
        /// `--class` on `data history ls`, `data catalog` or `data instruments add`: `--asset-class` now.
        Class { verb: &'static str }
            => "cli.data.class.retired":
               "--class on `data {verb}` is now --asset-class, the one spelling of an instrument's asset \
                class",
    }
}

#[path = "refusals_tests.rs"]
#[cfg(test)]
mod refusals_tests;
