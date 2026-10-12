//! **The BOOK selector — one addressing grammar, one implementation.**
//!
//! A trade request's WHO coordinate is `venue x account`, and the owner ruled it a POSITIONAL
//! first argument rather than two flags: two flags are two things to forget and can be
//! half-given, while one positional cannot be partially specified.
//!
//! This module adds NO second addressing scheme. `vike_model::accounts::account_keys` decided the
//! vocabulary — the label type, its validation and the `parse_wire_account` wire parse — and every
//! refusal here is that type's own, never re-worded.
//!
//! The precedent for the shape is `crate::cmd::runs::selector`: its `FORMS` const is both the
//! roster and the text every refusal appends, so a form cannot ship without its refusal learning
//! about it.

use vike_model::accounts::account_keys::AccountLabel;

use crate::cmd::args::{Flags, help_requested, no_value};
use crate::exit::CliError;

/// The roster AND the text every refusal appends. ⚠ Adding a form means adding it HERE, which is
/// what keeps the refusals from going stale.
pub(crate) const FORMS: &str = "<venue> | <venue>/<LABEL> | <venue>/DEFAULT | @<mark>";

/// One account of one venue — the resolved WHO coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Book {
    pub venue: String,
    pub label: AccountLabel,
}

/// Parse a selector. PURE — no settings, no socket, no store.
pub(crate) fn parse(selector: &str) -> Result<Book, CliError> {
    if selector.is_empty() {
        return Err(CliError::usage(format!("an empty book selector - expected one of: {FORMS}")));
    }
    if let Some(name) = selector.strip_prefix('@') {
        // A MARK. Recognised as a FORM it cannot yet honour, never reported as an unknown venue:
        // the `runs` selector makes the same distinction for `<id>#N`.
        return Err(CliError::usage(format!(
            "`@{name}` is a MARK, and marks are not built yet - name the book directly. \
             The forms are: {FORMS}"
        )));
    }
    if let Some((venue, label)) = selector.split_once('#') {
        // A ROUTE KEY (`venue#LABEL`), never a book selector — recognised so it can be REFUSED
        // WELL, the same courtesy the `@<mark>` arm above gets, rather than silently mis-parsed.
        // ⚠ **This is not a hypothetical spelling an operator would never type.** The wire hands it
        // back verbatim: `vike_model::accounts::account_keys::route_key_of` renders it,
        // `vike_tradehub_client::wire::WireVenueBlock`'s `route_key` field publishes it on every
        // `position ls`/`trade status` read, and the MCP surface's own refusal text recommends
        // copying it EXACTLY ("read `venues[].route_key`, and re-issue naming one of them EXACTLY",
        // `crates/vike-cli/src/cmd/mcp/venue_gate.rs`'s `venue_verdict`). Left unrecognised, `binance#ALT`
        // would parse as a BARE venue literally named `"binance#ALT"` (no `/`, so a Default label)
        // — a venue string no real engine answers to, which fails LATE at the node instead of being
        // refused here, or not at all inside the node's own pre-first-publish routing window.
        return Err(CliError::usage(format!(
            "'{selector}' is a ROUTE KEY, not a book selector — this plane has ONE addressing \
             grammar and it spells an account with '/', never '#'. Name it as `{venue}/{label}` \
             instead. The forms are: {FORMS}"
        )));
    }
    let (venue, label) = match selector.split_once('/') {
        None => (selector, AccountLabel::Default),
        Some((v, rest)) => {
            if rest.contains('/') {
                return Err(CliError::usage(format!(
                    "'{selector}' has more than one '/' - a selector names a venue and at most one \
                     account. The forms are: {FORMS}"
                )));
            }
            (
                v,
                vike_model::accounts::account_keys::parse_wire_account(rest).map_err(|e| {
                    CliError::usage(format!("'{selector}': {e} The forms are: {FORMS}"))
                })?,
            )
        }
    };
    if venue.is_empty() {
        return Err(CliError::usage(format!(
            "'{selector}' names no venue - expected one of: {FORMS}"
        )));
    }
    Ok(Book { venue: venue.to_string(), label })
}

