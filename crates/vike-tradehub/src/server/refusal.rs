//! `server::refusal` — the node-edge ROUTING gates a control command passes before it is lowered:
//! the venue gate ([`venue_refusal`]), the account gate within it ([`account_refusal`], with its
//! bracket arm `bracket_book_refusal`) and a bracket's own verdict (`bracket_refusal`, values
//! first via `check_bracket`, then what its one engine can hold via `bracket_engine_refusal`).
//!
//! Split out of `server.rs` as a pure move. The gates are pure functions over the published engine
//! roster, which is what lets the Preview verbs and the `telegram` surface make the SAME calls
//! `super::control::accept_command` makes, so a preview and a command cannot answer one frame
//! differently.

use vike_tradehub_client::wire::{WireBracketSpec, WireCommand};

/// **REFUSE a command that names a venue this node runs no engine for** — the routing gate, and the
/// reason it exists is that without it the command does NOT fail, it lands somewhere else.
///
/// # What went wrong without it
///
/// Every order-carrying wire variant names a venue and always has ([`WireCommand::addressed_venue`]
/// is the one reading of it). [`super::control::lower_command`] copies that string onto
/// `vike_model::OrderRequest::venue`, and `vike_core`'s `CoreThread::route_of` resolves it to an
/// engine index — `.unwrap_or(0)` when it resolves to none. Engine 0 is the PRIMARY. So a control
/// peer naming a venue this process runs no engine for (an operator's DOM on a second exchange, a
/// `vike-cli submit okx …` against a binance-primary daemon, a typo) had its order risk-gated,
/// signed and sent by the PRIMARY venue's `ExecutionClient`, answered `Ack`, and appeared in the
/// snapshot — with no error anywhere. Worse than mere misrouting: `CoreThread::caps_venue` falls
/// back to the PAYLOAD's venue when nothing routed, so the capability preflight
/// (`vike_model::preflight_order_at`) was run against the row of the venue the order never reached.
///
/// # Why REFUSE rather than pick a default
///
/// Refusing costs the operator one retry and a message naming the venues this node actually runs.
/// Guessing costs them a position on a book they did not name, discovered on a statement. There is
/// no third answer available here: nothing in the frame says which of several engines was meant,
/// and "the primary" is not an inference from the operator's input, it is the absence of one.
///
/// # The three inputs, and the one that is a trap
///
/// * `cmd`'s address. `None` (an order-scoped, account-wide, or non-book verb) is never refused —
///   see [`WireCommand::addressed_venue`] for why `None` is a claim rather than an omission. In
///   particular the UNSCOPED panic button (`MarketExit { venue: None }`) reaches every engine and
///   can never be refused by this gate.
/// * `engines` — the venues this node runs an engine for
///   ([`crate::publish::PublisherHandle::engine_venues`]).
/// * ⚠ **An EMPTY `engines` means the core has not published yet, and is treated as UNKNOWN — the
///   command is NOT refused.** This is the one place this function deliberately does not bias
///   toward refusing, and the reason is that the opposite is a deadlock, not a conservative choice:
///   a core publishes when its state goes dirty, a refused command never reaches the core, so a
///   feed-less daemon that refused on an empty set would refuse every command it ever received, for
///   ever. The residual is the window between a node accepting connections and its first publish,
///   in which routing is exactly as unchecked as it was before this function existed.
///
/// The match is EXACT, never case-folded or trimmed, because exact is what the core does: engine
/// selection is a string comparison against `ExecutionEngine::route_key`, so accepting `"BINANCE"`
/// here would hand the core a string it then fails to route and silently sends to engine 0 — this
/// gate's own defect, reintroduced by being helpful. The refusal message names the roster, which is
/// what makes a case or spelling slip obvious in one line.
pub fn venue_refusal(cmd: &WireCommand, engines: &[String]) -> Option<String> {
    let venue = cmd.addressed_venue()?;
    // ⚠ The SENTENCE is `crate::config::no_engine_refusal`'s, not this function's, and the move is
    // the point rather than tidying: 0057's Phase 0 names the mount path as "the mount-side twin of
    // the defect the order path had fixed", so the profile path refuses an engine-less venue in
    // these exact words. Two spellings of one refusal is how an operator learns to read two
    // different faults into one situation.
    crate::config::no_engine_refusal(venue, engines, "The command")
}

