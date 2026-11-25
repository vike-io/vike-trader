//! **The BOOK selector — one addressing grammar, one implementation.**
//!
//! A trade request's WHO coordinate is `venue x account`, and the owner ruled it a POSITIONAL
//! first argument rather than two flags: two flags are two things to forget and can be
//! half-given, while one positional cannot be partially specified.
//!
//! This module adds NO second addressing scheme. `vike_model::accounts::account_keys` decided the
//! vocabulary — the label type, its validation, the `parse_wire_account` wire parse and
//! `parse_book_token`, the reading of a token the Telegram channel shares — and every refusal here
//! is that module's own, never re-worded.
//!
//! The precedent for the shape is `crate::cmd::backtest::read::selector`: its `FORMS` const is both the
//! roster and the text every refusal appends, as `BOOK_FORMS` is here, so a form cannot ship without
//! its refusal learning about it.

use vike_model::accounts::account_keys::{AccountLabel, BOOK_FORMS, parse_book_token};

use super::render::OrderRow;
use crate::exit::CliError;

/// One account of one venue — the resolved WHO coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Book {
    pub venue: String,
    pub label: AccountLabel,
}

/// Parse a selector. PURE — no settings, no socket, no store.
pub(crate) fn parse(selector: &str) -> Result<Book, CliError> {
    let (venue, account) = parse_book_token(selector).map_err(CliError::usage)?;
    Ok(Book { venue: venue.to_string(), label: account.unwrap_or_default() })
}

/// Render a book as the operator typed it: the bare venue for the default account.
pub(crate) fn render(book: &Book) -> String {
    match book.label.text() {
        None => book.venue.clone(),
        Some(l) => format!("{}/{l}", book.venue),
    }
}

/// What a WRITE's book says, as the `(venue, account)` it sends. PURE, and [`parse`]'s refusals.
///
/// ⚠ **A bare venue and `<venue>/DEFAULT` are different words on a write** (decision 0254), which [`parse`] erases
/// because a read does not care. `<venue>/<LABEL>` and `<venue>/DEFAULT` name one account. A bare venue names NONE:
/// the node's door takes it only where the venue has one account, running or not and otherwise refuses it, naming the books, and
/// on a lane twin's venue it is the spelling that reaches both lanes of the one account.
pub(crate) fn addressed(selector: &str) -> Result<(String, Option<String>), CliError> {
    let (venue, account) = parse_book_token(selector).map_err(CliError::usage)?;
    Ok((venue.to_string(), account.map(|label| label.to_string())))
}

/// The OPTIONAL book of a risk-reducing write (`mass-cancel`, `close-all`) as the `(venue, account)`
/// it sends ([`addressed`]). PURE. No book is the node-wide panic button and carries neither.
pub(crate) fn optional_book(
    token: Option<&String>,
) -> Result<(Option<String>, Option<String>), CliError> {
    let Some(raw) = token else { return Ok((None, None)) };
    let (venue, account) = addressed(raw)?;
    Ok((Some(venue), account))
}

/// The sentence every write verb's `--help` carries for the rule [`addressed`] sends to the node's door.
pub(crate) const ONE_ACCOUNT_RULE: &str = "A bare <venue> is taken only where the node has ONE account of \
it, running or not (a lane twin is that account's second lane, and a bare venue reaches both); anywhere else the node refuses \
it with nothing sent and names the books to choose from. <venue>/<LABEL> names that account and \
<venue>/DEFAULT the unlabelled one.";

// The command line of a book-scoped `ls` — `trade order ls` and `trade position ls`, the two READ
// verbs whose grammar is "an optional book selector, then `--node`/`--symbol`/`--json`". One
// struct and one [`LsCommand::into_args`] here because those two modules used to carry byte-identical
// copies of both.
//
// The book selector is the one positional. It used to be recognised only as the FIRST token (the
// one that does not start with `-`); `clap` reads it from anywhere among the flags.
#[derive(clap::Args, Debug)]
pub(crate) struct LsCommand {
    #[arg(
        value_name = "ACCOUNT",
        help = "narrow the read: <venue> (every account of it) | <venue>/<LABEL> | <venue>/DEFAULT"
    )]
    book: Option<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address [default: config.node_addr]"
    )]
    node: Option<String>,
    #[arg(long, value_name = "SYM", help = "narrow to one symbol")]
    symbol: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the rows as one JSON array")]
    json: bool,
}

/// The resolved line of a book-scoped `ls`: [`LsCommand`] after its rules ran.
#[derive(Debug)]
pub(crate) struct LsArgs {
    pub node: String,
    /// The optional positional book selector. A READ of a bare venue shows every account of it, each
    /// row naming its own, and only a book naming an account narrows to one ([`Self::named_account`]).
    /// ⚠ A WRITE's bare venue is another word: it names no account at all ([`addressed`]).
    pub book: Option<Book>,
    /// The book was typed with no `/`. [`Book`] cannot say so: `binance` and `binance/DEFAULT` parse
    /// to one label, and a read treats them differently.
    pub bare_venue: bool,
    pub symbol: Option<String>,
    pub json: bool,
}

