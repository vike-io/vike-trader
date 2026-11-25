//! The per-mount fill ATTRIBUTION ledger, [`MountAttribution`], and the pure law it is folded and valued
//! by ([`MountAttribution::fold_fill`], [`MountAttribution::valued_at`]).
//!
//! It lives here, below `vike-core` and `vike-simulator`, so the live core and the backtest attribute a
//! fill to a mount by ONE law, as both judge a mount's budget by [`crate::MountBudget`]. Plain numbers:
//! no serde, no clock, no venue. It is not part of `Account`, `EngineSnapshot` or `state_hash`. Its
//! durable shapes (the journal `Snapshot` row, the ownership file's `Ledger` line) and every choice of
//! WHICH row a fill folds into, at which contract value and fee, are vike-core's
//! (`vike_core`'s `dispatch_applied_fills`).

/// Per-mount fill ATTRIBUTION ledger (steal/core-per-mount-budget) — one per mount slot, folded
/// from that mount's OWN attributed fills (order client id -> mount via `vike_core`'s
/// `CoreThread::order_client_id_mount`) at fill cadence in `dispatch_applied_fills`, NEVER the
/// per-message hot fold. Pure numbers so the per-closed-bar budget sweep and the publish-time view
/// can snapshot it cheaply; the (venue, symbol) it prices against comes from the parallel
/// `CoreThread::mount_vs`, not from here. The position/avg-px fold ([`Self::fold_fill`]) mirrors
/// `Account::fold` exactly (both call `vike_model::compute_fill`), so a mount tracks the same
/// weighted-average-cost position per instrument that the account does.
///
/// **ONE ROW PER `(venue, account, symbol)`, as the accounts fold** (the account only on a row of another
/// account of its venue than the mount's route resolves, decision 0293). The four numbers are the row of the
/// mount's OWN `(venue, symbol)`; [`Self::legs`] holds a row per OTHER instrument the mount traded.
/// A position is a fact about one instrument, so one `(size, average_price)` across two read a sell of ETH
/// as a close of a long of BTC (a loss nobody took) and a hedge on another venue as flat (a loss
/// nobody saw). Every figure that is money adds across the rows ([`Self::realized_net`],
/// `CoreThread::mount_valuation`); sizes of two instruments do not add, so the mount's published
/// position stays its own symbol's.
///
/// ⚠ `legs` stays EMPTY on a mount that declares no `vike_core::Mount::symbols`, whatever a fill says
/// (`CoreThread::fill_row_is_own`), so such a mount folds, publishes and persists exactly as it did
/// before the rows existed: no allocation, and one `is_empty` per valuation on the publish path.
///
/// ⚠ A new field breaks the struct literals in `vike_core`'s `assemble_core` and `Folded::apply` on
/// purpose, but NOT its two writers (`publish.rs`'s `Snapshot` rows, `OrderOwnerRecord::ledger_row`):
/// add it to both by hand or a restart loses it.
///
/// shortcut: a leg on an engine in ANOTHER CURRENCY adds that currency's money to the mount's figures
/// (as the one fold did), and a tier refusal of a leg's engine alone drops no row
/// (`forget_refused_mount_ledgers` judges the mount's own engine). No such mount can run: decide both
/// when one is built.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountAttribution {
    /// signed net position folded from this mount's attributed fills
    pub size: f64,
    /// weighted-average entry price of the folded position (0.0 while flat)
    pub average_price: f64,
    /// cumulative GROSS realized price PnL on this mount's closed portions
    pub realized_pnl: f64,
    /// cumulative commission/fees on this mount's attributed fills (signed; a maker rebate is < 0)
    pub fees_paid: f64,
    /// The rows of every instrument other than the mount's own, first fill first.
    pub legs: Vec<MountAttributionRow>,
}