/// Render a book as the operator typed it: the bare venue for the default account.
pub(crate) fn render(book: &Book) -> String {
    match book.label.text() {
        None => book.venue.clone(),
        Some(l) => format!("{}/{l}", book.venue),
    }
}

/// The parsed line of a book-scoped `ls` — `trade order ls` and `trade position ls`, the two READ
/// verbs whose grammar is "an optional book selector, then `--node`/`--symbol`/`--json`". One struct
/// and one [`parse_ls`] here because those two modules used to carry byte-identical copies of both.
#[derive(Debug)]
pub(crate) struct LsArgs {
    pub node: String,
    /// The optional positional book selector — narrows to one VENUE. Whether it can narrow to one
    /// ACCOUNT of that venue as well is the calling verb's `run_ls` question, not this grammar's:
    /// `position ls` can, `order ls` cannot yet (see [`refuse_an_unaddressable_book`] and
    /// [`crate::cmd::trade::render::order_rows`]).
    pub book: Option<Book>,
    pub symbol: Option<String>,
    pub json: bool,
}

/// `ls`'s own grammar: an optional LEADING positional (the book selector — recognised because it is
/// the one token that does not start with `-`), then ordinary flags. PURE.
pub(crate) fn parse_ls(args: impl Iterator<Item = String>) -> Result<LsArgs, String> {
    let mut it = args.peekable();
    let book = match it.peek() {
        Some(tok) if !tok.starts_with('-') => {
            let raw = it.next().expect("just peeked Some");
            Some(parse(&raw).map_err(|e| e.msg)?)
        }
        _ => None,
    };
    let mut node: Option<String> = None;
    let mut symbol: Option<String> = None;
    let mut json = false;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--symbol" => symbol = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let node = node.ok_or("--node <host:port> is required")?;
    Ok(LsArgs { node, book, symbol, json })
}

