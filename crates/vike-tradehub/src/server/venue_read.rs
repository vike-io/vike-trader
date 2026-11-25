//! `server::venue_read` — the body of the `Request::VenueRead` arm: an operator's READ of one account at its
//! venue (its balance with the allowance each exchange contract holds, the venue's own list of its open
//! orders, or the positions it holds), asked over the account's OPEN trade lease. Split out of `session.rs` so the whole reply is decidable
//! without a socket.
//!
//! # What it is, and what it is not
//!
//! The node holds no venue connection and no credential but the gatewayhub's Trade key (decision 0164), so it
//! cannot read a venue itself. It forwards the read to the one place that can: the account's lease. The lease
//! registry ([`super::symbol_admission::VenueReads::venue_read`], production: `TradeLeaseRegistry`) picks the asking
//! account's `TradeLease` by route key, the session sends `TradeUp::VenueRead`, and the gatewayhub's bridge
//! asks the venue with the account's own credentials at the account's own tier.
//!
//! * **It writes and folds nothing.** No order is placed, moved or cancelled, no event reaches the core, the
//!   journal or the recent-events ring, no symbol is admitted and the daemon's reconcile client is told
//!   nothing. The answer is kept by nothing: the next ask is a new ask.
//! * **It is no reconcile pass.** A reconcile fetch folds what it finds and is gated by `flags.poly_reconcile`
//!   and the policy row; a read the operator asks for is gated by neither (a Polymarket account with the flag
//!   unset, which is what the 2026-10-27 order test found, is as readable as any), and it needs no adapter
//!   started on the gatewayhub, only the lease the account's start opened. An account that is not running here
//!   has no lease: it is told so, by name, and nothing else is tried (the node never opens a lease of its own for
//!   a read; that would claim the account's process-wide lock).
//! * **It is bounded where the venue is.** The gatewayhub holds the read to one in flight per venue and a gap
//!   between two reads of one account and kind, and answers a refused one in words without touching the venue
//!   (`crates/vike-gatewayhub/CLAUDE.md`). This arm adds no gate of its own and takes no control rate token: a
//!   read is not a control command, and the connection-local bucket would only bound a peer that stays
//!   connected. The wait on the lease is `VENUE_READ_BOUND`, below the client's own one-shot wait.
//! * **Our own orders are named by our order client id.** A venue that lists our order under a wire id that is not the
//!   order client id (hyperliquid's cloid) has the row joined to the account's live orders through the lease's
//!   `TradeUp::WireIds` ([`name_our_orders`]); a row that names none of them stays as the venue listed it.
//!
//! # Why `Scope::Read` (the OBSERVE key) and not `Scope::Write`
//!
//! The scope split is "who may change the node": a Control peer can place orders, flatten, mount a script and
//! write settings; an Observe peer reads. This verb changes nothing at the venue, in the book or on disk, so
//! by the repo's own rule it belongs with the reads, beside `trade reconcile ls`, `Directory` and `StrategyStatus`.
//! The data does not demand the stronger key either, because an Observe peer already holds its classes:
//!
//! * **the balance**: every published frame carries each running account's venue block with its balance and
//!   equity; the new read adds, on Polymarket, the per-contract allowances, which are public chain data (a spender
//!   address and an amount), and the asset word;
//! * **the open orders**: every frame carries the order registry, and the held divergences
//!   (`CoreSnapshotWire::recon`, which `trade reconcile ls` prints to the same key) name the venue's orders this node
//!   does not know; the new read is the venue's complete list, fresh, for an account whose reconcile pass is off;
//! * **the positions**: every frame carries the engine's positions; the new read is the venue's own list, fresh
//!   and complete, for an account whose reconcile pass is off, and it states no address: Polymarket's wallet
//!   holdings from its public data-api (anyone may ask it for any address), Deribit's `private/get_positions`.
//!
//! What it does cost is the venue's request budget, which is why it is bounded above and not by the key. No
//! credential, key name or account identifier the reply could leak rides it: the wire types carry the figures
//! and the order rows the venue lists, and the funder or master address the venue read was made for is not
//! among them. A peer that holds only the Observe key therefore learns nothing it could not already learn,
//! only fresher and for the account it names. Decision 0210 records the choice and what would reopen it.
//!
//! # Refusals are words
//!
//! Every refusal is in the operator's sentence, never an empty answer: an empty
//! `OpenOrders` list is an ANSWER ("nothing rests at the venue"), so a read that could not be made must never
//! look like one. This node's own refusals (a venue off the roster, a bad account word, no lease registry, a figure
//! that is not a number) are the refusal frame of a [`VenueReadRefusal`]; a bridge's refusal that declares an id
//! (`ibkr.venue_read.…`, carried over the lease in `TradeDown::VenueReadReply`'s `refusal_id`) is the refusal
//! frame under THAT id, so it is shown once; the rest are a [`Response::Error`] in the
//! lease's and the gatewayhub's own sentences: no lease (not running here), a
//! gatewayhub that predates the read (the client refused it before sending anything), `not offered by <venue>`
//! (a bridge with no account-wide read of that kind; only Polymarket, Hyperliquid, Deribit, Bybit (the balance and the
//! open orders), Binance, OKX, IG, cTrader, OANDA, Alpaca, Dukascopy, IBKR's socket backend and FXCM (the open orders)
//! answer), asked too soon, or the venue's refusal (the status head only, never a body).

