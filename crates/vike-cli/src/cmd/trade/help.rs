//! The REPL's terse verb reference — the `help` verb and the `--help` startup path.

use crate::cmd::verbs;

/// Print the terse verb reference (the `help` verb + the `--help` startup path).
pub(super) fn print_verb_help() {
    println!("commands:");
    println!("  WRITE (preview + confirm):");
    println!(
        "    submit <venue> <symbol> <buy|sell> <qty> [@<price>|@market] [--reduce-only] \
         [--coid <id>]"
    );
    println!(
        "    buy|sell <venue> <symbol> <qty> [@<price>|@market] [--reduce-only] [--coid <id>]"
    );
    println!(
        "      (a client_order_id is MINTED per order and shown in the preview; --coid pins one)"
    );
    // STATE THE CHARSET HERE. `submit --coid` refuses a bad one with a good message, but a rejection
    // is the wrong place to learn a constraint the help screen could have stated — and `cancel`,
    // which takes the same field, only WARNS (it must; see `verbs::coid_charset_warning`), so the
    // help is the one surface that can state the rule once for every verb that takes a coid.
    println!("      <id> is {}", verbs::COID_CHARSET);
    println!("    cancel <coid>                (fire-and-forget: the node cannot confirm a match)");
    println!("    modify <coid> [--qty Q] [--price P]");
    println!("    flatten <venue> <symbol>");
    println!("    mass-cancel [venue] [symbol]");
    println!("    market-exit [venue]          (panic: cancel all + flatten all)");
    // ⚠ Each is a WORD, and the help says what it does rather than what it sets: `state
    // <active|reducing|halted>` used to live here, and an argument that turns a read into a halt is
    // the shape ruling 17 removed. The halt line states the covered-reduce exemption, because an
    // operator who reads "halted" as "I am now trapped" un-halts to get out — restarting the
    // strategy that got them there. `docs/ops/kill-switches.md` argues that at length.
    println!(
        "    halt                         (mode -> Halted: no order that OPENS or ADDS risk; \
         position-covered reduces still pass, so market-exit/flatten still work)"
    );
    println!("    resume                       (mode -> Active)");
    println!("  STRATEGY LIFECYCLE (preview + confirm; the node must speak these verbs):");
    println!(
        "    mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path-on-the-node>) \
         [--id <mount-id>] [--params <json>]"
    );
    println!(
        "      (--name XOR --rhai; --params is a JSON object and runs to end of line, so put it \
         last; --rhai is a path on the NODE)"
    );
    println!(
        "    unmount <mount-id>           (the --id given at mount time, else the node-derived \
         {{venue}}__{{symbol}}__{{interval}})"
    );
    // SAY WHAT UNMOUNT DOES NOT DO. "unmount" reads to a person under pressure as "get me out of
    // this", and it is not: the node cancels that mount's live ORDERS and saves its state, and
    // leaves every POSITION open and now unattended. An operator who learns that from the position
    // table afterwards learned it too late.
    println!(
        "      (the node cancels that mount's live orders and saves its state; POSITIONS ARE NOT \
         FLATTENED — use `flatten`/`market-exit` for that)"
    );
    println!("    set-setting <full.dotted.key> <value to end of line>");
    // Say what the preview shows and that nothing else is asked: a scripted session reading this
    // screen must be able to tell that `--yes` covers the settings write like every other write.
    println!(
        "      (one row of the node's settings, named by its key and validated by the node's own \
         loader; the preview shows old → new, read off the node, and the y/N is the only question)"
    );
    println!("  any WRITE line may end with:");
    println!("    --reason <text to end of line>   recorded in the node's audit trail");
    println!("  READ:");
    println!("    orders [symbol]");
    println!("    positions [venue]");
    println!("    equity");
    println!("    recent [N]");
    println!(
        "    status                       (the trading mode AND one row per mounted strategy)"
    );
    println!("    snapshot");
    println!("  meta:  help    quit|exit  (or Ctrl-D)");
    // The one-shot half of ruling 16: the same three words, outside the prompt. Stated in the REPL's
    // own help because an operator who has found the prompt is exactly the one who then wants to
    // put a halt in a runbook or a unit file.
    println!(
        "  the same three words run one-shot, no REPL: vike-cli trade <status|halt|resume> \
         --node <host:port>"
    );
}