/// Refuse a book an ORDER READ cannot attribute a row to.
///
/// ⚠ **ORDER-SCOPED, and this narrowing is a CORRECTION rather than the original design.** This
/// function used to guard every read row type at once, on the argument that `WireOrderView` and
/// `WirePositionView` shared the same gap — neither carried a per-row account field. Task 6 measured
/// `WirePositionView`'s actual situation directly against
/// `crates/vike-tradehub-client/src/wire.rs` and found the shared premise false: a position lives
/// inside a `vike_tradehub_client::wire::WireVenueBlock`, and THAT type carries `account` — so
/// `crate::cmd::trade::render::position_rows` can and does attribute a labelled book for real, and
/// `crate::cmd::trade::position`'s `ls` never calls this function. Only the ORDER read still cannot
/// attribute a label for certain (the next paragraphs), so this function's scope shrank to match —
/// the wording used to say "the snapshot" and name both row types, which became misleading the
/// moment `position ls` started narrowing correctly on the same selector: an operator refused here
/// and then watching `position ls` succeed on the identical book was owed an explanation, not a
/// contradiction.
///
/// `Ok(())` for the DEFAULT (unlabelled) account — `order ls` already reaches that book correctly. A
/// LABELLED book is refused OUTRIGHT rather than silently widened to "every account of this venue":
/// an order row in a pushed `vike_tradehub_client::wire::WireSnapshot`'s `orders`
/// (`vike_tradehub_client::wire::WireOrderView`) names its account only when that account is
/// labelled, and a node that predates the field sends labelled orders without it, so this read
/// cannot tell one account's order row apart from another's on the same venue for certain. A
/// consumer that filtered "as if" it had narrowed would show the operator something they did not
/// ask for — the read-side instance of the misroute design's own law: the destination is what the
/// sender SAID, and where the words admit more than what the data can attribute, it refuses rather
/// than picks.
///
/// ⚠ **THE GUARD IS NOT LIFTED, and that is a ruling rather than an oversight.** This doc used to
/// say the refusal "goes the day `WireOrderView` grows a per-row account field", and the field now
/// exists: `WireOrderView::account` is optional, absent for the default account and present for a
/// labelled one. The refusal stays, because the Trade window work deliberately does not teach this
/// read the field (a follow-up), and for a reason beyond scheduling: an absent `account` means the
/// default account OR a node that predates the field, and only the venue blocks' `mode` tells the
/// two apart (`WireOrderView::account`'s doc), which `order ls` does not read. A labelled book is
/// therefore still refused by the CLI, exactly as before.
///
/// ⚠ **This is NOT a node-capability gap, and the refusal message must never claim it is.** The
/// node's own account-routing capability
/// (`crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_ROUTING`) is served
/// UNCONDITIONALLY by every running `vike-tradehub`
/// (`crates/vike-tradehub/src/server/handshake.rs`'s `served_features`) and already covers the ORDER-WRITE
/// side: `WireOrderRequest` and the account-scoped `WireCommand` variants (`MassCancel`/`Flatten`/
/// `MarketExit`) all carry `account`, and `crates/vike-tradehub-client/src/remote_control.rs`'s
/// `required_feature`/`names_an_account` already gate a write on it. The gap this function guards
/// is narrower and lives entirely on THIS side: `order ls` does not read `WireOrderView::account`,
/// whose absence cannot tell the default account from an older node, so there is no capability for
/// a node string to fill. An earlier version of this message said "no node advertises the
/// account-routing capability", which was false about the fleet and would have sent an operator to
/// upgrade a node that was never the problem — corrected once that was measured against
/// `served_features` directly.
/// ⚠ "Already covers the ORDER-WRITE side" is no longer how the write side is gated: six released
/// nodes advertise `account-routing` while discarding the account, so `required_feature` now asks a
/// labelled `Submit` for `FEATURE_ACCOUNT_SCOPED_SUBMIT` and a labelled reduce for
/// `FEATURE_ACCOUNT_SCOPED_REDUCE`. The conclusion for THIS function stands: its gap is the read
/// row's, and no node string fills it.
///
/// ⚠ **READ-SIDE ONLY — the write path is NOT waiting on this function and must not call it.**
/// ⚠ **CORRECTED TWICE, and the second correction is a DELETION.** This paragraph first said a
/// write consumer PASSES the label through, reasoning that `WireOrderRequest`/the account-scoped
/// `WireCommand` variants already carry `account` and every running node advertises
/// `FEATURE_ACCOUNT_ROUTING` unconditionally — both true, and both one layer short of the actual
/// consumer. It was then corrected to name a WRITE-side twin of this function,
/// `refuse_an_unroutable_account`, which refused every labelled book on the four order/position
/// write verbs because the node's `lower_command` built a `vike_model::OrderRequest` with no
/// `account` field and never read the wire's back. The two were kept as separate functions because
/// they guarded two gaps with two lifetimes — this one goes the day the order read can attribute a
/// row to an account, that one the day the node reads the account back.
///
/// **That twin is DELETED** — stage 5 of
/// `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md`, 2026-09-26,
/// once this branch carried main's node half. Its gap closed on the node, and each of its four
/// cases is now refused somewhere else, never nowhere: a labelled `submit` is refused CLIENT-side
/// against a node that does not advertise
/// `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_SUBMIT`, and against one
/// that does it is ROUTED to the account it names, while an account the node does not hold is
/// refused at the node's own edge, before the Ack (`crates/vike-tradehub/src/server/refusal.rs`'s
/// `account_refusal`). (⚠ This sentence first named only the node's edge. A review measured the
/// release tags and found six released nodes advertising `account-routing` — the string the client
/// gate then trusted — while discarding the account, so against those the deletion had left the
/// `submit` case refused nowhere; the gate's new string closed it.) A labelled `mass-cancel`/
/// `flatten`/`close-all` is refused CLIENT-side, before anything is enqueued, by
/// `crates/vike-tradehub-client/src/remote_control.rs`'s `required_feature`, until a node advertises
/// `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_ACCOUNT_SCOPED_REDUCE`. None of that
/// touches THIS function's gap, which is still open and still retires on its own day.
///
/// **ONE place to delete.** `parse` stays a pure GRAMMAR question — Task 3's own tests fix
/// `parse("binance/ALT")` as a SUCCESS carrying the label, and that must stay true the day this
/// function stops refusing. "Is this well-formed" and "can a read attribute it" are different
/// questions; collapsing them into `parse` would make the grammar lie about itself the moment the
/// read can attribute. So `order ls` calls THIS function rather than writing its own guard, and when
/// the read learns to attribute rows the refusal disappears in one edit here instead of a guard
/// repeated per verb. ⚠ It is **not** "any later READ verb with the same gap" any more —
/// that was true when this doc was first written and stopped being true the moment `position ls`
/// turned out to have a different wire shape underneath it; a future READ verb must check its OWN
/// row type against the wire before assuming this function's gap applies to it too.
pub(crate) fn refuse_an_unaddressable_book(book: &Book) -> Result<(), CliError> {
    if book.label.is_default() {
        return Ok(());
    }
    Err(CliError::refused(format!(
        "'{}' names an account, and order reads cannot attribute a row to one yet: the pushed \
         snapshot's WireOrderView rows name their account only when it is labelled, and a node \
         that predates the field sends labelled orders without it, so nothing was sent and no \
         orders were narrowed. The bare venue (`{}`) reaches every account of it today. The forms \
         are: {FORMS}",
        render(book),
        book.venue,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_venue_is_the_default_account() {
        let b = parse("binance").expect("a bare venue parses");
        assert_eq!(b.venue, "binance");
        assert!(b.label.is_default());
        assert_eq!(render(&b), "binance");
    }

    #[test]
    fn a_label_is_parsed_by_the_model_type_and_not_here() {
        let b = parse("binance/ALT").expect("a labelled selector parses");
        assert_eq!(b.venue, "binance");
        assert_eq!(b.label.text(), Some("ALT"));
        assert_eq!(render(&b), "binance/ALT");
    }

    #[test]
    fn the_reserved_word_names_the_unlabelled_account_positively() {
        let b = parse("binance/DEFAULT").expect("DEFAULT is the wire's positive spelling");
        assert!(b.label.is_default());
    }

    #[test]
    fn an_empty_selector_names_nothing_and_says_what_the_forms_are() {
        let e = parse("").expect_err("an empty selector is a usage error");
        assert!(e.msg.contains(FORMS), "the refusal must append the forms: {}", e.msg);
    }

    #[test]
    fn a_malformed_label_is_refused_in_the_model_types_own_words() {
        // lower case is refused by AccountLabel::parse; this test asserts we do not re-word it.
        let e = parse("binance/alt").expect_err("a lower-case label is refused");
        assert!(!e.msg.is_empty());
        assert!(e.msg.contains(FORMS));
    }

    #[test]
    fn a_mark_is_recognised_as_a_form_rather_than_a_venue() {
        let e = parse("@hedge").expect_err("marks are not built until P3");
        assert!(
            e.msg.contains("mark"),
            "an @-selector must be named as a MARK, never reported as an unknown venue: {}",
            e.msg
        );
    }

    /// ⚠ **The regression this pins: `venue#LABEL` is the WIRE's own `route_key` spelling**
    /// (`vike_model::accounts::account_keys::route_key_of`, published on `WireVenueBlock::route_key` and
    /// recommended verbatim by the MCP surface's own refusal text) — an operator who copies what a
    /// read just showed them types EXACTLY this, and without this guard it silently parses as a
    /// bare venue literally named `"binance#ALT"` with a DEFAULT label, and reaches the wire as a
    /// venue no engine answers to — failing late at the node, or not at all inside the node's own
    /// pre-first-publish routing window. (This said it sailed past `refuse_an_unroutable_account`,
    /// the write-side label refusal stage 5 deleted — that function only ever saw the label, so it
    /// was never what stopped this spelling; this guard is, and it survives the deletion.)
    #[test]
    fn a_route_key_spelling_is_refused_naming_the_slash_form() {
        let e = parse("binance#ALT").expect_err("a route key is not a book selector");
        assert!(e.msg.contains("ROUTE KEY"), "{}", e.msg);
        assert!(
            e.msg.contains("`binance/ALT`"),
            "must name the corrected, slash-spelled form: {}",
            e.msg
        );
        assert!(e.msg.contains(FORMS), "{}", e.msg);
    }

    #[test]
    fn a_selector_with_two_slashes_is_refused_rather_than_truncated() {
        assert!(parse("binance/ALT/extra").is_err());
    }

    #[test]
    fn a_default_book_is_addressable() {
        let b = parse("binance").expect("a bare venue parses");
        assert!(refuse_an_unaddressable_book(&b).is_ok());
        let explicit = parse("binance/DEFAULT").expect("DEFAULT is the wire's positive spelling");
        assert!(refuse_an_unaddressable_book(&explicit).is_ok());
    }

    #[test]
    fn a_labelled_book_is_refused_naming_the_real_gap_and_the_bare_venue() {
        let b = parse("binance/ALT").expect("a labelled selector parses");
        let e =
            refuse_an_unaddressable_book(&b).expect_err("a labelled book cannot be addressed yet");
        assert_eq!(e.exit, crate::exit::Exit::Refused, "{}", e.msg);
        // The real gap is the ORDER read view, never the node's capability — round 2 of the review
        // caught a version of this message that claimed "no node advertises" it, which was false
        // about the fleet (every running vike-tradehub serves it unconditionally).
        assert!(e.msg.contains("WireOrderView"), "{}", e.msg);
        // ⚠ Task 6 fix round 1: `WirePositionView` must NOT be named here any more. It carries no
        // per-row account field, but `position ls` attributes a labelled book anyway — through the
        // enclosing `WireVenueBlock`, which DOES carry `account` — so a message naming both row
        // types would tell an operator `position ls` is refused on the same grounds it just watched
        // succeed on. This message is ORDER-scoped now; see `crate::cmd::trade::position`'s module
        // doc for the position-side evidence.
        assert!(!e.msg.contains("WirePositionView"), "{}", e.msg);
        assert!(
            !e.msg.to_lowercase().contains("advertise"),
            "must not claim a node capability gap: {}",
            e.msg
        );
        assert!(e.msg.contains("nothing was sent"), "{}", e.msg);
        assert!(e.msg.contains("no orders were narrowed"), "{}", e.msg);
        assert!(
            e.msg.contains("`binance`"),
            "names the bare-venue spelling that works today: {}",
            e.msg
        );
        assert!(e.msg.contains(FORMS), "{}", e.msg);
    }

    // ---- the WRITE-side twin's tests went with it -----------------------------------------------
    //
    // Three tests pinned `refuse_an_unroutable_account` here — the default book passing in both
    // directions, and a labelled book refused with a direction-specific remedy. The function is
    // deleted (stage 5 of the order-payload design; see `refuse_an_unaddressable_book`'s own doc),
    // so they are too. What REPLACES them is pinned where the behaviour now lives: the write parsers
    // in `crate::cmd::trade::order`/`crate::cmd::trade::position` thread a labelled book onto the
    // wire (their own unit tests), and `crates/vike-cli/tests/trade_plane_cli.rs` proves over the
    // shipped binary that each case is still refused — by the node's edge for `submit`, by the
    // client's capability gate for the risk-reducing verbs.
}