use vike_execution::OrderView;
use vike_hub_protocol::refusal::HubRefusal;
use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};
use vike_model::{VENUES, VenueRead, VenueReadKind, VenueReadOpenOrder};
use vike_tradehub_client::proto::Response;
use vike_utility::errors::Declared;

use super::symbol_admission::VenueReads;
use crate::refusals::VenueReadRefusal;

/// How many characters of a peer's venue or account an error echoes back.
const ECHO_LIMIT: usize = 64;

/// `text` as an error echoes it: bounded, so a peer cannot make the node print a megabyte string it chose.
fn echoed(text: &str) -> String {
    let head: String = text.chars().take(ECHO_LIMIT).collect();
    if text.chars().count() > ECHO_LIMIT { format!("{head}...") } else { head }
}

/// **Our order client id on our own orders in an open-orders answer**, for a venue whose wire id is not the order client id
/// (hyperliquid lists `keccak(order client id)` cut to 128 bits as the order's client id). Only when a listed client id
/// is none of the account's live order client ids are those order client ids asked for their wire ids over the lease
/// ([`VenueReads::wire_ids`], the boot prefetch's `TradeUp::WireIds`), and a row listed under one of them is
/// named by its order client id. Everything else stays as the venue listed it: a foreign order, one of ours this node no
/// longer holds live, every row of a venue that echoes the order client id (deribit: nothing is asked), and every row
/// when the ask fails (the read is still the venue's answer).
fn name_our_orders(
    read: &mut VenueRead,
    source: &dyn VenueReads,
    route_key: &str,
    ours: impl FnOnce() -> Vec<OrderView>,
) {
    let VenueRead::OpenOrders { orders, .. } = read else { return };
    let order_client_ids: Vec<String> = ours()
        .into_iter()
        .filter(|o| {
            o.status.is_live()
                && route_key_of(&o.venue, &o.account.clone().unwrap_or_default()) == route_key
        })
        .map(|o| o.order_client_id)
        .collect();
    let unnamed = |o: &VenueReadOpenOrder| {
        o.order_client_id.as_ref().is_some_and(|c| !order_client_ids.contains(c))
    };
    if order_client_ids.is_empty() || !orders.iter().any(unnamed) {
        return;
    }
    let pairs = match source.wire_ids(route_key, &order_client_ids) {
        Ok(pairs) => pairs,
        Err(why) => {
            tracing::warn!(%route_key, %why, "vike-tradehub: the venue's open orders are shown under the venue's ids");
            return;
        }
    };
    for row in orders.iter_mut() {
        let hit = pairs.iter().find(|(_, wire)| row.order_client_id.as_ref() == Some(wire));
        if let Some((order_client_id, _)) = hit {
            row.order_client_id = Some(order_client_id.clone());
        }
    }
}