impl LsArgs {
    /// The book when it names an account, `binance/DEFAULT` included: the one account this read
    /// narrows to. A bare venue answers `None`, every account of it.
    pub(crate) fn named_account(&self) -> Option<&Book> {
        self.book.as_ref().filter(|_| !self.bare_venue)
    }

    /// The `account:` line of a human table, as the book narrowed. [`render`] reads `binance/DEFAULT`
    /// back as the bare venue, which here is every account and not what was read.
    pub(crate) fn book_line(&self) -> Option<String> {
        let b = self.book.as_ref()?;
        let read =
            if self.bare_venue { b.venue.clone() } else { format!("{}/{}", b.venue, b.label) };
        Some(format!("account: {read}"))
    }
}

impl LsCommand {
    /// `ls`'s rules after the parse: the book selector must be a well-formed one (judged by
    /// [`parse`], in its own words), then the node: `--node`, else `configured` (this box's
    /// `config.node_addr`). PURE.
    pub(crate) fn into_args(self, configured: Option<&str>) -> Result<LsArgs, String> {
        let bare_venue = self.book.as_deref().is_some_and(|raw| !raw.contains('/'));
        let book = match self.book {
            Some(raw) => Some(parse(&raw).map_err(|e| e.message)?),
            None => None,
        };
        let node = super::node_or_refusal(self.node, configured)?;
        Ok(LsArgs { node, book, bare_venue, symbol: self.symbol, json: self.json })
    }
}

