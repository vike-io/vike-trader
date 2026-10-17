//! The REPL's terse verb reference — the `help` verb, and the block `vike-cli trade --help` prints
//! after the plane's own roster.

use std::fmt::Write as _;

use crate::cmd::verbs;

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
         [--coid <id>]"
    ));
    line(format_args!(
        "    buy|sell <venue> <symbol> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]"
    ));
    line(format_args!(
        "      (a client_order_id is MINTED per order and shown in the preview; --coid pins one)"
    ));
    // STATE THE CHARSET HERE. `submit --coid` refuses a bad one with a good message, but a rejection
    // is the wrong place to learn a constraint the help screen could have stated — and `cancel`,
    // which takes the same field, only WARNS (it must; see `verbs::coid_charset_warning`), so the
    // help is the one surface that can state the rule once for every verb that takes a coid.
    line(format_args!("      <id> is {}", verbs::COID_CHARSET));
    line(format_args!(
        "    cancel <coid>                (fire-and-forget: the node cannot confirm a match)"
    ));
    line(format_args!("    modify <coid> [--qty Q] [--price P]"));
    line(format_args!("    flatten <venue> <symbol>"));
    line(format_args!("    mass-cancel [venue] [symbol]"));
    line(format_args!("    market-exit [venue]          (panic: cancel all + flatten all)"));
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
        "    mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path-on-the-node>) \
         [--id <mount-id>] [--params <json>]"
    ));
    line(format_args!(
        "      (--name XOR --rhai; --params is a JSON object and runs to end of line, so put it \
         last; --rhai is a path on the NODE)"
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
    out
}