/// **The reply to one `Request::VenueRead`**: the venue's answer for `venue`'s `account`, or the operator's
/// sentence for why there is none. `source` is the publisher's lease registry (`None`: this node has none,
/// which is a node not running behind a gatewayhub); `ours` the node's published orders, read only for an
/// open-orders answer ([`name_our_orders`]).
///
/// Blocking (a venue round trip, bounded by the lease registry): called on the connection's own thread, never on
/// the core's.
pub(super) fn venue_read_reply(
    source: Option<&dyn VenueReads>,
    venue: &str,
    account: &str,
    kind: VenueReadKind,
    ours: impl FnOnce() -> Vec<OrderView>,
) -> Response {
    if !VENUES.contains(&venue) {
        let (venue, roster) = (echoed(venue), VENUES.join(", "));
        return Response::Refused(VenueReadRefusal::UnknownVenue { venue, roster }.into());
    }
    let label = match parse_wire_account(account) {
        Ok(label) => label,
        Err(e) => {
            let (account, why) = (echoed(account), e.to_string());
            return Response::Refused(VenueReadRefusal::BadAccount { account, why }.into());
        }
    };
    let route_key = route_key_of(venue, &label);
    tracing::info!(
        %route_key, kind = kind.word(),
        "vike-tradehub: venue read asked by a peer (nothing is placed, cancelled or folded)"
    );
    let Some(source) = source else {
        return Response::Refused(VenueReadRefusal::NoLeaseRegistry { route_key }.into());
    };
    match source.venue_read(&route_key, kind) {
        Ok(read) if !read.is_finite() => {
            let kind = kind.word();
            Response::Refused(VenueReadRefusal::NotANumber { route_key, kind }.into())
        }
        Ok(mut read) => {
            name_our_orders(&mut read, source, &route_key, ours);
            Response::VenueRead(Box::new(read))
        }
        Err(why) => match why.declared_id() {
            // A bridge's refusal, under its own id: the one a person reads.
            Some(id) => Response::Refused(HubRefusal::new(id, &why)),
            // The lease's and the gatewayhub's own words, passed on whole: they declare no id of this node's.
            None => Response::Error(why.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use vike_model::OrderStatus::{Accepted, Filled};
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_model::{OrderStatus, VenueReadAllowance, VenueReadBalance};

    /// [`super::venue_read_reply`] for a node that holds no orders of its own: every test but the join's.
    fn venue_read_reply(
        source: Option<&dyn VenueReads>,
        venue: &str,
        account: &str,
        kind: VenueReadKind,
    ) -> Response {
        super::venue_read_reply(source, venue, account, kind, Vec::new)
    }

    /// A registry that answers `answer` for every route key and remembers the one it was asked. Its wire ids are
    /// the `wire` pairs of the order client ids asked (a pure function of the order client id, as the venue's are), or `wire`'s error.
    struct Answers {
        answer: Result<VenueRead, vike_utility::errors::CarriedRefusal>,
        asked: std::sync::Mutex<Vec<(String, VenueReadKind)>>,
        wire: Result<Vec<(String, String)>, String>,
        wire_asked: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    }

    impl Answers {
        fn new(answer: Result<VenueRead, vike_utility::errors::CarriedRefusal>) -> Self {
            Answers {
                answer,
                asked: std::sync::Mutex::new(Vec::new()),
                wire: Ok(Vec::new()),
                wire_asked: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn with_wire(self, wire: Result<Vec<(&str, &str)>, String>) -> Self {
            let owned = |pairs: Vec<(&str, &str)>| -> Vec<(String, String)> {
                pairs.into_iter().map(|(c, w)| (c.to_string(), w.to_string())).collect()
            };
            Answers { wire: wire.map(owned), ..self }
        }
    }

    impl VenueReads for Answers {
        fn venue_read(
            &self,
            route_key: &str,
            kind: VenueReadKind,
        ) -> Result<VenueRead, vike_utility::errors::CarriedRefusal> {
            self.asked.lock().unwrap().push((route_key.to_string(), kind));
            self.answer.clone()
        }
        fn wire_ids(
            &self,
            route_key: &str,
            order_client_ids: &[String],
        ) -> Result<Vec<(String, String)>, String> {
            self.wire_asked
                .lock()
                .unwrap()
                .push((route_key.to_string(), order_client_ids.to_vec()));
            let wire = self.wire.clone()?;
            Ok(wire.into_iter().filter(|(c, _)| order_client_ids.contains(c)).collect())
        }
    }

    /// Our order `pd4hl1` as hyperliquid lists it: `keccak("pd4hl1")` cut to 128 bits.
    const PD4HL1_CLOID: &str = "0x08772b4c8d06c0325d398b63d115b008";
    /// A cloid no order of this node has (one placed in the venue's own UI by another tool).
    const FOREIGN_CLOID: &str = "0x00000000000000000000000000abcdef";

    /// One resting row as the venue lists it, under `order_client_id`.
    fn listed(order_client_id: Option<&str>) -> VenueReadOpenOrder {
        VenueReadOpenOrder {
            venue_order_id: "168136755771512".into(),
            order_client_id: order_client_id.map(str::to_string),
            symbol: "BTC".into(),
            side: 1,
            price: Some(60_700.0),
            trigger_price: None,
            order_type: "limit".into(),
            qty: 0.0002,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            timestamp_ms: 1_761_279_876_000,
        }
    }

    fn book(rows: Vec<VenueReadOpenOrder>) -> VenueRead {
        VenueRead::OpenOrders { orders: rows, truncated: false }
    }

    /// One order of the node's own book, as the publisher holds it.
    fn ours(
        order_client_id: &str,
        venue: &str,
        account: Option<&str>,
        status: OrderStatus,
    ) -> OrderView {
        OrderView {
            data_delay: None,
            order_client_id: order_client_id.to_string(),
            venue: venue.to_string(),
            account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
            symbol: "BTC".to_string(),
            side: 1,
            qty: 0.0002,
            order_type: vike_model::OrderType::Limit,
            price: Some(60_700.0),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            average_fill_price: 0.0,
            refusal_id: None,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        }
    }

    /// The client ids of an open-orders reply, row for row.
    fn listed_ids(reply: Response) -> Vec<Option<String>> {
        let Response::VenueRead(read) = reply else { panic!("expected a read, got {reply:?}") };
        let VenueRead::OpenOrders { orders, .. } = *read else {
            panic!("an open-orders answer stays one")
        };
        orders.into_iter().map(|o| o.order_client_id).collect()
    }

    fn some(id: &str) -> Option<String> {
        Some(id.to_string())
    }

    /// **Our order comes back under our order client id** (`vike-cli trade venue open-orders hyperliquid` showed
    /// `0x08772b4c...` for `pd4hl1`): the account's LIVE order client ids are asked for their wire ids, a row listed under
    /// one is named by the order client id, and a foreign cloid and a row with no client id stay as the venue listed them.
    /// A filled order, another account's and another venue's are not asked about.
    #[test]
    fn our_order_listed_under_its_wire_id_comes_back_with_our_order_client_id() {
        let rows =
            book(vec![listed(Some(PD4HL1_CLOID)), listed(Some(FOREIGN_CLOID)), listed(None)]);
        let source = Answers::new(Ok(rows)).with_wire(Ok(vec![
            ("pd4hl1", PD4HL1_CLOID),
            ("pd4hl0", "0x0000000000000000000000000000dead"),
        ]));
        let node = vec![
            ours("pd4hl1", "hyperliquid", None, Accepted),
            ours("pd4hl0", "hyperliquid", None, Filled),
            ours("pd4alt", "hyperliquid", Some("ALT"), Accepted),
            ours("bn1", "binance", None, Accepted),
        ];
        let reply = super::venue_read_reply(
            Some(&source),
            "hyperliquid",
            "DEFAULT",
            VenueReadKind::OpenOrders,
            || node,
        );
        assert_eq!(listed_ids(reply), [some("pd4hl1"), some(FOREIGN_CLOID), None]);
        assert_eq!(
            *source.wire_asked.lock().unwrap(),
            [("hyperliquid".to_string(), vec!["pd4hl1".to_string()])],
            "only the account's live order client ids are asked, under its route key"
        );

        // A labelled account's own orders are its own: `hyperliquid#ALT` asks for `pd4alt` alone.
        let source = Answers::new(Ok(book(vec![listed(Some("0xa17"))])))
            .with_wire(Ok(vec![("pd4alt", "0xa17")]));
        let node = vec![
            ours("pd4hl1", "hyperliquid", None, Accepted),
            ours("pd4alt", "hyperliquid", Some("ALT"), Accepted),
        ];
        let reply = super::venue_read_reply(
            Some(&source),
            "hyperliquid",
            "ALT",
            VenueReadKind::OpenOrders,
            || node,
        );
        assert_eq!(listed_ids(reply), [some("pd4alt")]);
        assert_eq!(
            *source.wire_asked.lock().unwrap(),
            [("hyperliquid#ALT".to_string(), vec!["pd4alt".to_string()])]
        );
    }

    /// **A venue whose id IS the order client id is unchanged** (deribit lists our order client id as the order's label): every listed
    /// id is already one of ours, so nothing is asked and every row stays as listed. A balance read reads no
    /// orders of the node at all.
    #[test]
    fn a_venue_whose_id_is_the_order_client_id_is_left_as_listed_and_asked_nothing() {
        let source = Answers::new(Ok(book(vec![listed(Some("db1")), listed(None)])));
        let reply = super::venue_read_reply(
            Some(&source),
            "deribit",
            "DEFAULT",
            VenueReadKind::OpenOrders,
            || vec![ours("db1", "deribit", None, Accepted)],
        );
        assert_eq!(listed_ids(reply), [some("db1"), None]);
        assert!(source.wire_asked.lock().unwrap().is_empty(), "nothing was asked");

        let source = Answers::new(Ok(balance()));
        let reply = super::venue_read_reply(
            Some(&source),
            "deribit",
            "DEFAULT",
            VenueReadKind::Balance,
            || panic!("a balance read reads no orders"),
        );
        assert!(matches!(reply, Response::VenueRead(_)), "{reply:?}");
    }

    /// **A foreign order stays as the venue gave it**, and so does every row when the wire ids cannot be had: the
    /// read is still the venue's answer, never an error.
    #[test]
    fn a_foreign_wire_id_and_a_failed_ask_leave_the_rows_as_listed() {
        let source = Answers::new(Ok(book(vec![listed(Some(FOREIGN_CLOID))])))
            .with_wire(Ok(vec![("pd4hl1", PD4HL1_CLOID)]));
        let reply = super::venue_read_reply(
            Some(&source),
            "hyperliquid",
            "DEFAULT",
            VenueReadKind::OpenOrders,
            || vec![ours("pd4hl1", "hyperliquid", None, Accepted)],
        );
        assert_eq!(listed_ids(reply), [some(FOREIGN_CLOID)]);

        let source = Answers::new(Ok(book(vec![listed(Some(PD4HL1_CLOID))])))
            .with_wire(Err("the trade link is down".into()));
        let reply = super::venue_read_reply(
            Some(&source),
            "hyperliquid",
            "DEFAULT",
            VenueReadKind::OpenOrders,
            || vec![ours("pd4hl1", "hyperliquid", None, Accepted)],
        );
        assert_eq!(listed_ids(reply), [some(PD4HL1_CLOID)]);
    }

    fn balance() -> VenueRead {
        VenueRead::Balance(VenueReadBalance {
            asset: "pUSD".into(),
            balance: 20.0,
            allowances: vec![VenueReadAllowance {
                spender: "0xE111".into(),
                name: "CTF exchange".into(),
                amount: 0.0,
                unlimited: true,
            }],
            other_assets: Vec::new(),
        })
    }

    /// The words of a refusal: this node's own frame, or the lease's `Error`.
    fn error_text(response: Response) -> String {
        match response {
            Response::Error(text) => text,
            Response::Refused(frame) => frame.text,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// The account's route key is the bare venue for the unlabelled account and `venue#LABEL` for a labelled
    /// one: the registry is asked under exactly the key the lease was registered by, and under the kind the
    /// operator asked.
    #[test]
    fn the_registry_is_asked_under_the_accounts_route_key_and_the_asked_kind() {
        let source = Answers::new(Ok(balance()));
        for (account, want) in [("DEFAULT", "polymarket"), ("ALT", "polymarket#ALT")] {
            let reply =
                venue_read_reply(Some(&source), "polymarket", account, VenueReadKind::Balance);
            assert!(matches!(reply, Response::VenueRead(_)), "{reply:?}");
            let asked = source.asked.lock().unwrap().pop().expect("asked");
            assert_eq!(asked, (want.to_string(), VenueReadKind::Balance));
        }
        let _ = venue_read_reply(Some(&source), "polymarket", "DEFAULT", VenueReadKind::OpenOrders);
        assert_eq!(source.asked.lock().unwrap()[0].1, VenueReadKind::OpenOrders);
    }

    /// Both answers reach the reply as the venue gave them, every field included (the wire carries the
    /// model's type itself).
    #[test]
    fn the_answer_crosses_as_the_venue_gave_it() {
        let orders = VenueRead::OpenOrders {
            orders: vec![VenueReadOpenOrder {
                venue_order_id: "0xabc".into(),
                order_client_id: Some("pm1".into()),
                symbol: "6710".into(),
                side: -1,
                price: None,
                trigger_price: Some(1.2412),
                order_type: "stop".into(),
                qty: 5.0,
                filled_qty: 1.5,
                status: "PARTIALLY_FILLED".into(),
                timestamp_ms: 1_700_000_000_000,
            }],
            truncated: true,
        };
        for answer in [orders, balance()] {
            let source = Answers::new(Ok(answer.clone()));
            let reply = venue_read_reply(Some(&source), "polymarket", "DEFAULT", answer.kind());
            let Response::VenueRead(read) = reply else { panic!("expected a read, got {reply:?}") };
            assert_eq!(*read, answer);
        }
    }

    /// The positions kind reaches the registry as itself and its rows cross the wire field for field, the
    /// redeemable flag and the venue's price and value included; an empty wallet stays an empty ANSWER.
    #[test]
    fn a_positions_answer_crosses_field_for_field() {
        let held = VenueRead::Positions {
            positions: vec![vike_model::VenueReadPosition {
                symbol: "1484".into(),
                title: "Solana Up or Down".into(),
                outcome: "Down".into(),
                qty: 11.11,
                average_price: Some(0.2099),
                price: None,
                value: Some(0.0),
                redeemable: true,
            }],
            truncated: true,
        };
        let source = Answers::new(Ok(held.clone()));
        let reply =
            venue_read_reply(Some(&source), "polymarket", "DEFAULT", VenueReadKind::Positions);
        assert_eq!(source.asked.lock().unwrap()[0].1, VenueReadKind::Positions);
        let Response::VenueRead(read) = reply else { panic!("expected a read, got {reply:?}") };
        let VenueRead::Positions { positions, truncated } = *read else {
            panic!("a positions answer stays one")
        };
        assert!(truncated);
        let p = &positions[0];
        assert_eq!(
            (p.symbol.as_str(), p.title.as_str(), p.outcome.as_str()),
            ("1484", "Solana Up or Down", "Down")
        );
        assert_eq!(
            (p.qty, p.average_price, p.price, p.value),
            (11.11, Some(0.2099), None, Some(0.0))
        );
        assert!(p.redeemable);

        let empty = VenueRead::Positions { positions: Vec::new(), truncated: false };
        let source = Answers::new(Ok(empty));
        let reply =
            venue_read_reply(Some(&source), "polymarket", "DEFAULT", VenueReadKind::Positions);
        let Response::VenueRead(read) = reply else { panic!("an empty wallet is an answer") };
        assert_eq!(*read, VenueRead::Positions { positions: Vec::new(), truncated: false });
    }

    /// A positions figure that is not a number is turned into an error in words too (a `NaN` writes as
    /// `null` and ends the connection that decodes it).
    #[test]
    fn a_positions_figure_that_is_not_a_number_is_an_error_in_words() {
        let nan = VenueRead::Positions {
            positions: vec![vike_model::VenueReadPosition {
                symbol: "1".into(),
                title: String::new(),
                outcome: String::new(),
                qty: f64::NAN,
                average_price: None,
                price: None,
                value: None,
                redeemable: false,
            }],
            truncated: false,
        };
        let source = Answers::new(Ok(nan));
        let text = error_text(venue_read_reply(
            Some(&source),
            "polymarket",
            "DEFAULT",
            VenueReadKind::Positions,
        ));
        assert!(text.contains("positions answer carried a figure that is not a number"), "{text}");
    }

    /// A refusal is the operator's sentence, verbatim, as an error and never as an empty answer.
    #[test]
    fn a_refusal_is_the_lease_and_the_daemons_words() {
        let sentence = "not offered by aster: its bridge has no account-wide open-orders read";
        let source = Answers::new(Err(sentence.into()));
        let reply = venue_read_reply(Some(&source), "aster", "DEFAULT", VenueReadKind::OpenOrders);
        assert_eq!(error_text(reply), sentence);
    }

    /// A bridge's refusal that declares an id is the refusal FRAME under that id (W3 carried it over the lease),
    /// its text the bare sentence, so a client shows ONE id; an older client reads today's `Error` text.
    #[test]
    fn a_bridge_refusal_with_an_id_is_the_refusal_frame_under_that_id() {
        let sentence = "ibkr: the account's gateway session is not logged in";
        let source = Answers::new(Err(vike_utility::errors::CarriedRefusal::new(
            Some("ibkr.venue_read.not_logged_in".into()),
            sentence.into(),
        )));
        let reply = venue_read_reply(Some(&source), "ibkr", "DEFAULT", VenueReadKind::OpenOrders);
        let frame = HubRefusal::new("ibkr.venue_read.not_logged_in", sentence);
        assert_eq!(reply, Response::Refused(frame));
        assert_eq!(reply.older_form(false), Some(Response::Error(sentence.to_string())));
    }

    /// A figure that is not a number would write as `null` and end the connection that decodes it; the node
    /// turns it into an error in words (the gatewayhub does the same before it writes).
    #[test]
    fn a_figure_that_is_not_a_number_is_an_error_in_words() {
        let mut read = balance();
        if let VenueRead::Balance(b) = &mut read {
            b.balance = f64::NAN;
        }
        let source = Answers::new(Ok(read));
        let text = error_text(venue_read_reply(
            Some(&source),
            "polymarket",
            "DEFAULT",
            VenueReadKind::Balance,
        ));
        assert!(text.contains("not a number"), "{text}");
    }

    /// A peer's strings are judged before any lease is asked: an unknown venue and a malformed account are
    /// refused by name, bounded in what they echo, and the registry is asked nothing.
    #[test]
    fn a_bad_venue_or_account_is_refused_before_anything_is_asked() {
        let source = Answers::new(Ok(balance()));
        let unknown =
            venue_read_reply(Some(&source), "nosuchvenue", "DEFAULT", VenueReadKind::Balance);
        let text = error_text(unknown);
        assert!(text.contains("not a venue of this node's roster") && text.contains("polymarket"));
        let bad = error_text(venue_read_reply(
            Some(&source),
            "polymarket",
            "not a label!",
            VenueReadKind::Balance,
        ));
        assert!(bad.contains("not an account name"), "{bad}");
        let long = "x".repeat(10_000);
        let echoed_long =
            error_text(venue_read_reply(Some(&source), &long, "DEFAULT", VenueReadKind::Balance));
        assert!(echoed_long.len() < 400, "the echo is bounded: {} bytes", echoed_long.len());
        assert!(source.asked.lock().unwrap().is_empty(), "nothing was asked of the lease registry");
    }

    /// A node with no lease registry says so, by account, and asks nothing.
    #[test]
    fn a_node_with_no_lease_registry_says_so() {
        let text =
            error_text(venue_read_reply(None, "polymarket", "DEFAULT", VenueReadKind::Balance));
        assert!(text.contains("polymarket:") && text.contains("no trade lease registry"), "{text}");
    }

    /// **This node's own refusal is the frame of its id**, and a person reads it as `error [<id>]: <text>`; an
    /// older client is sent the `Error` of the same words.
    #[test]
    fn a_node_refusal_is_the_frame_of_its_id() {
        let reply = venue_read_reply(None, "polymarket", "DEFAULT", VenueReadKind::Balance);
        let Response::Refused(frame) = &reply else { panic!("a frame: {reply:?}") };
        let refusal = VenueReadRefusal::NoLeaseRegistry { route_key: "polymarket".into() };
        assert_eq!(frame.id, "tradehub.venue_read.no_lease_registry");
        assert_eq!(frame.text, refusal.to_string());
        assert_eq!(
            refusal.rendered(),
            "error [tradehub.venue_read.no_lease_registry]: polymarket: this node holds no trade lease \
             registry (it is not running behind a gatewayhub), so there is no path to the venue to read; \
             nothing was asked"
        );
        assert_eq!(reply.older_form(false), Some(Response::Error(frame.text.clone())));
    }
}
