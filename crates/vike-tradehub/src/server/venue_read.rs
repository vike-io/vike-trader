//! `server::venue_read` — the body of the `Request::VenueRead` arm: an operator's READ of one account at its
//! venue (its balance with the allowance each exchange contract holds, the venue's own list of its open
//! orders, or the positions it holds), asked over the account's OPEN trade lease. Split out of `session.rs` so the whole reply is decidable
//! without a socket, like [`super::tearsheet`].
//!
//! # What it is, and what it is not
//!
//! The node holds no venue connection and no credential but the gatewayhub's Trade key (decision 0164), so it
//! cannot read a venue itself. It forwards the read to the one place that can: the account's lease. The lease
//! registry ([`super::symbol_admission::SymbolFacts::venue_read`], production: `LeaseFacts`) picks the asking
//! account's `TradeSession` by route key, the session sends `TradeUp::VenueRead`, and the gatewayhub's bridge
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
//!
//! # Why `Scope::Read` (the OBSERVE key) and not `Scope::Write`
//!
//! The scope split is "who may change the node": a Control peer can place orders, flatten, mount a script and
//! write settings; an Observe peer reads. This verb changes nothing at the venue, in the book or on disk, so
//! by the repo's own rule it belongs with the reads, beside `trade recon ls`, `Directory` and `StrategyStatus`.
//! The data does not demand the stronger key either, because an Observe peer already holds its classes:
//!
//! * **the balance**: every published frame carries each running account's venue block with its balance and
//!   equity; the new read adds the venue's per-contract allowances, which are public chain data (a spender
//!   address and an amount), and the asset word;
//! * **the open orders**: every frame carries the order registry, and the held divergences
//!   (`WireSnapshot::recon`, which `trade recon ls` prints to the same key) name the venue's orders this node
//!   does not know; the new read is the venue's complete list, fresh, for an account whose reconcile pass is off;
//! * **the positions**: every frame carries the engine's positions; the new read is the wallet's holdings as the
//!   venue's public data-api lists them (anyone may ask it for any address), fresh and complete, for an account
//!   whose reconcile pass is off, and it states no address.
//!
//! What it does cost is the venue's request budget, which is why it is bounded above and not by the key. No
//! credential, key name or account identifier the reply could leak rides it: the wire types carry the figures
//! and the order rows the venue lists, and the funder or master address the venue read was made for is not
//! among them. A peer that holds only the Observe key therefore learns nothing it could not already learn,
//! only fresher and for the account it names. Decision 0210 records the choice and what would reopen it.
//!
//! # Refusals are words
//!
//! Every refusal is a [`Response::Error`] in the operator's sentence, never an empty answer: an empty
//! `OpenOrders` list is an ANSWER ("nothing rests at the venue"), so a read that could not be made must never
//! look like one. The sentences are the lease's and the gatewayhub's own: no lease (not running here), a
//! gatewayhub that predates the read (the client refused it before sending anything), `not offered by <venue>`
//! (a bridge with no account-wide read; only Polymarket and Hyperliquid answer), asked too soon, or the venue's
//! refusal (the status head only, never a body).

use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};
use vike_model::{VENUES, VenueRead, VenueReadKind};
use vike_tradehub_client::proto::Response;
use vike_tradehub_client::wire::{
    WireVenueAllowance, WireVenueBalance, WireVenueOpenOrder, WireVenuePosition, WireVenueRead,
    WireVenueReadKind,
};

use super::symbol_admission::SymbolFacts;

/// How many characters of a peer's venue or account an error echoes back.
const ECHO_LIMIT: usize = 64;

/// `text` as an error echoes it: bounded, so a peer cannot make the node print a megabyte string it chose.
fn echoed(text: &str) -> String {
    let head: String = text.chars().take(ECHO_LIMIT).collect();
    if text.chars().count() > ECHO_LIMIT { format!("{head}...") } else { head }
}