/// **REFUSE a submit — or a runtime MOUNT, or a mass-cancel / flatten / market exit — naming an
/// ACCOUNT of that venue this node runs no engine for** — [`venue_refusal`]'s account-level
/// sibling, and the answer to the question that gate cannot pose.
///
/// # What went wrong without it
///
/// `vike-cli trade order submit binance/NOSUCH BTCUSDT buy 1` printed `accepted`. The node Acked
/// the frame and refused the order OUT OF BAND, as a recent-events note nobody was watching, so the
/// client reported success over an order that never existed. The core's own refusal is real and
/// stays — `crates/vike-core/src/runtime/routing.rs`'s `route_for_payload_account` composes
/// `route_key_of(venue, account)` and refuses when no engine carries it, rather than falling
/// through to that venue's default book — but it happens AFTER the Ack, on the other side of the
/// single-writer lane, which is a place a wire response can no longer be reached from. This gate
/// is the same verdict moved to where the client is still listening: the node ANSWERS before it
/// Acks. The core's copy is defence in depth for the callers this edge cannot cover (a journal
/// replay, a strategy-minted order, the GUI's own lane) and is not made redundant by this one.
///
/// # Why this is a SECOND function and not a WIDER [`venue_refusal`]
///
/// The two ask different questions, and only one of them has an answer the other could stand in
/// for. [`venue_refusal`] asks *does this node run this EXCHANGE at all*, compares against
/// [`crate::publish::PublisherHandle::engine_venues`], and its whole exactness argument is written
/// about venue ids. Re-pointing it at route keys would change what an existing refusal MEANS —
/// `binance` would stop matching a node that runs only `binance#ALT`, and the sentence an operator
/// reads would still say *"this node runs no engine for venue `binance`"* while the node plainly
/// runs one. So the venue gate keeps its slice and this one takes
/// [`crate::publish::PublisherHandle::engine_route_keys`] beside it.
///
/// The ORDER is load-bearing and is the other half of that argument: [`super::control::accept_command`] runs the
/// venue gate FIRST, so a venue this node does not run is refused in the venue's own words and can
/// never reach this function. What is left for this one is exactly the case the venue gate passes
/// and the router then misroutes — the venue IS run, the ACCOUNT is not. When no route key belongs
/// to `venue` at all this returns `None` for the same reason: that is the venue question wearing
/// account clothing, and two spellings of one refusal is how an operator learns to read two faults
/// into one situation (`crate::config::no_engine_refusal`'s own doc makes that argument for the
/// sentence it owns).
///
/// # `DEFAULT` is a NAME, and it resolves through the one composer
///
/// The wire admits `"account":"DEFAULT"`, meaning *the unlabelled account, deliberately* — a claim,
/// never an omission ([`vike_tradehub_client::wire::WireOrderRequest::account`] states the three
/// wire states). ⚠ `route_key_of` renders that account as the BARE VENUE ID, so `binance#DEFAULT`
/// is a key no engine anywhere carries, and a gate that suffixed the label text unconditionally
/// would turn the one spelling that addresses the original book into a refusal. This composes the
/// key with `route_key_of` — the same function `route_for_payload_account` composes with, so the
/// edge and the core cannot disagree about a spelling — and then asks whether the roster holds it.
/// A two-account node publishes `["binance", "binance#ALT"]`, so `DEFAULT` matches; a node whose
/// binance accounts are ALL labelled publishes neither, so `DEFAULT` names a book it does not have
/// and is refused by name. `vike_core`'s
/// `naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue` and
/// `a_payload_naming_default_on_an_unmounted_venue_refuses_while_its_account_less_twin_does_not`
/// pin the same two rows one plane down.
///
/// # The three inputs, and the two that are traps
///
/// * `cmd`. [`WireCommand::Submit`] and [`WireCommand::MountStrategy`] are checked — the two
///   variants that NAME a book they will trade — and the match below has no wildcard arm so that a
///   new variant is classified rather than defaulted. The mount arm is the same verdict one plane
///   up, and it was a declared residual until it landed: its stakes are a rung HIGHER than an
///   order's (a misrouted mount is every order that strategy will ever place), and until it was
///   checked here the node Acked the frame and `vike_core`'s `CoreThread::mount_strategy_runtime`
///   refused it out of band, which is the exact defect the submit arm above exists to delete. The
///   two share this function's sentence and differ only in its SUBJECT — see the `format!` below.
///   The three RISK-REDUCING verbs ([`WireCommand::MassCancel`], [`WireCommand::Flatten`],
///   [`WireCommand::MarketExit`]) are the THIRD plane, checked ONLY when they NAME an account — owner
///   ruling "B" (2026-09-26). An account-LESS reduce is never this gate's: the risk-direction law of
///   `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §4.5 is
///   that a reducing venue verb naming no account FANS OUT to every account of that venue, and
///   [`venue_refusal`]'s own doc records that the UNSCOPED panic button can never be refused by a
///   gate of this family. Narrowing a way OUT of a position is the one thing an account-routing
///   change must not do, and naming an account is an opt-in narrowing, never a precondition.
///   ⚠ One reducing-plane refusal does NOT consult the roster at all: an account named with NO
///   venue (`MassCancel`/`MarketExit`, whose venue is optional). It cannot resolve on any roster — a
///   label names one book OF a venue — and the arm it would otherwise reach is the GLOBAL one, so it
///   is refused even while the roster is UNKNOWN; there is no publish that could make it valid.
///   [`WireCommand::Bracket`] is the FOURTH plane and names no account at all;
///   [`bracket_book_refusal`] answers it, and it is the one arm here that refuses on an EMPTY
///   roster.
/// * `route_keys` — one per engine ([`crate::publish::PublisherHandle::engine_route_keys`]).
/// * ⚠ **An EMPTY `route_keys` means the core has not published yet, and is treated as UNKNOWN —
///   the command is NOT refused.** Inherited from [`venue_refusal`] rather than re-decided, and the
///   argument there is a deadlock rather than a preference: a core publishes when its state goes
///   dirty and a refused command never reaches the core, so a feed-less daemon that refused on an
///   empty roster would refuse every command it ever received, for ever. The inheritance is also
///   STRUCTURAL — both rosters are projections of one `portfolio.venues`, so they are empty
///   together and this gate cannot be armed while the venue gate is blind. ⚠ **A BRACKET is the
///   one exception**: it is refused on an empty roster, because its verdict is a POSITIVE claim
///   about the roster that an empty one cannot back, and refusing it alone leaves every other
///   command free to make the core publish — so the deadlock argument does not reach it.
/// * ⚠ **A MALFORMED account is not this gate's refusal.** `parse_wire_account` owns the charset,
///   the length bound and the case-sensitivity that makes `alt` a different string from `ALT`, and
///   [`super::control::lower_command`] already refuses on it — before the Ack, since it runs inside
///   [`super::control::accept_command`]. Answering here too would put two sentences on one fault, so a label this
///   gate cannot parse falls through to the parser that can.
///
/// The membership test is EXACT, never case-folded or trimmed, for [`venue_refusal`]'s reason
/// applied one field along: engine selection is a string comparison against
/// `vike_exec::ExecutionEngine::route_key`, so being helpful here hands the core a key it then
/// fails to route. The refusal NAMES the accounts of that venue this node does hold, rendered with
/// `AccountLabel`'s own `Display` (so the unlabelled one reads `DEFAULT`, the one spelling a client
/// can send back), sorted and deduplicated exactly as the venue roster is — which is what makes a
/// spelling slip obvious in one line.
pub fn account_refusal(cmd: &WireCommand, route_keys: &[String]) -> Option<String> {
    // NO WILDCARD ARM, for `WireCommand::addressed_venue`'s reason: a new order-carrying variant
    // must be CLASSIFIED here rather than fall into the not-checked column by default, which is
    // exactly where a new risk-INCREASING verb would land silently.
    let (venue, account, subject) = match cmd {
        WireCommand::Submit(req) => (req.venue.as_str(), req.account.as_deref()?, "The command"),
        // THE RISK-REDUCING PLANE — owner ruling "B", 2026-09-26. These three returned `None`
        // whatever account they named, and the reason was sound while it held: `lower_command`
        // DROPPED the account and the core fanned the verb over every account of the venue, so the
        // refusal lived CLIENT-side behind `FEATURE_ACCOUNT_SCOPED_REDUCE`, which no node
        // advertised — refusing here would have refused a verb the node's own capability claimed.
        // Both halves moved together: `lower_command` carries the field, `vike_core`'s reducing
        // arms narrow to the named account (`CoreThread::reduce_route_for_account`), and
        // `served_features` advertises the string. So a NAMED account this node does not hold is
        // now exactly the out-of-band refusal this gate exists to move in front of the Ack.
        //
        // ⚠ An ABSENT account is still never this gate's (`as_deref()?`) — §4.5's fan-out, and the
        // UNSCOPED panic button, which names neither a venue nor an account.
        WireCommand::MassCancel { venue: Some(venue), account: Some(account), .. } => {
            (venue.as_str(), account.as_str(), "The mass-cancel")
        }
        WireCommand::Flatten { venue, account, .. } => {
            (venue.as_str(), account.as_deref()?, "The flatten")
        }
        WireCommand::MarketExit { venue: Some(venue), account: Some(account) } => {
            (venue.as_str(), account.as_str(), "The market exit")
        }
        // ⚠ AN ACCOUNT WITH NO VENUE — refused here, BEFORE the roster is consulted, because no
        // roster could resolve it: an account label names one book OF a venue. And the arm a
        // venue-less `MassCancel`/`MarketExit` reaches in the core is the GLOBAL one, every engine
        // on the node, so reading the account as ignorable would turn the narrowest request an
        // operator can make into the widest action the node can take. The core refuses it too
        // (defence in depth for the callers this edge does not stand in front of); this is the
        // answer the client hears.
        WireCommand::MassCancel { venue: None, account: Some(account), .. } => {
            return Some(venueless_account_refusal(account, "The mass-cancel"));
        }
        WireCommand::MarketExit { venue: None, account: Some(account) } => {
            return Some(venueless_account_refusal(account, "The market exit"));
        }
        // Account-less: the venue-wide fan-out and the unscoped panic button. Never refused here.
        WireCommand::MassCancel { account: None, .. }
        | WireCommand::MarketExit { account: None, .. } => return None,
        // THE MOUNT PLANE — this arm was a DECLARED RESIDUAL (`return None`) and is now checked.
        // `MountStrategy` names an account too, and its own wire doc rates the stakes a rung HIGHER
        // than an order's: a misrouted order is one order on the wrong book, a misrouted mount is
        // every order that strategy will ever place, sized against a book nobody named. The
        // out-of-band shape was identical to the one this gate deletes for orders — the node Acked
        // the frame and `vike_core`'s `CoreThread::mount_strategy_runtime` refused it on the far
        // side of the single-writer lane as a recent-events note — and NOTHING else at this edge
        // had anything to say about it: `crate::mount_factory::validate_spec` validates the PROFILE
        // (the strategy name, the params keys, the name-XOR-rhai) and never the account, so a
        // `buy_hold` mount onto `binance#NOSUCH` was a well-formed spec and a plain `Ack`.
        //
        // ⚠ It is a WIDENING of what this gate refuses, and every one of its silences is INHERITED
        // rather than re-decided, which is what keeps it cheap: an account-less mount is untouched
        // (`as_deref()?`), an empty roster is UNKNOWN, a venue with no engine at all is
        // `venue_refusal`'s sentence, and an unparseable label is `lower_command`'s. So the only
        // frames that newly refuse are the ones the core was already refusing out of band.
        WireCommand::MountStrategy { venue, account, .. } => {
            (venue.as_str(), account.as_deref()?, "The mount")
        }
        // THE BRACKET — the one order verb that cannot name its account. See
        // `bracket_book_refusal`: it is accepted only on a venue whose one account is the default.
        WireCommand::Bracket(b) => return bracket_book_refusal(&b.venue, route_keys),
        // Verbs that name no account at all: order-scoped (the coid resolves the engine), the
        // account-wide kill switch, the mount-keyed verbs, and the one verb that never enters the
        // core.
        WireCommand::Cancel(_)
        | WireCommand::Modify { .. }
        | WireCommand::SetTradingState(_)
        | WireCommand::UpdateParams { .. }
        | WireCommand::UnmountStrategy { .. }
        | WireCommand::SetSetting { .. } => return None,
    };
    if route_keys.is_empty() {
        return None;
    }
    // A label this gate cannot parse belongs to `lower_command`'s refusal, not to a second one.
    let label = vike_model::accounts::account_keys::parse_wire_account(account).ok()?;
    // THE ONE COMPOSER — `route_key_of`, the same call the core's own resolution makes. The default
    // account renders as the bare venue id here, which is why `DEFAULT` matches a single-account
    // roster rather than minting `venue#DEFAULT` and refusing the book it names.
    let wanted = vike_model::accounts::account_keys::route_key_of(venue, &label);
    if route_keys.iter().any(|k| k == &wanted) {
        return None;
    }
    let mut held: Vec<String> = route_keys
        .iter()
        .filter_map(|k| vike_model::accounts::account_keys::label_of_route_key(venue, k))
        .map(|l| l.to_string())
        .collect();
    // No engine of this venue at all ⇒ the VENUE question, which `venue_refusal` answers first and
    // in its own words. Reachable here only by a caller that runs this gate alone.
    if held.is_empty() {
        return None;
    }
    held.sort_unstable();
    held.dedup();
    // ⚠ ONE SENTENCE, THREE PLANES, and `subject` is the whole of the difference — the shape
    // `crate::config::no_engine_refusal` already carries for the venue question, where the order
    // path passes "The command" and the profile path passes its mount row. Two spellings of one
    // refusal is how an operator learns to read two different faults into one situation, so only
    // the SUBJECT varies and the order plane's sentence is byte-identical to before the mount and
    // reducing arms existed. The tail stays order-shaped for a mount too, exactly as
    // `no_engine_refusal`'s does for its mount caller: what a mount misroutes IS orders, every one
    // it will ever place. For a reducing verb it is order-shaped for the reason its own arm gives:
    // a flatten IS an order, and a cancel or exit applied to "another book of that venue" is the
    // venue-wide fan-out the named account was sent to prevent.
    //
    // It is bound in the match above rather than taken as a PARAMETER, which is where this gate
    // diverges from that one and has to: `no_engine_refusal` has two callers on two planes, while
    // this gate has one caller (`accept_command`) carrying every plane, so a caller-supplied
    // subject could only ever be a constant that was wrong for all but one of them.
    Some(format!(
        "this node runs no account `{account}` of venue `{venue}` — it runs: {}. {subject} was \
         REFUSED rather than applied to another book of that venue: an order names the book it is \
         for, and a node that cannot honour the name must not choose one",
        held.join(", ")
    ))
}

