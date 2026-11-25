//! `venue_read` — an OPERATOR's read of an IBKR account (socket backend), asked of the venue now: its open orders
//! (`vike_model::VenueReadKind::OpenOrders`; the gatewayhub's `TradeUp::VenueRead` asks it, and
//! `trade venue open-orders ibkr` prints it). Nothing is folded and nothing changes at the venue.
//!
//! `reqAllOpenOrders` answers for every API client, TWS and account of the login; the rows are kept to the account
//! (`config.account`) and parsed by the reconcile client's own parser (`super`'s `order_reports_for`, once per contract
//! the answer names), so the two reads cannot disagree about a row: `lmtPrice` is a limit's PRICE, `auxPrice` a
//! stop's TRIGGER, `orderRef` OUR ID. Each row is named by the contract's `localSymbol` (the venue's own name), else
//! its `symbol`.
//!
//! ⚠ **It rides the account's RUNNING reconcile connection** (lent by [`lend`], weakly) and opens no connection of
//! its own: one more `Client` would need one more API client id on the gateway, and every id is the operator's to
//! write. An account whose reconcile client is not running in this daemon (reconciliation off, the cpapi backend, a
//! failed connect) is answered in words. The read waits for the pass's own `all_open_orders` to end first
//! (`orders_turn`), holds no `Client` while it drains (a client dropped meanwhile ends the subscription, and so
//! does the read), and believes the end only while that connection is still the client's. The balance and the
//! positions are not offered.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use ibapi::orders::Orders;
use vike_model::{VenueRead, VenueReadOpenOrder};

use crate::config::IbkrConfig;
use crate::reconcile::socket::{
    EndsBy, READ_DEADLINE, ReconcileClientIbkrSocket, ReconcileConnection, Target, drain,
    order_reports_for,
};
use crate::refusals::IbkrVenueReadRefusal;

/// What a running reconcile client lends: its connection (weakly) and its open-orders turn.
struct Lent {
    conn: Weak<Mutex<ReconcileConnection>>,
    turn: Arc<Mutex<()>>,
}

/// The lent connections, by gateway and reconcile client id (unique on a gateway: IB refuses a second one).
static LENT: LazyLock<Mutex<HashMap<String, Lent>>> = LazyLock::new(Default::default);

fn key(config: &IbkrConfig) -> String {
    format!("{}:{}/{}", config.host, config.port, config.reconcile_client_id)
}

/// Lend `client`'s connection to an operator's read of its account; the next client on the same id replaces it.
pub(crate) fn lend(client: &ReconcileClientIbkrSocket) {
    let lent = Lent { conn: Arc::downgrade(&client.conn), turn: Arc::clone(&client.orders_turn) };
    LENT.lock().unwrap_or_else(PoisonError::into_inner).insert(key(&client.config), lent);
}