/// One row of a [`MountAttribution`] for an instrument other than the mount's own.
#[derive(Debug, Clone, PartialEq)]
pub struct MountAttributionRow {
    pub venue: String,
    /// The ACCOUNT of [`Self::venue`] the row's position is held on when it is not the book the mount's route
    /// resolves for that venue (a leg named by another account, decision 0293): two accounts of one venue on
    /// one symbol are two rows. `None` for every other row, which keeps them as they always were.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    pub symbol: String,
    /// The row's four numbers; its own `legs` is always empty.
    pub attr: MountAttribution,
}

impl MountAttribution {
    /// Fold one fill of THIS row's instrument (`side` +1 buys, -1 sells, `qty` at `price`, contract
    /// `value`): the weighted-average-cost position and the realized PnL of its closing part through
    /// `vike_model::compute_fill_value`, then `fee`, the commission as the account booked it. A fee
    /// correction (decision 0238) moves no position: its caller adds the fee alone.
    #[inline] // keeps vike-core's `dispatch_applied_fills` what it was while these lines sat in it
    pub fn fold_fill(
        &mut self,
        side: i32,
        qty: f64,
        price: f64,
        value: vike_model::ContractValue,
        fee: f64,
    ) {
        let out =
            vike_model::compute_fill_value(self.size, self.average_price, side, qty, price, value);
        self.size = out.new_size;
        self.average_price = out.new_average_price;
        if out.closing_qty > 0.0 {
            self.realized_pnl += out.realized_pnl;
        }
        self.fees_paid += fee;
    }

    /// `(realized net of fees, unrealized, gross notional)` of THIS row at `price` and contract `value`
    /// (its `legs` are not read). A flat row is its realized net alone and reads neither, so a flat
    /// inverse row's zero average is never divided by. The pure half of `vike_core`'s
    /// `ledger_row_valuation`, which finds the price and values an unpriced row at nothing.
    #[inline]
    pub fn valued_at(&self, price: f64, value: vike_model::ContractValue) -> (f64, f64, f64) {
        let realized_net = self.realized_pnl - self.fees_paid;
        if self.size == 0.0 {
            return (realized_net, 0.0, 0.0);
        }
        let unrealized = value.unrealized_pnl(self.average_price, price, self.size);
        (realized_net, unrealized, value.gross_notional(self.size, price))
    }

    /// Realized PnL less fees over EVERY row. With no leg it is the one expression it always was.
    pub fn realized_net(&self) -> f64 {
        let mut net = self.realized_pnl - self.fees_paid;
        for leg in &self.legs {
            net += leg.attr.realized_pnl - leg.attr.fees_paid;
        }
        net
    }

    /// The row of `(venue, account, symbol)`, made zero on its first fill.
    pub fn leg_mut(
        &mut self,
        venue: &str,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
        symbol: &str,
    ) -> &mut MountAttribution {
        let at = self
            .legs
            .iter()
            .position(|l| l.venue == venue && l.account.as_ref() == account && l.symbol == symbol);
        let at = at.unwrap_or_else(|| {
            self.legs.push(MountAttributionRow {
                venue: venue.to_string(),
                account: account.cloned(),
                symbol: symbol.to_string(),
                attr: MountAttribution::default(),
            });
            self.legs.len() - 1
        });
        &mut self.legs[at].attr
    }

    /// Put one persisted row back: under its key, or as the own row when it has none, the only row
    /// that existed before the key did. The other rows stay, so the order rows are read in is free.
    /// `account` is the row's persisted account (its wire spelling, `DEFAULT` for the unlabelled one).
    pub fn put_row(
        &mut self,
        venue: Option<String>,
        account: Option<String>,
        symbol: Option<String>,
        mut row: MountAttribution,
    ) {
        let account =
            account.and_then(|a| vike_model::accounts::account_keys::parse_wire_account(&a).ok());
        match venue.zip(symbol) {
            Some((venue, symbol)) => *self.leg_mut(&venue, account.as_ref(), &symbol) = row,
            None => {
                row.legs = std::mem::take(&mut self.legs);
                *self = row;
            }
        }
    }
}

#[path = "mount_attribution_tests.rs"]
#[cfg(test)]
mod mount_attribution_tests;
