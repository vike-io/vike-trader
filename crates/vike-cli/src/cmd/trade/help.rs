//! The REPL's terse verb reference — the `help` verb, and the block `vike-cli trade --help` prints
//! after the plane's own roster.

use std::fmt::Write as _;

use vike_client::verbs;

/// Print the terse verb reference (the `help` verb).
pub(super) fn print_verb_help() {
    print!("{}", verb_reference());
}

/// The terse verb reference as text, one `\n`-terminated line per row. `vike-cli trade --help`
/// carries it as the plane's `after_help` and the REPL's `help` verb writes the same text, so the two cannot
/// say different things about the same prompt.
pub(super) fn verb_reference() -> String {
    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let mut line = |text: std::fmt::Arguments<'_>| {
        let _ = writeln!(out, "{text}");
    };
    line(format_args!("commands:"));
    line(format_args!("  WRITE (preview + confirm):"));
    line(format_args!(
        "    submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] \
         [--order-client-id <id>] [--type stop|stop-limit|take-profit --trigger <price> | --type \
         trailing|trailing-limit --trail <price>|--trail-percent <p> [--offset <price>] [--trigger \
         <activation>] | --type trailing-take-profit --trail <price>|--trail-percent <p> [--offset \
         <price>] | --type market-to-limit | --type pegged --peg best|mid|market|primary [--offset \
         <price>]] [--adjust-at <price> --adjust-to stop|stop-limit|trailing --adjust-stop <price> \
         [--adjust-limit <price> | --adjust-trail <price> | --adjust-trail-percent <p>]] \
         [--tif day|gtc|ioc|fok|gtd|at-open|at-close [--expires <when>] | \
         --post-only] [--display <qty>] [--all-or-none] [--sweep-to-fill] [--discretion <amount>] \
         [--at <when> [--when-price above|below:[<symbol>:]<price> [--when-price-on <venue>]]]"
    ));
    line(format_args!(
        "    buy|sell <venue> <symbol> <qty> [@<price>|@market] [--reduce-only] [--order-client-id <id>] \
         [--type … --trigger <price>] [--tif … | --post-only] [--display …] [--all-or-none …] \
         [--adjust-at …] [--at …]"
    ));
    line(format_args!(
        "      (an order_client_id is MINTED per order and shown in the preview; --order-client-id pins one)"
    ));
    line(format_args!(
        "      (<venue>/<LABEL> in place of <venue> names the ACCOUNT; a bare <venue> is taken only \
         where the node has ONE account of it, running or not; --type and --trigger come together, stop-limit \
         also takes @<price>, and no flag of submit, modify or mount may follow --reason)"
    ));
    line(format_args!(
        "      (a limit order's lifetime: --tif, with --expires for gtd, or --post-only, a maker-only \
         order the venue REFUSES rather than execute as a taker; a venue that cannot do it refuses \
         it by name; none may follow --reason)"
    ));
    // STATE THE CHARSET HERE. `submit --order-client-id` refuses a bad one with a good message, but a rejection
    // is the wrong place to learn a constraint the help screen could have stated — and `cancel`,
    // which takes the same field, only WARNS (it must; see `verbs::order_client_id_charset_warning`), so the
    // help is the one surface that can state the rule once for every verb that takes an order client id.
    line(format_args!("      <id> is {}", verbs::ORDER_CLIENT_ID_CHARSET));
    line(format_args!(
        "    cancel <order client id>                (fire-and-forget: the node cannot confirm a match)"
    ));
    line(format_args!("    modify <order client id> [--qty Q] [--price P] [--trigger P]"));
    line(format_args!(
        "    oco <venue> <symbol> <buy|sell> <qty> --limit <px> --stop <px> [--stop-limit <px>] [--reduce-only]"
    ));
    line(format_args!(
        "    oto <venue> <symbol> <buy|sell> <qty> [@<price>|@market] (--stop <px> | --limit <px>)"
    ));
    line(format_args!(
        "    ladder <venue> <symbol> <buy|sell> <qty> [@<price>|@market] --stop-loss <px> --take-profit \
         <px>:<qty>..."
    ));
    line(format_args!(
        "      (one order list on ONE book and symbol; an order client id is MINTED per leg and shown in the \
         preview, --order-client-id once per leg pins them; `trade order oco|oto|ladder --help` says each)"
    ));
    line(format_args!("    flatten <venue> <symbol>"));
    line(format_args!("    mass-cancel [venue] [symbol]"));
    line(format_args!("    market-exit [venue]          (panic: cancel all + flatten all)"));
    line(format_args!(
        "      (<venue> may be <venue>/<LABEL> or <venue>/DEFAULT here too. A bare <venue> is taken \
         only where the node has ONE account of it, running or not, and refused elsewhere, naming the books; no \
         venue at all is the whole node)"
    ));
    // ⚠ Each is a WORD, and the help says what it does rather than what it sets: `state
    // <active|reducing|halted>` used to live here, and an argument that turns a read into a halt is
    // the shape ruling 17 removed. The halt line states the covered-reduce exemption, because an
    // operator who reads "halted" as "I am now trapped" un-halts to get out — restarting the
    // strategy that got them there. `docs/ops/kill-switches.md` argues that at length.
    line(format_args!(
        "    halt                         (mode -> Halted: no order that OPENS or ADDS risk; \
         position-covered reduces still pass, so market-exit/flatten still work)"
    ));
    line(format_args!("    resume                       (mode -> Active)"));
    line(format_args!(
        "  STRATEGY LIFECYCLE (preview + confirm; the node must speak these verbs):"
    ));
    line(format_args!(
        "    mount <venue> <symbol> <interval> --file <path-under-the-node's-strategies/> \
         [--id <mount-id>] [--params <json>]"
    ));
    line(format_args!(
        "      (--file is required; --params is a JSON object and runs to end of line, so put it \
         last; --file is a path on the NODE)"
    ));
    line(format_args!(
        "    unmount <mount-id>           (the --id given at mount time, else the node-derived \
         {{venue}}__{{symbol}}__{{interval}})"
    ));
    // SAY WHAT UNMOUNT DOES NOT DO. "unmount" reads to a person under pressure as "get me out of
    // this", and it is not: the node cancels that mount's live ORDERS and saves its state, and
    // leaves every POSITION open and now unattended. An operator who learns that from the position
    // table afterwards learned it too late.
    line(format_args!(
        "      (the node cancels that mount's live orders and saves its state; POSITIONS ARE NOT \
         FLATTENED — use `flatten`/`market-exit` for that)"
    ));
    line(format_args!("    set-setting <full.dotted.key> <value to end of line>"));
    // Say what the preview shows and that nothing else is asked: a scripted session reading this
    // screen must be able to tell that `--yes` covers the settings write like every other write.
    line(format_args!(
        "      (one row of the node's settings, named by its key and validated by the node's own \
         loader; the preview shows old → new, read off the node, and the y/N is the only question)"
    ));
    line(format_args!("  any WRITE line may end with:"));
    line(format_args!("    --reason <text to end of line>   recorded in the node's audit trail"));
    line(format_args!("  READ:"));
    line(format_args!("    orders [symbol]"));
    line(format_args!("    positions [venue]"));
    line(format_args!("    equity"));
    line(format_args!("    recent [N]"));
    line(format_args!(
        "    status                       (the trading mode AND one row per mounted strategy)"
    ));
    line(format_args!("    snapshot"));
    line(format_args!("  meta:  help    quit|exit  (or Ctrl-D)"));
    // The one-shot half of ruling 16: the same three words, outside the prompt. Stated in the REPL's
    // own help because an operator who has found the prompt is exactly the one who then wants to
    // put a halt in a runbook or a unit file.
    line(format_args!(
        "  the same three words run one-shot, no REPL: vike-cli trade <status|halt|resume> \
         --node <host:port>"
    ));
    // The per-venue twin of `resume` has no REPL word, like `recon confirm`: one-shot only.
    line(format_args!(
        "  one-shot only: vike-cli trade resume-venue <venue> [--force] --node <host:port>  \
         (release ONE venue's link dead-man halt; `resume` clears every venue's)"
    ));
    // A TP/SL bracket has no REPL word either: one-shot only, like the two above.
    line(format_args!(
        "  one-shot only: vike-cli trade order bracket <book> <symbol> <buy|sell> <qty> \
         [@<price>|@market] --stop-loss <price> --take-profit <price> --node <host:port>  (an entry \
         with its exits; a venue that cannot hold one refuses the whole bracket)"
    ));
    // The venue read has no REPL word either: one-shot only, observe key, writes nothing.
    line(format_args!(
        "  one-shot only: vike-cli trade venue <book> <balance|open-orders|positions> --node <host:port>  \
         (what the account's VENUE says now; read-only, observe key)"
    ));
    // The per-account twin of `halt`/`resume`: one-shot only as well.
    line(format_args!(
        "  one-shot only: vike-cli trade account halt <book> --node <host:port>  (ONE account's \
         engine; `reduce` and `resume` take the same line; the bare `halt`/`resume` reach every account)"
    ));
    // Giving a symbol back is one-shot only too.
    line(format_args!(
        "  one-shot only: vike-cli trade account remove-symbol <book> <symbol> --node <host:port>  (give \
         back one admitted symbol of ONE account, freeing its slot; only while the engine is flat in it)"
    ));
    // Clearing a mount's budget latch is one-shot only too.
    line(format_args!(
        "  one-shot only: vike-cli trade strategy reset-budget <mount> --node <host:port>  (clear ONE \
         mount's budget latch; only while the mount is flat; its loss budget counts from its \
         current PnL afterwards)"
    ));
    out
}