/// [`account_refusal`]'s sentence for a risk-REDUCING verb that names an account and NO venue —
/// the one refusal on this gate the roster has no part in (see that function's doc for why it is
/// refused even while the roster is UNKNOWN).
///
/// It keeps the plane sentence's shape — the fault, then `{subject} was REFUSED rather than …` —
/// with the tail that fits this fault: what it would otherwise have been is not "another book of
/// that venue" but EVERY book on the node, and the fix is a word the operator can add.
fn venueless_account_refusal(account: &str, subject: &str) -> String {
    format!(
        "account `{account}` was named with no venue — an account label names one book OF a venue, \
         so on its own it names nothing this node can resolve. {subject} was REFUSED rather than \
         widened to every engine: name the venue the account belongs to, or name neither for the \
         whole-node verb"
    )
}

/// [`account_refusal`]'s BRACKET arm — the one order verb on this wire that cannot name its
/// account.
///
/// `vike_model::BracketSpec` carries no account, so neither does
/// [`vike_tradehub_client::wire::WireBracketSpec`]. The core lowers a bracket with
/// `EngineRoute::Payload` and no account, which leaves two outcomes:
/// - on a venue with several engines, `vike_core`'s `CoreThread::ambiguous_accounts` refuses it
///   AFTER this node's `Ack`, as a recent-events note;
/// - on a venue whose only engine is LABELLED, `CoreThread::route_of` finds no bare-venue key and
///   falls back to engine 0, which may be another exchange's book.
///
/// So this cut accepts a bracket exactly when it can mean only one book: the venue's published
/// route keys are the bare venue id and nothing else. Every other shape is refused here, before the
/// `Ack`, naming the accounts the venue does have (in `AccountLabel`'s own `Display`, so the
/// unlabelled one reads `DEFAULT`, sorted as strings exactly as [`account_refusal`]'s sentence
/// sorts them).
///
/// ⚠ **The venue's keys are counted RAW** — no dedup, and a key whose label does not parse still
/// counts. The core counts ENGINES (`CoreThread::engines_of_venue` compares each engine's venue and
/// never parses a label), and a gate that answered "exactly one" where the core answers "several"
/// would hand the core its after-the-`Ack` refusal back. Which keys are this venue's is
/// [`route_key_is_of_venue`]'s answer: the two shapes `vike_model::accounts::account_keys::route_key_of`
/// renders, read by shape alone.
///
/// ⚠ **An EMPTY roster REFUSES. This is the one place this gate family departs from
/// [`venue_refusal`]'s UNKNOWN rule.** That rule is a deadlock argument: refusing EVERY command on
/// an empty roster would stop a feed-less core from ever publishing. A refused bracket leaves every
/// other command flowing, so the core still publishes on the first of them that changes its state.
/// And "this venue's one account is the default one" is a positive claim that an empty roster
/// cannot back.
///
/// A venue with no engine at all answers `None`: that is [`venue_refusal`]'s question, asked first
/// and in its own words.
///
/// What this gate establishes — exactly one engine, the default account's — is the premise of the
/// next one: [`bracket_refusal`] then judges the bracket by THAT engine's published block (does it
/// trade the bracket's symbol, and can its own lane hold a stop-loss), because the adapter places
/// every order on its engine's instrument, whatever the frame names.
fn bracket_book_refusal(venue: &str, route_keys: &[String]) -> Option<String> {
    if route_keys.is_empty() {
        return Some(format!(
            "this node has not published its accounts yet, so it cannot tell whether venue \
             `{venue}` runs exactly one account. The bracket was REFUSED rather than sent: a \
             bracket cannot name its account yet, so it goes only to a venue whose one account is \
             the default one. Retry once the node has published its first snapshot"
        ));
    }
    let of_venue: Vec<&str> =
        route_keys.iter().map(String::as_str).filter(|k| route_key_is_of_venue(venue, k)).collect();
    match of_venue.as_slice() {
        [] => None,
        [only] if *only == venue => None,
        _ => {
            let mut held: Vec<String> = of_venue
                .iter()
                .map(|k| {
                    vike_model::accounts::account_keys::label_of_route_key(venue, k)
                        .map_or_else(|| (*k).to_string(), |l| l.to_string())
                })
                .collect();
            held.sort_unstable();
            Some(format!(
                "venue `{venue}` runs these accounts on this node: {}. A bracket cannot name its \
                 account yet, so it goes only to a venue whose one account is the default one. The \
                 bracket was REFUSED rather than applied to one of them",
                held.join(", ")
            ))
        }
    }
}