/// The model's kind for the wire's.
fn model_kind(kind: WireVenueReadKind) -> VenueReadKind {
    match kind {
        WireVenueReadKind::Balance => VenueReadKind::Balance,
        WireVenueReadKind::OpenOrders => VenueReadKind::OpenOrders,
        WireVenueReadKind::Positions => VenueReadKind::Positions,
    }
}

/// The wire's mirror of the gatewayhub's answer, field for field.
pub(super) fn wire_of(read: VenueRead) -> WireVenueRead {
    match read {
        VenueRead::Balance(b) => WireVenueRead::Balance(WireVenueBalance {
            asset: b.asset,
            balance: b.balance,
            allowances: b
                .allowances
                .into_iter()
                .map(|a| WireVenueAllowance {
                    spender: a.spender,
                    name: a.name,
                    amount: a.amount,
                    unlimited: a.unlimited,
                })
                .collect(),
        }),
        VenueRead::OpenOrders { orders, truncated } => WireVenueRead::OpenOrders {
            orders: orders
                .into_iter()
                .map(|o| WireVenueOpenOrder {
                    venue_order_id: o.venue_order_id,
                    client_order_id: o.client_order_id,
                    symbol: o.symbol,
                    side: o.side,
                    price: o.price,
                    qty: o.qty,
                    filled_qty: o.filled_qty,
                    status: o.status,
                    ts_ms: o.ts_ms,
                })
                .collect(),
            truncated,
        },
        VenueRead::Positions { positions, truncated } => WireVenueRead::Positions {
            positions: positions
                .into_iter()
                .map(|p| WireVenuePosition {
                    symbol: p.symbol,
                    title: p.title,
                    outcome: p.outcome,
                    qty: p.qty,
                    avg_price: p.avg_price,
                    price: p.price,
                    value: p.value,
                    redeemable: p.redeemable,
                })
                .collect(),
            truncated,
        },
    }
}