/// The account's open orders over the reconcile connection `config` names. `Err` is the operator's refusal.
pub(crate) fn open_orders(config: &IbkrConfig) -> Result<VenueRead, IbkrVenueReadRefusal> {
    let (conn, turn) = {
        let lent = LENT.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = lent.get(&key(config)).ok_or(IbkrVenueReadRefusal::NoReconcileConnection)?;
        (entry.conn.clone(), Arc::clone(&entry.turn))
    };
    let _turn = turn.lock().unwrap_or_else(PoisonError::into_inner);
    let current = || {
        let conn = conn.upgrade()?;
        let held = conn.lock().unwrap_or_else(PoisonError::into_inner);
        held.link.as_ref().map(|link| Arc::clone(&link.client))
    };
    let client = current().ok_or(IbkrVenueReadRefusal::NoReconcileConnection)?;
    let sub = client
        .all_open_orders()
        .map_err(|e| IbkrVenueReadRefusal::AskFailed { why: e.to_string() })?;
    let asked = Arc::downgrade(&client);
    drop(client);
    let rows =
        drain(|t| sub.next_timeout(t), READ_DEADLINE, "open orders", EndsBy::Close, |_| false)
            .map_err(|why| IbkrVenueReadRefusal::Read { why })?;
    // A torn-down subscription reads like one that ended: the rows count only if the connection outlived the read.
    if !current().zip(asked.upgrade()).is_some_and(|(now, then)| Arc::ptr_eq(&now, &then)) {
        return Err(IbkrVenueReadRefusal::ConnectionClosed);
    }
    let (orders, truncated) = open_orders_from(&rows, &config.account);
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// `all_open_orders` rows → the account's rows, every contract's, cut at [`vike_model::MAX_VENUE_OPEN_ORDERS`].
fn open_orders_from(rows: &[Orders], account: &str) -> (Vec<VenueReadOpenOrder>, bool) {
    let mut contracts: Vec<Target> = Vec::new();
    for row in rows {
        if let Orders::OrderData(od) = row
            && !contracts.iter().any(|(conid, _)| *conid == od.contract.contract_id)
        {
            let name = if od.contract.local_symbol.is_empty() {
                od.contract.symbol.to_string()
            } else {
                od.contract.local_symbol.clone()
            };
            contracts.push((od.contract.contract_id, name));
        }
    }
    vike_model::open_orders_read(order_reports_for(rows, account, &contracts))
}

#[cfg(test)]
mod tests {
    use ibapi::contracts::{Contract, Currency};
    use ibapi::orders::{Action, Order, OrderData, OrderState, OrderStatus, OrderStatusKind};
    use vike_model::AccountTier;
    use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
    use vike_secrets::venue_setting::VenueSettings;

    use super::*;
    use crate::config::load_ibkr_config_from;

    const ACCOUNT: &str = "DU1234567";

    fn contract(conid: i32, local: &str) -> Contract {
        Contract {
            contract_id: conid,
            local_symbol: local.to_string(),
            currency: Currency::from("USD"),
            ..Contract::default()
        }
    }

    fn open_order(id: i32, perm: i64, contract: Contract, order: Order) -> Orders {
        Orders::OrderData(OrderData {
            order_id: id,
            contract,
            order: Order { order_id: id, perm_id: perm, ..order },
            order_state: OrderState { status: OrderStatusKind::Submitted, ..OrderState::default() },
        })
    }

    /// An `all_open_orders` answer (constructed ibapi rows, the shape `tests/ibkr_socket_reconcile_parse.rs` pins):
    /// our BUY LMT on AAPL (its `orderRef` is our order client id), a SELL STP on TSLA typed in TWS (no `orderRef`), its status
    /// joined by permId, and another account's order on the same login.
    fn rows() -> Vec<Orders> {
        let limit = Order {
            action: Action::Buy,
            total_quantity: 100.0,
            order_type: "LMT".into(),
            limit_price: Some(150.0),
            order_ref: "S00A307k3x9q2m7d1p4ENLM".into(),
            account: ACCOUNT.into(),
            ..Order::default()
        };
        let stop = Order {
            action: Action::Sell,
            total_quantity: 10.0,
            order_type: "STP".into(),
            aux_price: Some(240.5),
            account: ACCOUNT.into(),
            ..Order::default()
        };
        let elsewhere = Order { account: "DU7654321".into(), ..limit.clone() };
        vec![
            open_order(11, 9001, contract(265598, "AAPL"), limit),
            open_order(0, 9002, contract(76792991, "TSLA"), stop),
            Orders::OrderStatus(OrderStatus {
                order_id: 0,
                perm_id: 9002,
                status: OrderStatusKind::PreSubmitted,
                remaining: 10.0,
                ..OrderStatus::default()
            }),
            open_order(12, 9003, contract(265598, "AAPL"), elsewhere),
        ]
    }

    #[test]
    fn every_contracts_open_order_of_the_account_is_listed_with_its_kind_trigger_and_our_id() {
        let (orders, truncated) = open_orders_from(&rows(), ACCOUNT);
        assert!(!truncated);
        assert_eq!(orders.len(), 2, "another account's order is not this account's: {orders:?}");
        let (limit, stop) = (&orders[0], &orders[1]);
        assert_eq!((limit.symbol.as_str(), limit.venue_order_id.as_str()), ("AAPL", "11"));
        assert_eq!((limit.side, limit.qty), (1, 100.0));
        assert_eq!(
            (limit.price, limit.trigger_price, limit.order_type.as_str()),
            (Some(150.0), None, "limit")
        );
        assert_eq!(limit.order_client_id.as_deref(), Some("S00A307k3x9q2m7d1p4ENLM"));
        assert_eq!((stop.symbol.as_str(), stop.side, stop.qty), ("TSLA", -1, 10.0));
        assert_eq!(
            (stop.price, stop.trigger_price, stop.order_type.as_str()),
            (None, Some(240.5), "stop"),
            "a STP's auxPrice is its TRIGGER"
        );
        assert_eq!(stop.order_client_id, None, "an order typed in TWS carries no id of ours");
        assert_eq!(stop.status, "ACCEPTED", "its status is the one joined by permId");
        assert_eq!(
            open_orders_from(&[], ACCOUNT),
            (Vec::new(), false),
            "nothing open is an answer"
        );
    }

    /// A kind this bridge does not read, and an account that does not arm, are refused before anything is looked up.
    #[test]
    fn a_read_that_cannot_be_made_is_refused_before_any_lookup() {
        use vike_bridge_core::venue_adapter::VenueAdapter;
        use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
        use vike_model::{VenueReadKind, not_offered_words};

        let fx = AdapterFixture::new(&[]);
        let read =
            |kind| crate::adapter::VenueAdapterIbkr.venue_read(&fx.inputs(AccountTier::Demo), kind);
        for kind in [VenueReadKind::Balance, VenueReadKind::Positions] {
            assert_eq!(read(kind).unwrap_err(), not_offered_words("ibkr", kind).into());
        }
        // The trait's answer is the refusal, its id kept across `venue_read`'s seam.
        assert_eq!(
            read(VenueReadKind::OpenOrders).unwrap_err(),
            IbkrVenueReadRefusal::AccountDoesNotArm.carried()
        );
    }

    /// With no reconcile client of the account running here, nothing is dialled and the operator is told why.
    #[test]
    fn an_account_with_no_running_reconcile_connection_is_told_so() {
        let fields = CredentialFields::from_pairs([("ACCOUNT", ACCOUNT)]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        let seeded =
            VenueSettings::default().with_urls(vike_venues::venue_urls::VenueUrls::seed("ibkr"));
        let mut config =
            load_ibkr_config_from(AccountTier::Demo, &creds, &seeded).expect("a config");
        config.port = 1;
        assert_eq!(open_orders(&config).unwrap_err(), IbkrVenueReadRefusal::NoReconcileConnection);
    }
}