/// Whether `key` is one of `venue`'s route keys, by SHAPE alone: the bare venue id, or the venue id
/// followed by the label separator — whatever follows it, parseable or not.
///
/// ⚠ A third spelling of the route-key separator, beside `vike_model::accounts::account_keys::route_key_of`
/// (which renders it) and `label_of_route_key` (which parses it). Neither can answer this
/// question: `label_of_route_key` answers `None` both for another venue's key and for this
/// venue's key with an unparseable label, and [`bracket_book_refusal`] must COUNT the second (it
/// counts raw, as the core counts engines). So it is PINNED to `route_key_of` over the whole
/// roster instead (`account_refusal_tests`'
/// `the_route_key_predicate_agrees_with_route_key_of_for_every_roster_venue`): a change to the
/// separator reddens that test rather than this gate silently counting nothing.
pub(super) fn route_key_is_of_venue(venue: &str, key: &str) -> bool {
    key == venue || key.strip_prefix(venue).is_some_and(|rest| rest.starts_with('#'))
}

/// A bracket's VALUES, refused before the `Ack` — [`bracket_refusal`]'s first half, and the one
/// [`super::control::lower_command`] repeats for a caller that lowers a frame without that gate.
///
/// ⚠ **Nothing here reads the venue's lane or the symbol.** That question is about the ENGINE the
/// bracket reaches, not about the frame, and this function holds no roster: it is
/// [`bracket_engine_refusal`]'s, which reads the engine's published block. (It lived here once,
/// read off the frame's symbol, and admitted a `.P` bracket that a spot-mounted engine then signed
/// on the spot lane — that function's doc carries the scenario.)
///
/// # Why the node checks what a client already should
///
/// The core HOLDS a bracket's two exits and first examines them at RELEASE, when the entry has
/// filled, and a covered reduce bypasses the risk gate's price collar and floors by design. So an
/// exit the venue cannot accept fails AFTER the position is open, and the operator holds it without
/// the protection they asked for. A plain `Submit` carrying the same value fails with no position,
/// so without this check a bracket would have a failure mode its three legs sent one by one do not.
/// A client may validate first — the desktop's ticket plans a bracket through
/// `Planner::admit_bracket` (`crates/vike-app-core/src/orders/order_dispatch.rs`) — but this edge
/// is the one every client meets, whatever it checked.
///
/// # What is refused, each naming the field
///
/// * `side` not exactly `1` or `-1`. The adapters read anything `<= 0` as a SELL, and
///   `vike_model::build_bracket` gives the exits `-side`, so a `0` would make all three legs sells.
/// * `qty`, a priced `entry_price`, `stop_loss` or `take_profit` that is not finite and above zero.
///   This check is the ONLY one at this edge that catches a NaN: [`super::control::ControlLimits`]' notional cap
///   runs BEFORE it in [`super::control::accept_command`] and cannot see one (`NaN > max` is false). For `qty` it
///   is the only guard this edge has. For a PRICE it also runs before the ordering checks below, and
///   there the order buys the field's NAME rather than the refusal: a NaN price makes every
///   ordering comparison false, so it would still be refused — but as an "inverted" bracket, which
///   sends an operator looking at the wrong field.
/// * An INVERTED bracket. With a priced entry a buy needs `stop_loss < entry_price < take_profit`
///   and a sell `take_profit < entry_price < stop_loss`, strictly. With a MARKET entry
///   (`entry_price: None`) there is no price to order the exits around, but their order against
///   EACH OTHER is still checkable with no false positive: a buy needs `stop_loss < take_profit`, a
///   sell `take_profit < stop_loss`.
///
/// # ⚠ DECLARED RESIDUAL: the inversion check compares against the LIMIT price, not the fill
///
/// This edge holds no book, so it cannot know where a limit entry will FILL. A MARKETABLE limit —
/// a buy limit at 200 with the market at 100 — fills near 100, and its stop-loss at 150 passes the
/// check above yet sits ABOVE the fill when it is released. A stop that would trigger immediately
/// is refused by some venues outright (Binance perp answers `-2021` to such a `STOP_MARKET`), which
/// leaves the position unprotected. Nothing here can see that; the same is true of a market entry's
/// exits against wherever the market happens to be. Passing this check is therefore a statement
/// about the frame, never a promise that both exits will rest.
pub(super) fn check_bracket(b: &WireBracketSpec) -> Result<(), String> {
    if b.side != 1 && b.side != -1 {
        return Err(format!(
            "bracket: `side` must be 1 (a long entry) or -1 (a short entry), got {}",
            b.side
        ));
    }
    let positive = |field: &str, v: f64| {
        if v.is_finite() && v > 0.0 {
            Ok(())
        } else {
            Err(format!("bracket: `{field}` must be a finite number above zero, got {v}"))
        }
    };
    positive("qty", b.qty)?;
    if let Some(entry) = b.entry_price {
        positive("entry_price", entry).map_err(|e| format!("{e} (null is a market entry)"))?;
    }
    positive("stop_loss", b.stop_loss)?;
    positive("take_profit", b.take_profit)?;
    let (sl, tp) = (b.stop_loss, b.take_profit);
    let dir = if b.side == 1 { "long" } else { "short" };
    match b.entry_price {
        Some(entry) => {
            let ordered =
                if b.side == 1 { sl < entry && entry < tp } else { tp < entry && entry < sl };
            if !ordered {
                let rule = if b.side == 1 {
                    "stop_loss < entry_price < take_profit"
                } else {
                    "take_profit < entry_price < stop_loss"
                };
                return Err(format!(
                    "bracket: an inverted {dir} bracket — it needs {rule}, got stop_loss {sl}, \
                     entry_price {entry}, take_profit {tp}. An exit on the wrong side of its \
                     entry would fire the moment the entry fills"
                ));
            }
        }
        None => {
            let ordered = if b.side == 1 { sl < tp } else { tp < sl };
            if !ordered {
                let rule =
                    if b.side == 1 { "stop_loss < take_profit" } else { "take_profit < stop_loss" };
                return Err(format!(
                    "bracket: an inverted {dir} bracket — even with a market entry it needs \
                     {rule}, got stop_loss {sl}, take_profit {tp}. Whatever the entry fills at, \
                     one of these exits is on the wrong side of it"
                ));
            }
        }
    }
    Ok(())
}