/// Refuse a book that names an account when an order row of its venue cannot be attributed to one
/// ([`OrderRow::account_known`], off the frame). Narrowing past such a row would show the operator
/// something they did not ask for, or hide an order they did: the destination is what the sender
/// said, and where the data cannot attribute it the read refuses rather than picks.
///
/// Only a node that predates `OrderViewWire::account` can reach it, so it takes the rows and runs
/// after the read, where it used to refuse every labelled book before dialling. It is order-scoped:
/// a position's account comes off its `vike_tradehub_client::wire::VenueBlockWire`, which has no such
/// gap, and a write verb never calls it (the node routes the account a write names). It is no node
/// capability either, so its words never send an operator after a feature string.
pub(crate) fn refuse_an_unaddressable_book(book: &Book, rows: &[OrderRow]) -> Result<(), CliError> {
    if rows.iter().all(|row| row.account_known) {
        return Ok(());
    }
    Err(CliError::refused(format!(
        "'{}/{}' names an account, and this node's frame cannot attribute every order of the venue \
         to one: a node that predates OrderViewWire::account sends labelled orders without it, so \
         no orders were narrowed. The bare venue (`{}`) lists every account of it, each order it \
         cannot attribute marked `?`. The forms are: {BOOK_FORMS}",
        book.venue, book.label, book.venue,
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
        assert!(e.message.contains(BOOK_FORMS), "the refusal must append the forms: {}", e.message);
    }

    #[test]
    fn a_malformed_label_is_refused_in_the_model_types_own_words() {
        // lower case is refused by AccountLabel::parse; this test asserts we do not re-word it.
        let e = parse("binance/alt").expect_err("a lower-case label is refused");
        assert!(!e.message.is_empty());
        assert!(e.message.contains(BOOK_FORMS));
    }

    #[test]
    fn a_mark_is_recognised_as_a_form_rather_than_a_venue() {
        let e = parse("@hedge").expect_err("marks are not built until P3");
        assert!(
            e.message.contains("mark"),
            "an @-selector must be named as a MARK, never reported as an unknown venue: {}",
            e.message
        );
    }

    /// ⚠ **The regression this pins: `venue#LABEL` is the WIRE's own `route_key` spelling**
    /// (`vike_model::accounts::account_keys::route_key_of`, published on `VenueBlockWire::route_key` and
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
        assert!(e.message.contains("ROUTE KEY"), "{}", e.message);
        assert!(
            e.message.contains("`binance/ALT`"),
            "must name the corrected, slash-spelled form: {}",
            e.message
        );
        assert!(e.message.contains(BOOK_FORMS), "{}", e.message);
    }

    #[test]
    fn a_selector_with_two_slashes_is_refused_rather_than_truncated() {
        assert!(parse("binance/ALT/extra").is_err());
    }

    /// [`optional_book`] is what `mass-cancel` and `close-all` each spelled inline: no book sends
    /// neither a venue nor an account, a bare venue names NO account (decision 0254), `/DEFAULT` names
    /// the unlabelled one, a label travels as written, and a malformed book is the grammar's own refusal.
    #[test]
    fn an_optional_book_is_the_venue_and_account_a_reducing_write_sends() {
        let book = |raw: &str| optional_book(Some(&raw.to_string()));
        assert_eq!(optional_book(None).unwrap(), (None, None));
        assert_eq!(book("bybit").unwrap(), (Some("bybit".into()), None));
        assert_eq!(book("bybit/DEFAULT").unwrap(), (Some("bybit".into()), Some("DEFAULT".into())));
        assert_eq!(book("bybit/ALT").unwrap(), (Some("bybit".into()), Some("ALT".into())));
        assert_eq!(book("bybit#ALT").unwrap_err().message, parse("bybit#ALT").unwrap_err().message);
    }

    /// A bare venue and `/DEFAULT` are different words on a write: `addressed` keeps them apart (decision 0254).
    #[test]
    fn addressed_keeps_a_bare_venue_apart_from_the_default_account() {
        assert!(parse("bybit").expect("parses").label.is_default());
        assert_eq!(addressed("bybit").unwrap(), ("bybit".to_string(), None));
    }

    /// What a READ's book narrows to: a bare venue reaches every account of it, and `DEFAULT` names
    /// one account like any label.
    #[test]
    fn only_a_book_naming_an_account_narrows_a_read() {
        let read = |raw: &str| {
            let book = parse(raw).expect("a selector parses");
            LsArgs {
                node: "n:1".into(),
                book: Some(book),
                bare_venue: !raw.contains('/'),
                symbol: None,
                json: false,
            }
        };
        assert!(read("bybit").named_account().is_none());
        assert_eq!(read("bybit").book_line().as_deref(), Some("account: bybit"));
        for raw in ["bybit/DEFAULT", "bybit/LIVE"] {
            let a = read(raw);
            assert_eq!(a.named_account().map(render), a.book.as_ref().map(render), "{raw}");
            assert_eq!(a.book_line(), Some(format!("account: {raw}")));
        }
    }

    /// One row, attributed or not; nothing else of it is read here.
    fn row(account_known: bool) -> OrderRow {
        OrderRow {
            order_client_id: "c".into(),
            venue: "binance".into(),
            account: None,
            account_known,
            symbol: "BTCUSDT".into(),
            side: "BUY",
            qty: 1.0,
            order_type: "limit".into(),
            price: None,
            status: "ACCEPTED".into(),
            filled_qty: 0.0,
            reason: None,
            refusal_id: None,
            release: None,
            waiting: None,
            trail: None,
            trail_percent: None,
            limit_offset: None,
            activation: None,
            attributes: None,
            adjust: None,
            display_quantity: None,
            child_of: None,
        }
    }

    #[test]
    fn a_book_whose_rows_are_all_attributed_is_addressable() {
        for raw in ["binance/DEFAULT", "binance/ALT"] {
            let b = parse(raw).expect("a selector parses");
            assert!(refuse_an_unaddressable_book(&b, &[row(true)]).is_ok(), "{raw}");
            assert!(refuse_an_unaddressable_book(&b, &[]).is_ok(), "{raw}: nothing to attribute");
        }
    }

    #[test]
    fn a_row_the_frame_cannot_attribute_is_refused_naming_the_real_gap_and_the_bare_venue() {
        let b = parse("binance/ALT").expect("a labelled selector parses");
        let e = refuse_an_unaddressable_book(&b, &[row(true), row(false)])
            .expect_err("an unattributed row cannot be narrowed past");
        assert_eq!(e.exit, crate::exit::Exit::Refused, "{}", e.message);
        assert!(e.message.contains("'binance/ALT'"), "{}", e.message);
        // The real gap is the ORDER read view, never the node's capability — round 2 of the review
        // caught a version of this message that claimed "no node advertises" it, which was false
        // about the fleet (every running vike-tradehub serves it unconditionally).
        assert!(e.message.contains("OrderViewWire"), "{}", e.message);
        // ⚠ `PositionViewWire` must NOT be named here: `position ls` attributes a labelled book
        // through the enclosing `VenueBlockWire`, so a message naming both row types would tell an
        // operator `position ls` is refused on grounds it just watched succeed on. Nor may it claim
        // a node capability gap ("no node advertises"). The WHOLE text pins both.
        assert_eq!(
            e.message,
            format!(
                "'binance/ALT' names an account, and this node's frame cannot attribute every \
                 order of the venue to one: a node that predates OrderViewWire::account sends \
                 labelled orders without it, so no orders were narrowed. The bare venue \
                 (`binance`) lists every account of it, each order it cannot attribute marked \
                 `?`. The forms are: {BOOK_FORMS}"
            )
        );
        assert!(e.message.contains("no orders were narrowed"), "{}", e.message);
        assert!(
            e.message.contains("`binance`"),
            "names the bare-venue spelling that works today: {}",
            e.message
        );
        assert!(e.message.contains(BOOK_FORMS), "{}", e.message);
    }

    // ---- the WRITE-side twin's tests went with it -----------------------------------------------
    //
    // Three tests pinned `refuse_an_unroutable_account` here — the default book passing in both
    // directions, and a labelled book refused with a direction-specific remedy. The function is
    // deleted (stage 5 of the order-payload design; see `refuse_an_unaddressable_book`'s own doc),
    // so they are too. What REPLACES them is pinned where the behaviour now lives: the write parsers
    // in `crate::cmd::trade::order`/`crate::cmd::trade::position` thread a labelled book onto the
    // wire (their own unit tests), and `crates/vike-cli/tests/shipped_cli/trade_plane_cli.rs` proves over the
    // shipped binary that each case is still refused — by the node's edge for `submit`, by the
    // client's capability gate for the risk-reducing verbs.
}