/// **The reply to one `Request::VenueRead`**: the venue's answer for `venue`'s `account`, or the operator's
/// sentence for why there is none. `source` is the publisher's lease registry (`None`: this node has none,
/// which is a node not running behind a gatewayhub).
///
/// Blocking (a venue round trip, bounded by the lease registry): called on the connection's own thread, never on
/// the core's.
pub(super) fn venue_read_reply(
    source: Option<&dyn SymbolFacts>,
    venue: &str,
    account: &str,
    kind: WireVenueReadKind,
) -> Response {
    if !VENUES.contains(&venue) {
        return Response::Error(format!(
            "`{}` is not a venue of this node's roster ({}); nothing was asked",
            echoed(venue),
            VENUES.join(", ")
        ));
    }
    let label = match parse_wire_account(account) {
        Ok(label) => label,
        Err(e) => {
            return Response::Error(format!(
                "`{}` is not an account name: {e}; nothing was asked",
                echoed(account)
            ));
        }
    };
    let route_key = route_key_of(venue, &label);
    tracing::info!(
        %route_key, kind = kind.word(),
        "vike-tradehub: venue read asked by a peer (nothing is placed, cancelled or folded)"
    );
    let Some(source) = source else {
        return Response::Error(format!(
            "{route_key}: this node holds no trade lease registry (it is not running behind a \
             gatewayhub), so there is no path to the venue to read; nothing was asked"
        ));
    };
    match source.venue_read(&route_key, model_kind(kind)) {
        Ok(read) if !read.is_finite() => Response::Error(format!(
            "{route_key}: the venue's {} answer carried a figure that is not a number, so it is not \
             passed on",
            kind.word()
        )),
        Ok(read) => Response::VenueRead(Box::new(wire_of(read))),
        Err(why) => Response::Error(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use vike_model::{VenueAllowance, VenueBalance, VenueOpenOrder};

    /// A registry that answers `answer` for every route key and remembers the one it was asked.
    struct Answers {
        answer: Result<VenueRead, String>,
        asked: std::sync::Mutex<Vec<(String, VenueReadKind)>>,
    }

    impl Answers {
        fn new(answer: Result<VenueRead, String>) -> Self {
            Answers { answer, asked: std::sync::Mutex::new(Vec::new()) }
        }
    }

    impl SymbolFacts for Answers {
        fn admission(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<vike_exec::SymbolAdmission, String> {
            Err("not asked".into())
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Err("not asked".into())
        }
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn venue_read(&self, route_key: &str, kind: VenueReadKind) -> Result<VenueRead, String> {
            self.asked.lock().unwrap().push((route_key.to_string(), kind));
            self.answer.clone()
        }
    }

    fn balance() -> VenueRead {
        VenueRead::Balance(VenueBalance {
            asset: "pUSD".into(),
            balance: 20.0,
            allowances: vec![VenueAllowance {
                spender: "0xE111".into(),
                name: "CTF exchange".into(),
                amount: 0.0,
                unlimited: true,
            }],
        })
    }

    fn error_text(response: Response) -> String {
        match response {
            Response::Error(text) => text,
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
                venue_read_reply(Some(&source), "polymarket", account, WireVenueReadKind::Balance);
            assert!(matches!(reply, Response::VenueRead(_)), "{reply:?}");
            let asked = source.asked.lock().unwrap().pop().expect("asked");
            assert_eq!(asked, (want.to_string(), VenueReadKind::Balance));
        }
        let _ =
            venue_read_reply(Some(&source), "polymarket", "DEFAULT", WireVenueReadKind::OpenOrders);
        assert_eq!(source.asked.lock().unwrap()[0].1, VenueReadKind::OpenOrders);
    }

    /// Both answers cross the wire field for field.
    #[test]
    fn the_answer_is_mirrored_field_for_field() {
        let orders = VenueRead::OpenOrders {
            orders: vec![VenueOpenOrder {
                venue_order_id: "0xabc".into(),
                client_order_id: Some("pm1".into()),
                symbol: "6710".into(),
                side: -1,
                price: None,
                qty: 5.0,
                filled_qty: 1.5,
                status: "PARTIALLY_FILLED".into(),
                ts_ms: 1_700_000_000_000,
            }],
            truncated: true,
        };
        let WireVenueRead::OpenOrders { orders, truncated } = wire_of(orders) else {
            panic!("an open-orders answer stays one")
        };
        assert!(truncated);
        let o = &orders[0];
        assert_eq!(
            (o.venue_order_id.as_str(), o.client_order_id.as_deref()),
            ("0xabc", Some("pm1"))
        );
        assert_eq!((o.symbol.as_str(), o.side, o.price), ("6710", -1, None));
        assert_eq!(
            (o.qty, o.filled_qty, o.status.as_str(), o.ts_ms),
            (5.0, 1.5, "PARTIALLY_FILLED", 1_700_000_000_000)
        );

        let WireVenueRead::Balance(b) = wire_of(balance()) else { panic!("a balance stays one") };
        assert_eq!((b.asset.as_str(), b.balance), ("pUSD", 20.0));
        assert_eq!(
            (b.allowances[0].spender.as_str(), b.allowances[0].name.as_str()),
            ("0xE111", "CTF exchange")
        );
        assert_eq!((b.allowances[0].amount, b.allowances[0].unlimited), (0.0, true));
    }

    /// The positions kind reaches the registry as itself and its rows cross the wire field for field, the
    /// redeemable flag and the venue's price and value included; an empty wallet stays an empty ANSWER.
    #[test]
    fn a_positions_answer_is_mirrored_field_for_field() {
        let held = VenueRead::Positions {
            positions: vec![vike_model::VenuePosition {
                symbol: "1484".into(),
                title: "Solana Up or Down".into(),
                outcome: "Down".into(),
                qty: 11.11,
                avg_price: Some(0.2099),
                price: None,
                value: Some(0.0),
                redeemable: true,
            }],
            truncated: true,
        };
        let source = Answers::new(Ok(held.clone()));
        let reply =
            venue_read_reply(Some(&source), "polymarket", "DEFAULT", WireVenueReadKind::Positions);
        assert_eq!(source.asked.lock().unwrap()[0].1, VenueReadKind::Positions);
        let Response::VenueRead(read) = reply else { panic!("expected a read, got {reply:?}") };
        let WireVenueRead::Positions { positions, truncated } = *read else {
            panic!("a positions answer stays one")
        };
        assert!(truncated);
        let p = &positions[0];
        assert_eq!(
            (p.symbol.as_str(), p.title.as_str(), p.outcome.as_str()),
            ("1484", "Solana Up or Down", "Down")
        );
        assert_eq!((p.qty, p.avg_price, p.price, p.value), (11.11, Some(0.2099), None, Some(0.0)));
        assert!(p.redeemable);

        let empty = VenueRead::Positions { positions: Vec::new(), truncated: false };
        let source = Answers::new(Ok(empty));
        let reply =
            venue_read_reply(Some(&source), "polymarket", "DEFAULT", WireVenueReadKind::Positions);
        let Response::VenueRead(read) = reply else { panic!("an empty wallet is an answer") };
        assert_eq!(*read, WireVenueRead::Positions { positions: Vec::new(), truncated: false });
    }

    /// A positions figure that is not a number is turned into an error in words too (a `NaN` writes as
    /// `null` and ends the connection that decodes it).
    #[test]
    fn a_positions_figure_that_is_not_a_number_is_an_error_in_words() {
        let nan = VenueRead::Positions {
            positions: vec![vike_model::VenuePosition {
                symbol: "1".into(),
                title: String::new(),
                outcome: String::new(),
                qty: f64::NAN,
                avg_price: None,
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
            WireVenueReadKind::Positions,
        ));
        assert!(text.contains("positions answer carried a figure that is not a number"), "{text}");
    }

    /// A refusal is the operator's sentence, verbatim, as an error and never as an empty answer.
    #[test]
    fn a_refusal_is_the_lease_and_the_daemons_words() {
        let sentence = "not offered by binance: its bridge has no account-wide open-orders read";
        let source = Answers::new(Err(sentence.to_string()));
        let reply =
            venue_read_reply(Some(&source), "binance", "DEFAULT", WireVenueReadKind::OpenOrders);
        assert_eq!(error_text(reply), sentence);
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
            WireVenueReadKind::Balance,
        ));
        assert!(text.contains("not a number"), "{text}");
    }

    /// A peer's strings are judged before any lease is asked: an unknown venue and a malformed account are
    /// refused by name, bounded in what they echo, and the registry is asked nothing.
    #[test]
    fn a_bad_venue_or_account_is_refused_before_anything_is_asked() {
        let source = Answers::new(Ok(balance()));
        let unknown =
            venue_read_reply(Some(&source), "nosuchvenue", "DEFAULT", WireVenueReadKind::Balance);
        let text = error_text(unknown);
        assert!(text.contains("not a venue of this node's roster") && text.contains("polymarket"));
        let bad = error_text(venue_read_reply(
            Some(&source),
            "polymarket",
            "not a label!",
            WireVenueReadKind::Balance,
        ));
        assert!(bad.contains("not an account name"), "{bad}");
        let long = "x".repeat(10_000);
        let echoed_long = error_text(venue_read_reply(
            Some(&source),
            &long,
            "DEFAULT",
            WireVenueReadKind::Balance,
        ));
        assert!(echoed_long.len() < 400, "the echo is bounded: {} bytes", echoed_long.len());
        assert!(source.asked.lock().unwrap().is_empty(), "nothing was asked of the lease registry");
    }

    /// A node with no lease registry says so, by account, and asks nothing.
    #[test]
    fn a_node_with_no_lease_registry_says_so() {
        let text =
            error_text(venue_read_reply(None, "polymarket", "DEFAULT", WireVenueReadKind::Balance));
        assert!(text.contains("polymarket:") && text.contains("no trade lease registry"), "{text}");
    }
}