/// **THE bracket verdict past the account gate** — `Some(reason)` when this node refuses the
/// bracket, `None` for an admissible one and for every other verb: its VALUES ([`check_bracket`]),
/// then what the ONE engine it reaches can hold ([`bracket_engine_refusal`]), in that order.
///
/// ⚠ **One call, three places, and that is the point.** [`super::control::accept_command`] makes it at its step 1d,
/// and both preview surfaces (the `Request::Preview` arm and the `telegram` channel's
/// `ProdTelegramDeps::preview`) make it as their fourth step, each directly after
/// [`account_refusal`]. A Preview and a Command therefore cannot answer one frame differently,
/// because there is no second composition of these checks to drift from the first.
///
/// `blocks` is the node's published engine blocks
/// ([`crate::publish::PublisherHandle::engine_blocks`]); see [`bracket_engine_refusal`] for what
/// an empty one means.
pub(crate) fn bracket_refusal(
    cmd: &WireCommand,
    blocks: &[vike_core::VenueBlock],
) -> Option<String> {
    let WireCommand::Bracket(b) = cmd else { return None };
    check_bracket(b).err().or_else(|| bracket_engine_refusal(b, blocks))
}

/// [`bracket_refusal`]'s ENGINE half: whether the ONE engine a bracket reaches can hold it, judged
/// from that engine's PUBLISHED block, never from the frame.
///
/// # Why the engine and not the frame
///
/// A venue adapter takes its instrument from its ENGINE, not from the order. Binance's exec picks
/// its spot or perp loop ONCE, from the symbol the engine was mounted on
/// (`crates/bridges/binance/src/exec.rs`'s `run`), and then signs every order on that symbol
/// (`crates/bridges/binance/src/spot.rs`'s `BinanceSpotRest::build_order_params`); okx's client is
/// bound to one SWAP instrument the same way. Nothing downstream refuses an order naming a symbol
/// its engine was not mounted on — it is placed on the engine's instrument. So a check that read
/// the lane off the FRAME's symbol admitted `BTCUSDT.P` against a spot-mounted binance engine and
/// signed all three legs on binance SPOT: the entry filled, the stop-loss leg was rejected at
/// release, and the position was left with its take-profit only — the failure the check exists to
/// stop, reached by following the old refusal's own advice to "use `BTCUSDT.P`".
///
/// # What is refused, each naming what the engine trades
///
/// * **No engine to judge.** [`account_refusal`]'s bracket arm admitted the bracket only when its
///   venue's one route key is the bare venue id, so the engine is the block keyed by that id. A
///   caller that hands this an empty or a stale roster, or a block that names no symbol (an engine
///   that did not say), gets a REFUSAL rather than a bracket nobody checked: like the account
///   gate's empty-roster arm, admitting one is a positive claim this function cannot back. On
///   [`super::control::accept_command`]'s path the account gate has already refused both shapes in its own words.
/// * ⚠ **An engine on the SPOT lane of binance or aster** (`vike_catalog::engine_lane_holds_stop`
///   answers `false`), WHATEVER symbol the frame names. Their `vike_model::caps_for` rows list
///   `stop` as the spot+perp UNION, while the spot order builder sends `type=STOP` with no stop
///   price, so the core preflight passes the stop-loss leg and the venue rejects it at release. The
///   lane is read off the ENGINE's mounted symbol — `vike_core::VenueBlock::symbol`, the one the
///   adapter routes on — so `.p` in lower case is the SPOT lane, exactly as the adapter reads it.
///   No perp symbol is suggested: an engine mounted on the spot lane places nothing on the perp
///   lane, so the only fix is a perp engine. Refused here rather than fixed in the caps table, whose
///   per-lane row is the capability playbook's STEP 1 and not this cut; a paper mount fills stops on
///   either lane and cannot show the defect. ⚠ The rule lives in `vike-catalog` since the Trade
///   window needed it too (its final review, slice B, I-1): ONE predicate below both sides, so the
///   desktop's dispatcher and ticket refuse and explain the same bracket before the click, and the
///   two cannot disagree about a lane; its pin to `vike_catalog::fee_lane`'s dual-lane set moved
///   there with it.
/// * **A symbol the engine does not trade** (`vike_core::VenueBlock::trades`, the projection the
///   node publishes as `WireVenueBlock::symbols`) — the node-side twin of the desktop's
///   `order_dispatch::tradable`. The refusal names the engine's own symbols, and that is the only
///   place a perp `BASE.P` is ever suggested: when the engine trades it.
///
/// So TP/SL works only on the default account of a venue the node runs exactly ONE engine for, for
/// a symbol that engine trades, and on binance and aster only when that engine is a perp.
pub(super) fn bracket_engine_refusal(
    b: &WireBracketSpec,
    blocks: &[vike_core::VenueBlock],
) -> Option<String> {
    let venue = b.venue.as_str();
    let Some(engine) = blocks.iter().find(|v| v.route_key == venue && !v.symbol.is_empty()) else {
        return Some(format!(
            "bracket: this node publishes no engine for venue `{venue}`'s default account that \
             says what it trades, so it cannot tell whether that engine can hold this bracket. The \
             bracket was REFUSED rather than sent to an engine nobody checked"
        ));
    };
    let trades = std::iter::once(&engine.symbol)
        .chain(engine.extra_symbols.iter())
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if !vike_catalog::engine_lane_holds_stop(venue, &engine.symbol) {
        return Some(format!(
            "bracket: this node's `{venue}` engine trades {trades}, on {venue}'s spot lane, and \
             that lane cannot hold a stop-loss yet (the venue would reject the stop only after the \
             entry filled, leaving the position unprotected). The engine places every order on the \
             symbol it was mounted on, whatever a bracket names, so no bracket goes to `{venue}` on \
             this node until it runs a perp-lane engine (a `{}` symbol)",
            vike_catalog::PERP_SUFFIX
        ));
    }
    if !engine.trades(&b.symbol) {
        return Some(format!(
            "bracket: this node's `{venue}` engine trades {trades}, not `{}`. A bracket goes to that \
             one engine, which places every order on the symbol it was mounted on, so this bracket \
             would land on another instrument. Send it on a symbol that engine trades",
            b.symbol
        ));
    }
    None
}
