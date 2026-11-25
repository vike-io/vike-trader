//! **The tradehub's refusals a PERSON or an AGENT reads**, declared once (`vike_utility::refusal_enum!`, decision
//! 0301): the node's answers to the control, mount, account, settings and venue-read verbs, the strategy mount
//! refusals and the sentences a NOT RUNNING row carries. Each variant has ONE text and ONE id,
//! `tradehub.<what>[.<why>]`; the id is published in the workspace's `refusals.json`
//! (`crates/vike-backend/tests/refusal_catalog.rs`) and never renamed, reused or dropped.
//!
//! **On the wire a refusal is the refusal FRAME** (`vike_hub_protocol::refusal::HubRefusal`, `{id, text}`, as
//! `vike_tradehub_client::proto::Response::Refused`): every enum here converts into one under its id (`framed!`), and
//! the session's `Response::older_form` turns it back into the `Response::Error` of the same text for a client that
//! reads no frame. Its texts are byte for byte what that `Error` always carried.
//!
//! The ids a CLIENT decides on (`tradehub.starting`, `tradehub.busy`) are NOT here: they are
//! `vike_tradehub_client::refusals::TradehubRefusal`'s, the crate the node and its readers both link. Code decides on a
//! refusal's id or its variant, never on its text.

use vike_hub_protocol::refusal::HubRefusal;
use vike_utility::errors::CarriedRefusal;

/// Each named refusal enum answers the wire as the refusal FRAME under the one id it shows (a `carries` variant's
/// carried id when that has one: `vike_utility::errors`'s "A carrier shows ONE id").
macro_rules! framed {
    ($($refusal:ident),+ $(,)?) => {
        $(impl From<$refusal> for HubRefusal {
            fn from(refusal: $refusal) -> Self {
                HubRefusal::new(refusal.shown_id(), &refusal)
            }
        })+
    };
}

// ===== section 1: the control door's gates (`crate::server::refusal`, `order_list`, `post_only_door`, `unadmit`,
// `unlatch`) =====

vike_utility::refusal_enum! {
    /// The control door's refusal of a command before its `Ack` (`crate::server::refusal`'s routing, account, bracket
    /// and combo gates, `crate::server::order_list`, `crate::server::post_only_door`, `crate::server::unadmit`,
    /// `crate::server::unlatch`), and the profile's primary-mount twin of the venue gate
    /// (`crate::config::no_engine_refusal`). `subject` is the verb as the sentence names it (`The bracket`).
    #[derive(Debug, Clone, PartialEq)]
    pub enum DoorRefusal with ids DoorRefusalId {
        /// The command names a venue this node runs no engine for; `known` is the roster, sorted, deduplicated.
        NoEngine { venue: String, known: String, subject: String } => "tradehub.door.no_engine_for_venue":
            "this node runs no engine for venue `{venue}` — it runs: {known}. {subject} was REFUSED rather \
             than applied to this node's primary engine: an order names the book it is for, and a node \
             that cannot honour the name must not choose one",
        /// The venue runs, the named account does not; `held` is the venue's accounts this node runs.
        NoAccount { account: String, venue: String, held: String, subject: &'static str }
            => "tradehub.door.no_such_account":
            "this node runs no account `{account}` of venue `{venue}` — it runs: {held}. {subject} was \
             REFUSED rather than applied to another book of that venue: an order names the book it is \
             for, and a node that cannot honour the name must not choose one",
        /// A reducing verb names an account and no venue.
        AccountWithoutVenue { account: String, subject: &'static str } => "tradehub.door.account_without_venue":
            "account `{account}` was named with no venue — an account label names one book OF a venue, \
             so on its own it names nothing this node can resolve. {subject} was REFUSED rather than \
             widened to every engine: name the venue the account belongs to, or name neither for the \
             whole-node verb",
        /// A command naming no account, on a node that cannot read its account rows.
        AccountRowsUnreadable { why: String, venue: String, subject: &'static str }
            => "tradehub.door.account_rows_unreadable":
            "this node cannot read its account rows ({why}), so it cannot tell how many accounts \
             venue `{venue}` has, and {subject} was REFUSED with nothing sent: name the book it is for",
        /// A reducing command naming no account, before the node published its accounts.
        AccountsNotPublished { venue: String, subject: &'static str } => "tradehub.door.accounts_not_published":
            "this node has not published its accounts yet, so it cannot tell how many venue `{venue}` \
             runs, and {subject} was REFUSED with nothing sent: name the book it is for, or retry once \
             the node has published its first snapshot. A command naming no venue still reaches the \
             whole node",
        /// A command naming no account on a venue of several: `why` is
        /// `vike_tradehub_client::wire::names_no_account_refusal`'s sentence, the one every client door says too.
        AccountNotNamed { why: String } => "tradehub.door.account_not_named": "{why}",
        /// A submit's order client id an order that ENDED carries.
        OrderClientIdUsedByEndedOrder { order_client_id: String, status: String }
            => "tradehub.door.order_client_id.used_by_ended_order":
            "order client id {order_client_id} is already used by an order that is {status}: pick a new order \
             client id",
        /// A submit's order client id an order that is still LIVE carries.
        OrderClientIdUsedByLiveOrder { order_client_id: String, status: String }
            => "tradehub.door.order_client_id.used_by_live_order":
            "order client id {order_client_id} is already used by an order that is {status} (still live): pick a \
             new order client id, or change that order by this id with modify or cancel",
        /// A submit's order client id an order used before this node's last restart (decision 0270, D7).
        OrderClientIdUsedBeforeRestart { order_client_id: String }
            => "tradehub.door.order_client_id.used_before_restart":
            "order client id {order_client_id} was used by an order before this node's last restart (within 24 h): \
             pick a new order client id",
        /// An order list whose leg's id is not new (`said` is the leg's own `order_client_id` refusal): refused whole.
        OrderListOrderClientIdUsed { said: String } => "tradehub.order_list.order_client_id_used":
            "order list: {said}. The order list was REFUSED whole and no leg of it was sent",
        /// A combo whose id is not new: refused whole.
        ComboOrderClientIdUsed { said: String } => "tradehub.combo.order_client_id_used":
            "combo: {said}. The combo was REFUSED whole and nothing of it was sent",
        /// A bracket whose stem's leg ids are not new: refused whole.
        BracketOrderClientIdUsed { said: String } => "tradehub.bracket.order_client_id_used":
            "bracket: {said}. The bracket was REFUSED whole and nothing of it was sent",
        /// A release's price condition on the order's venue, with no engine that says what it trades.
        ReleaseEngineUnknown { key: String, named: String } => "tradehub.door.release.engine_unknown":
            "submit: `release` — this node publishes no engine for `{key}` that says what it \
             trades, so it cannot tell whether it can read the price of `{named}` the condition \
             names. The order was REFUSED rather than held on a price nobody checked",
        /// A release's price condition on a symbol the order's engine does not trade.
        ReleaseSymbolNotTraded { named: String, key: String, trades: String }
            => "tradehub.door.release.symbol_not_traded":
            "submit: `release` — the price condition reads `{named}`, which this node's `{key}` engine \
             does not trade (it trades {trades}), so it could never read that price and the order would \
             never be sent. Name a symbol it trades, or none for the order's own",
        /// A release's price condition on another venue that only a `paper` account trades.
        ReleaseVenuePaperOnly { symbol: String, venue: String } => "tradehub.door.release.venue_paper_only":
            "submit: `release` — the price condition reads {symbol} on {venue}, and no demo or live account of \
             {venue} feeds a price here (only a paper one trades it), so its price is never read here: REFUSED \
             rather than held on a price nobody reads",
        /// A release's price condition on another venue that no account of this node trades.
        ReleaseVenueNotTraded { symbol: String, venue: String } => "tradehub.door.release.venue_not_traded":
            "submit: `release` — the price condition reads {symbol} on {venue}, and no {venue} account on \
             this node trades it, so its price is never read here: REFUSED rather than held on a price \
             nobody reads",
        /// A modify of a trailing order's price or activation (`named` is the field).
        ModifyTrailingPrice { named: &'static str, order_client_id: String, order_type: String }
            => "tradehub.door.modify.trailing_price":
            "modify: `{named}` — order {order_client_id} is a {order_type} order, which a modify changes in its \
             size only (its trail, offset and activation stay as placed): cancel it and place a new one",
        /// A modify's trigger on an order that has none.
        ModifyNoTrigger { order_client_id: String, order_type: String } => "tradehub.door.modify.no_trigger":
            "modify: `new_trigger_price` — order {order_client_id} is a {order_type} order, which has no trigger \
             price to move: change its price instead",
        /// A command to an account this node lists NOT RUNNING, with the row's reason (decision 0156).
        AccountNotRunning { label: String, venue: String, tier: String, reason: String, subject: &'static str }
            => "tradehub.door.account_not_running":
            "account `{label}` of venue `{venue}` ({tier}) does NOT RUN on this node: {reason}. \
             {subject} was REFUSED: an account that does not run is not routed to another book of that \
             venue, and no paper simulator stands in for it",
        /// An opening command for an account still waiting for its venue balance (decisions 0169, 0246).
        AwaitingBalance { label: String, venue: String, since_secs: u64, why: String, subject: &'static str }
            => "tradehub.door.awaiting_balance":
            "account `{label}` of venue `{venue}` is awaiting the venue balance after the account start \
             ({since_secs} s ago); retry in a few seconds. Why it has not landed: {why}. {subject} was REFUSED: until the \
             first reconcile pass lands the venue's balance, the engine judges an opening order against its \
             seed cash and not against what the venue holds. Reducing orders, cancels, a flatten and a \
             market exit are not held. The reconcile pass asks again on its own; if it stays refused, see \
             this node's `reconcile` log lines",
        /// An opening command for a lane twin kept up only to be closed; `book` is its `venue/LABEL`.
        LaneTwinClosingOnly { key: String, subject: &'static str, book: String }
            => "tradehub.door.lane_twin_closing_only":
            "the perp lane `{key}` is kept up only to be closed: it still holds a position or an order, and the \
             row that started it no longer allows it to trade (its margin leverage is not stated, or its tier \
             is changing). {subject} was REFUSED: it takes no new position and no larger or more aggressive \
             opening order. Reducing orders, cancels, a flatten and a market exit are taken; state the leverage \
             again (or the old tier) to trade the lane. The engine is held reduce-only too, a hold the \
             node-wide `trade resume` and `trade account resume {book}` do not lift",
        /// A submit whose side is neither 1 nor -1.
        SubmitSide { side: i32 } => "tradehub.door.submit.side":
            "submit: `side` must be 1 (buy) or -1 (sell), got {side}. The order was REFUSED and nothing was sent",
        /// An account-less bracket on a venue whose one account is not the default one.
        BracketAccountNotNamed { venue: String, held: String } => "tradehub.bracket.account_not_named":
            "venue `{venue}` runs these accounts on this node: {held}. A bracket that names no \
             account goes only to a venue whose one account is the default one; name the \
             account it is for. The bracket was REFUSED rather than applied to one of them",
        /// A bracket before the node published its accounts.
        BracketAccountsNotPublished { venue: String } => "tradehub.bracket.accounts_not_published":
            "this node has not published its accounts yet, so it cannot tell which accounts venue \
             `{venue}` runs. The bracket was REFUSED rather than sent: a bracket goes to the account it \
             names, or, naming none, only to a venue whose one account is the default one. Retry once \
             the node has published its first snapshot",
        /// A bracket whose side is neither 1 nor -1.
        BracketSide { side: i32 } => "tradehub.bracket.side":
            "bracket: `side` must be 1 (a long entry) or -1 (a short entry), got {side}",
        /// A bracket's `qty`, `stop_loss` or `take_profit` that is not a finite number above zero.
        BracketNotPositive { field: &'static str, value: f64 } => "tradehub.bracket.not_positive":
            "bracket: `{field}` must be a finite number above zero, got {value}",
        /// A bracket's priced entry that is not a finite number above zero.
        BracketEntryNotPositive { value: f64 } => "tradehub.bracket.entry_not_positive":
            "bracket: `entry_price` must be a finite number above zero, got {value} (null is a market entry)",
        /// A priced bracket whose exits are on the wrong side of its entry; `rule` is the order it needs.
        BracketInverted { dir: &'static str, rule: &'static str, stop_loss: f64, entry: f64, take_profit: f64 }
            => "tradehub.bracket.inverted":
            "bracket: an inverted {dir} bracket — it needs {rule}, got stop_loss {stop_loss}, \
             entry_price {entry}, take_profit {take_profit}. An exit on the wrong side of its \
             entry would fire the moment the entry fills",
        /// A market-entry bracket whose exits are in the wrong order against each other.
        BracketInvertedMarketEntry { dir: &'static str, rule: &'static str, stop_loss: f64, take_profit: f64 }
            => "tradehub.bracket.inverted_market_entry":
            "bracket: an inverted {dir} bracket — even with a market entry it needs \
             {rule}, got stop_loss {stop_loss}, take_profit {take_profit}. Whatever the entry fills at, \
             one of these exits is on the wrong side of it",
        /// A bracket's account label that does not parse (`why` is the parser's words).
        BracketAccount { why: String } => "tradehub.bracket.account": "bracket: `account` — {why}",
        /// A bracket whose engine is not published with what it trades.
        BracketEngineUnknown { key: String, venue: String, label: String } => "tradehub.bracket.engine_unknown":
            "bracket: this node publishes no engine for `{key}` (venue `{venue}`, account `{label}`) \
             that says what it trades, so it cannot tell whether that engine can hold this \
             bracket. The bracket was REFUSED rather than sent to an engine nobody checked",
        /// A bracket for an engine on a spot lane that cannot hold a stop-loss.
        BracketSpotLane { key: String, trades: String, venue: String, perp_suffix: &'static str }
            => "tradehub.bracket.spot_lane":
            "bracket: this node's `{key}` engine trades {trades}, on {venue}'s spot lane, and \
             that lane cannot hold a stop-loss yet (the venue would reject the stop only after the \
             entry filled, leaving the position unprotected). The engine places every order on the \
             symbol it was mounted on, whatever a bracket names, so no bracket goes to `{key}` on \
             this node until it runs a perp-lane engine (a `{perp_suffix}` symbol)",
        /// A bracket on a symbol its engine does not trade.
        BracketSymbolNotTraded { key: String, trades: String, symbol: String }
            => "tradehub.bracket.symbol_not_traded":
            "bracket: this node's `{key}` engine trades {trades}, not `{symbol}`. A bracket goes to that \
             one engine, and only a plain order adds a symbol to an engine (a bracket does not), so \
             this bracket would land on another instrument. Send it on a symbol that engine trades",
        /// A combo's account label that does not parse (`why` is the parser's words).
        ComboAccount { why: String } => "tradehub.combo.account": "combo: `account` — {why}",
        /// A combo's lifetime that is no word, or one its order cannot carry (`why`: `vike_model`'s words).
        ComboTimeInForce { why: String } => "tradehub.combo.time_in_force": "combo: `time_in_force` — {why}",
        /// A combo `vike_model::OrderCombo::validate` refuses: `why` is its `ComboError`, which opens with `combo`.
        ComboInvalid { why: String } => "tradehub.combo.invalid": "{why}",
        /// A post-only combo.
        ComboPostOnly => "tradehub.combo.post_only":
            "combo: `time_in_force` — a combo cannot be post-only: no book models a maker-only combo",
        /// A combo with no order client id of its own.
        ComboOrderClientIdMissing => "tradehub.combo.order_client_id_missing":
            "combo: a remote combo requires a pre-minted order_client_id",
        /// An order list with a blank symbol.
        OrderListEmptySymbol => "tradehub.order_list.empty_symbol": "order list: empty symbol",
        /// An order list's account label that does not parse (`why` is the parser's words).
        OrderListAccount { why: String } => "tradehub.order_list.account": "order list: `account` — {why}",
        /// An order list's leg the door refuses, by its index (`why` names the field).
        OrderListLeg { leg: usize, why: String } => "tradehub.order_list.leg": "order list: leg {leg}: {why}",
        /// An order list with a leg that names no order client id of its own.
        OrderListOrderClientIdMissing => "tradehub.order_list.order_client_id_missing":
            "order list: a remote order list requires a pre-minted order_client_id on every leg",
        /// An order list whose ids or links `vike_model` refuses (`why`: its words).
        OrderListInvalid { why: String } => "tradehub.order_list.invalid": "order list: {why}",
        /// An order list whose engine is not published with what it trades.
        OrderListEngineUnknown { key: String, venue: String, label: String }
            => "tradehub.order_list.engine_unknown":
            "order list: this node publishes no engine for `{key}` (venue `{venue}`, account `{label}`) that says \
             what it trades, so it cannot tell whether that engine can hold this list. The order list was REFUSED \
             rather than sent to an engine nobody checked",
        /// An order list with a stop leg for an engine on a spot lane that cannot hold one.
        OrderListSpotLane { key: String, trades: String, venue: String, perp_suffix: &'static str }
            => "tradehub.order_list.spot_lane":
            "order list: this node's `{key}` engine trades {trades}, on {venue}'s spot lane, and that lane cannot \
             hold a stop yet (the venue would reject it only after the entry filled). No order list with a stop \
             leg goes to `{key}` on this node until it runs a perp-lane engine (a `{perp_suffix}` symbol)",
        /// An order list on a symbol its engine does not trade.
        OrderListSymbolNotTraded { key: String, trades: String, symbol: String }
            => "tradehub.order_list.symbol_not_traded":
            "order list: this node's `{key}` engine trades {trades}, not `{symbol}`. Every leg goes to that one engine, \
             and only a plain order adds a symbol to an engine, so this list would land on another instrument. \
             Send it on a symbol that engine trades",
        /// A post-only limit order that would cross the book this node holds (decision 0242, item 8).
        PostOnlyWouldCross {
            word: &'static str,
            limit: f64,
            touched: &'static str,
            taker: f64,
            symbol: String,
            venue: String,
            rest: &'static str,
        } => "tradehub.door.post_only_would_cross":
            "post-only {word} {limit} would cross the best {touched} {taker} ({symbol} on {venue}): it \
             would execute immediately as a taker; the order was NOT sent. Price it {rest} to rest in the \
             book as a maker, or send a plain limit order to take liquidity",
        /// A give-back naming no symbol (decision 0205).
        UnadmitEmptySymbol => "tradehub.unadmit.empty_symbol":
            "unadmit: an empty symbol names nothing to give back",
        /// A give-back's account label that does not parse (`why` is the parser's words).
        UnadmitAccount { why: String } => "tradehub.unadmit.account": "unadmit: `account` — {why}",
        /// A give-back of the symbol the engine was started on.
        UnadmitOwnSymbol { symbol: String, route_key: String } => "tradehub.unadmit.own_symbol":
            "`{symbol}` is the symbol `{route_key}`'s engine was started on, not one it admitted on demand: it holds \
             no slot, so there is nothing to give back",
        /// A give-back of a symbol the engine did not admit; `held` lists the ones it did.
        UnadmitNotAdmitted { typed: String, route_key: String, held: String } => "tradehub.unadmit.not_admitted":
            "`{typed}` is not one of the symbols `{route_key}`'s engine admitted ({held}); nothing to give back",
        /// A give-back of a symbol the engine still holds a position in.
        UnadmitPositionOpen { route_key: String, size: f64, listed: String } => "tradehub.unadmit.position_open":
            "`{route_key}` holds a position of {size} in `{listed}`; close it first (an engine gives a symbol back \
             only when it is flat in it)",
        /// A give-back of a symbol with orders that have not ended; `them` is `it` or `them`.
        UnadmitLiveOrders {
            route_key: String,
            count: usize,
            listed: String,
            order_client_id: String,
            status: String,
            them: &'static str,
        } => "tradehub.unadmit.live_orders":
            "`{route_key}` has {count} live order(s) in `{listed}` (for one: `{order_client_id}`, {status}; a \
             conditional counts); cancel {them} first",
        /// An un-latch naming no mount (decision 0239).
        UnlatchEmptyMountId => "tradehub.unlatch.empty_mount_id": "unlatch: an empty mount id names no mount",
        /// An un-latch of a mount this node does not run; `ids` lists the ones it does.
        UnlatchUnknownMount { typed: String, ids: String } => "tradehub.unlatch.unknown_mount":
            "`{typed}` is not a mount this node runs ({ids}); `vike-cli trade strategy ls` lists them",
        /// An un-latch of a mount that is not latched.
        UnlatchNotLatched { id: String, venue: String, symbol: String } => "tradehub.unlatch.not_latched":
            "mount `{id}` ({venue}/{symbol}) is not latched: there is nothing to clear",
        /// An un-latch of a mount that still holds a position; `how` says what its budget does about it.
        UnlatchPositionOpen { id: String, venue: String, symbol: String, position: f64, how: &'static str }
            => "tradehub.unlatch.position_open":
            "mount `{id}` ({venue}/{symbol}) still holds {position}: unlatch it once it is flat; {how}",
    }
}
framed!(DoorRefusal);

// ===== end of section 1 =====

// ===== section 2: a symbol's admission at the door (`crate::server::symbol_admission`) =====

vike_utility::refusal_enum! {
    /// The door's NO to an order's SYMBOL (`crate::server::symbol_admission`'s `plan_submit_symbol` and the gates
    /// beside it). A variant holding `engine` refuses a symbol the addressed engine does not trade: `engine` is what
    /// that engine trades and what the order named (`symbol_admission`'s `engine_words`), the rest is the cause. A
    /// `why` is words another party said (the facts source, the gatewayhub, `vike_catalog`), quoted as they came.
    #[derive(Debug, Clone, PartialEq)]
    pub enum SymbolRefusal with ids SymbolRefusalId {
        /// The venue's adapter signs every order on the engine's mounted symbol (`VenueSymbolRule::MountedOnly`).
        MountedOnly { engine: String, venue: String, reason: &'static str } => "tradehub.symbol.mounted_only":
            "submit: {engine}: {venue}'s adapter places every order on the symbol the engine was mounted on \
             ({reason}), so this order would have been placed on another instrument or priced on the wrong grid. \
             Send it on a symbol that engine trades. The order was REFUSED",
        /// A LIVE account takes a new symbol only with an account-wide exposure ceiling armed.
        LiveWithoutAccountCeiling { engine: String } => "tradehub.symbol.live_without_account_ceiling":
            "submit: {engine}: this is a LIVE account with no account-wide exposure ceiling armed, and the \
             per-symbol ceiling (`max_total_exposure`) would then apply to each new symbol on its own. Arm one first: \
             `vike-cli config set policy.max_account_exposure <amount in the account's currency>` (applied at the \
             next restart), or this account's own `vike-cli account set-exposure`. The order was REFUSED",
        /// A `paper` engine is one book on its one symbol.
        PaperOneBook { engine: String } => "tradehub.symbol.paper_one_book":
            "submit: {engine}: a paper account trades only the symbol it was started on. The order was REFUSED",
        /// The engine's block states no tier, so it is never wider than its own symbol.
        TierUnknown { engine: String } => "tradehub.symbol.tier_unknown":
            "submit: {engine}: this node does not say what kind of account this is, so it trades only the symbol it \
             was started on. The order was REFUSED",
        /// A market NAME typed on a surface with no facts source to resolve it.
        NoNameLookup { engine: String } => "tradehub.symbol.no_name_lookup":
            "submit: {engine}: this surface cannot look up a market name; give the outcome's token id (a number of \
             20 or more digits), or place the order from the Trade window or the CLI. The order was REFUSED",
        /// The facts source could not resolve a market name; `why` is its words.
        NameUnresolved { engine: String, why: String } => "tradehub.symbol.name_unresolved":
            "submit: {engine}: {why}. The order was REFUSED",
        /// The typed symbol names no kept row (no list, no row or two rows); `why` is `vike_catalog`'s words.
        UserNameRefused { engine: String, why: String } => "tradehub.symbol.user_name_refused":
            "submit: {engine}: {why}. The order was REFUSED",
        /// ibkr: a stock of another currency than the engine's money, on an account that does not convert.
        StockMoneyDiffers {
            engine: String,
            symbol: String,
            stock_money: String,
            mounted: String,
            money: String,
            wanted: String,
            hint: String,
        } => "tradehub.symbol.stock_money_differs":
            "submit: {engine}: `{symbol}` is a {stock_money} stock and this engine, mounted on `{mounted}`, counts its \
             money in {money} (the ledger is one currency and nothing converts). Use an account mounted on a \
             {wanted} contract{hint}. The order was REFUSED",
        /// dukascopy, fxcm: a pair quoted in another money than the engine's, on an account that does not convert.
        PairMoneyDiffers {
            engine: String,
            symbol: String,
            quote: String,
            mounted: String,
            money: String,
            wanted: String,
        } => "tradehub.symbol.pair_money_differs":
            "submit: {engine}: `{symbol}` is quoted in {quote} and this engine, mounted on `{mounted}`, counts its \
             money in {money} (the ledger is one currency and nothing converts). Use an account mounted on a pair \
             quoted in {wanted}. The order was REFUSED",
        /// binance, aster: no kept row names the mounted symbol, so the engine's lane is unknown.
        LaneUnknown { engine: String, mounted: String, venue: String } => "tradehub.symbol.lane_unknown":
            "submit: {engine}: this engine is mounted on `{mounted}`, which no {venue} kept row names, so which lane \
             it runs (spot or futures) cannot be told and no other symbol is admitted into it. The order was REFUSED",
        /// binance, aster: a symbol of the other exec lane than the engine's; `hint` names where it goes.
        OtherLane {
            engine: String,
            symbol: String,
            other: &'static str,
            venue: String,
            lane: &'static str,
            hint: String,
        } => "tradehub.symbol.other_lane":
            "submit: {engine}: `{symbol}` is a {other} symbol and this engine runs {venue}'s {lane} lane (an engine \
             has one lane and one user-data stream, so a fill on the other lane would never arrive). Use a {lane} \
             symbol, {hint}. The order was REFUSED",
        /// The engine already holds `cap` symbols beyond its own (`vike_execution::MAX_EXTRA_SYMBOLS`).
        TooManySymbols { engine: String, cap: usize } => "tradehub.symbol.too_many_symbols":
            "submit: {engine}: an engine admits at most {cap} symbols beyond the one it was started on; give one back \
             that holds no position and no order (`vike-cli trade account remove-symbol <account> <symbol>`). The order \
             was REFUSED",
        /// A surface with no facts source cannot admit a new symbol.
        NoFactsSource { engine: String } => "tradehub.symbol.no_facts_source":
            "submit: {engine}: this surface cannot look up a symbol's facts (its grid and contract), so it cannot \
             admit a new symbol; place the order from the Trade window or the CLI. The order was REFUSED",
        /// No price feed can be had for a new symbol's opening order; `why` is the facts source's words.
        NewNoPriceFeed { engine: String, symbol: String, why: String } => "tradehub.symbol.new_no_price_feed":
            "submit: {engine}: no price feed for `{symbol}`: {why}. The order was REFUSED",
        /// The facts lookup refused the symbol; `why` is the facts source's (the gatewayhub's) words, under the
        /// bridge's id when it has one: that id is the one shown.
        FactsRefused { engine: String, symbol: String, why: CarriedRefusal }
            => carries why, "tradehub.symbol.facts_refused":
            "submit: {engine}: cannot admit `{symbol}`: {why}. The order was REFUSED",
        /// The facts source answered for another symbol or account than the one asked.
        FactsMismatch { engine: String, answered: String, answered_key: String, symbol: String, key: String }
            => "tradehub.symbol.facts_mismatch":
            "submit: {engine}: the facts source answered for `{answered}` on `{answered_key}` when `{symbol}` on \
             `{key}` was asked. The order was REFUSED",
        /// The venue states no tick or no lot for the symbol.
        NoGrid { engine: String, venue: String, missing: &'static str, symbol: String } => "tradehub.symbol.no_grid":
            "submit: {engine}: {venue} publishes no {missing} for `{symbol}`, so its price and size cannot be rounded \
             on its own grid. The order was REFUSED",
        /// deribit: only an engine mounted on a perpetual or a dated future adds another instrument.
        DeribitMountNotAFuture { engine: String, mounted: String } => "tradehub.symbol.deribit_mount_not_a_future":
            "submit: {engine}: this engine is mounted on `{mounted}`, which is not a perpetual or a dated future, and \
             only an engine mounted on one adds another instrument. Use an account mounted on a perpetual. The order \
             was REFUSED",
        /// deribit: the symbol counts money the other way (inverse or linear) than the engine.
        DeribitMoneyModel {
            engine: String,
            symbol: String,
            theirs: &'static str,
            mounted: String,
            ours: &'static str,
            kind: &'static str,
        } => "tradehub.symbol.deribit_money_model":
            "submit: {engine}: `{symbol}` is {theirs} but this engine is mounted on `{mounted}`, which is {ours}, and \
             one account trades one money model. Use an account mounted on a {kind} perpetual. The order was REFUSED",
        /// One settle coin per engine (decision 0258).
        SettleCoin { engine: String, symbol: String, other: String, mounted: String, ours: String }
            => "tradehub.symbol.settle_coin":
            "submit: {engine}: `{symbol}` settles in {other} but this engine, mounted on `{mounted}`, keeps its books \
             in {ours}, and one engine holds one settle coin (the venue keeps one wallet per coin). Use an account \
             mounted on a {other} contract. The order was REFUSED",
        /// One contract kind per engine (decision 0233): admitting it would make the engine drop fills.
        ContractKind {
            engine: String,
            symbol: String,
            theirs: &'static str,
            venue: String,
            mounted: String,
            ours: &'static str,
            drops: String,
            kind: &'static str,
        } => "tradehub.symbol.contract_kind":
            "submit: {engine}: `{symbol}` is {theirs} but this {venue} engine, mounted on `{mounted}`, is {ours}, and \
             an engine counts money one way: it cannot value a fill of the other kind, so admitting `{symbol}` would \
             make the engine DROP {drops}, while the venue still fills the orders and the node books nothing. The \
             symbol is NOT admitted. Use a separate account mounted on {kind} contract; one engine holding both kinds \
             is not built (decision 0233). The order was REFUSED",
        /// The facts carry a conversion rate and no currency to apply it to.
        RateWithoutCurrency { engine: String, venue: String, symbol: String }
            => "tradehub.symbol.rate_without_currency":
            "submit: {engine}: {venue}: the facts for `{symbol}` carry a conversion rate and no currency to apply it \
             to, so the symbol is NOT admitted. The order was REFUSED",
        /// A symbol quoted in another currency whose facts carry no usable rate (decision 0183).
        NoUsableRate { engine: String, symbol: String, currency: String } => "tradehub.symbol.no_usable_rate":
            "submit: {engine}: `{symbol}` is quoted in {currency} and this account's money is another currency, so it \
             is admitted only with a usable conversion rate, and the facts carry none. The symbol is NOT admitted (no \
             rate, no admission: it is never valued at par). The order was REFUSED",
        /// A symbol quoted in another currency on an account that does not convert; `why` says which rule binds.
        NoConversion { engine: String, symbol: String, currency: String, why: String }
            => "tradehub.symbol.no_conversion":
            "submit: {engine}: `{symbol}` is quoted in {currency}: {why}. The order was REFUSED",
        /// ibkr: the facts of a stock of another currency carry no conversion into the engine's money.
        StockConversionMissing { engine: String, symbol: String, stock: String, money: String }
            => "tradehub.symbol.stock_conversion_missing":
            "submit: {engine}: `{symbol}` is a {stock} stock and this engine counts its money in {money}, and the \
             facts carry no conversion of {stock} into it (the gatewayhub converts a quote currency only for a `demo` \
             account whose ledger is anchored to the account's currency), so it is NOT admitted (it is never valued \
             at par). The order was REFUSED",
        /// A pair read as quoted in another money whose facts state another currency (or none).
        FactsCurrencyDiffers { engine: String, symbol: String, expected: String, stated: String }
            => "tradehub.symbol.facts_currency_differs":
            "submit: {engine}: `{symbol}` is quoted in {expected} and this account's money is another currency, but \
             the facts for it state {stated} (a gatewayhub that does not convert answers none). The symbol is NOT \
             admitted (no rate, no admission: it is never valued at par). The order was REFUSED",
        /// A pair in the mount's own money whose facts state a currency: the engine's ledger is not anchored.
        LedgerNotAnchored { engine: String, symbol: String, currency: String, mounted: String }
            => "tradehub.symbol.ledger_not_anchored":
            "submit: {engine}: `{symbol}` is quoted in {currency}, the money this engine's mounted pair `{mounted}` \
             counts in, but the facts state {currency} against an account currency that is another one: the engine's \
             ledger is not anchored to the account's currency, so it would refuse the admission \
             (`account-currency-unset`) and a price place would be held for a symbol it never trades. The symbol is \
             NOT admitted. Use an account mounted on a pair quoted in the account's currency. The order was REFUSED",
        /// A reduce-only order on a symbol the account holds nothing in.
        ReduceOnlyNewSymbol { engine: String } => "tradehub.symbol.reduce_only_new_symbol":
            "submit: {engine}: a reduce-only order can only close a position this account holds in the symbol, and \
             it holds none; send an opening order to start trading it. The order was REFUSED",
        /// A new symbol's order under the venue's minimum quantity.
        NewBelowMinQty { engine: String, typed: f64, min: f64 } => "tradehub.symbol.new_below_min_qty":
            "submit: {engine}: the quantity {typed} is below the symbol's minimum {min} (no symbol slot was spent). \
             The order was REFUSED",
        /// A new symbol's order that rounds to nothing on the venue's lot.
        NewBelowOneLot { engine: String, typed: f64, lot: f64 } => "tradehub.symbol.new_below_one_lot":
            "submit: {engine}: the quantity {typed} is below one lot ({lot}) (no symbol slot was spent). The order \
             was REFUSED",
        /// A new symbol's priced order under the venue's minimum notional.
        NewBelowMinNotional { engine: String, notional: f64, min: f64 } => "tradehub.symbol.new_below_min_notional":
            "submit: {engine}: the order's value {notional} is below the symbol's minimum notional {min} (no symbol \
             slot was spent). The order was REFUSED",
        /// A per-order venue whose engine cannot judge a foreign symbol (`VenueSymbolRule::PerOrderUnsafe`).
        PerOrderUnsafe { engine: String, venue: String, reason: &'static str } => "tradehub.symbol.per_order_unsafe":
            "submit: {engine}: {venue}'s adapter reads the order's symbol, but the engine cannot judge another one \
             ({reason}). Send it on a symbol that engine trades. The order was REFUSED",
        /// A per-order venue with no price source: an opening order without a limit price is judged at notional 0.
        PerOrderUnpriced { engine: String, venue: String } => "tradehub.symbol.per_order_unpriced":
            "submit: {engine}: {venue} has no price source for a symbol the engine was not started on, so an opening \
             order without a limit price would be judged at notional 0 (every ceiling, the margin check and the \
             collar would pass). Send a limit price, or a reduce-only order. The order was REFUSED",
        /// polymarket: a MARKET order that names a price (every order there is a signed limit at its price).
        MarketOrderPriced { venue: String } => "tradehub.symbol.market_order_priced":
            "submit: {venue}: a market order takes no price (every order here is a signed limit at its price, so it \
             would rest or fill at that number): send a limit order, or drop the price. The order was REFUSED and \
             nothing was sent",
        /// A reconcile confirm whose row books a symbol the engine cannot be admitted; `why` is that refusal's text.
        ConfirmUnadmittable { id: u64, symbol: String, why: String } => "tradehub.symbol.confirm_unadmittable":
            "reconcile confirm #{id}: the account's engine does not trade `{symbol}`, and it cannot be admitted for the \
             confirm: {why}. Nothing was sent; the row stays held",
        /// A `paper` account priced through its venue's demo login with no demo login running.
        PaperWithoutDemoLogin { no_login: String, venue: String, needed: String }
            => "tradehub.symbol.paper_without_demo_login":
            "{no_login}: a paper account of {venue} is priced and filled from the quotes of {venue}'s demo login, so \
             this order would never fill. {needed}",
        /// A `paper` order whose symbol no quote lane can reach; `why` is the cause.
        PaperNoQuote { symbol: String, venue: String, why: String } => "tradehub.symbol.paper_no_quote":
            "no price: no quote of `{symbol}` on {venue} can reach this paper account ({why}), so the order would \
             never fill at a current price",
        /// A `paper` order on a symbol no strategy mount of this node trades.
        PaperUnmounted { symbol: String, venue: String } => "tradehub.symbol.paper_unmounted":
            "no price: no strategy mount trades `{symbol}` on {venue} on this node, so no bar series of it reaches \
             this paper account and the order would never fill (a paper book fills on its symbol's closed bars). \
             Mount a strategy on `{symbol}` (`vike-cli trade strategy mount`), or trade it on a demo account",
        /// A `paper` order on a paper-only node whose feeds, wired at its start, reach no series of the symbol.
        PaperUnfed { symbol: String, venue: String } => "tradehub.symbol.paper_unfed":
            "no price: no price feed reaches `{symbol}` on {venue} on this node: a paper-only node \
             (flags.tradehub_live off) wires its feeds once, at its start, for the symbols its profile's mounts \
             trade, and `{symbol}` is not one of them (a mount made while the node runs, or a declared leg, gets no \
             price), so the order would never fill. Add a mount of `{symbol}` to the profile and restart, or trade it \
             on a demo account",
        /// A `paper` order while its venue's market is closed; `closed` is `symbol_admission`'s `paper_market_closed`.
        PaperMarketClosed { closed: String } => "tradehub.symbol.paper_market_closed":
            "{closed}. The order was REFUSED (a paper book would fill it on the last quote before the close); send it \
             again after the open",
        /// No price feed can be had for the symbol an order is held for; `why` is the facts source's words.
        NoPriceFeed { symbol: String, why: String } => "tradehub.symbol.no_price_feed":
            "submit: no price feed for `{symbol}`: {why}",
        /// A lane twin's own symbol with no price in its engine, on a surface that cannot ask for one.
        TwinNoPriceSource { key: String, symbol: String } => "tradehub.symbol.twin_no_price_source":
            "submit: the perp lane `{key}` has no price for `{symbol}` in its engine, and this surface cannot ask the \
             node's price feed for one, so the order's size could not be judged against any ceiling. The order was \
             REFUSED: place it from the Trade window or the CLI",
        /// A lane twin's own symbol with no price yet, and no price feed to be had; `why` is the facts source's words.
        TwinNoPriceFeed { key: String, symbol: String, why: String } => "tradehub.symbol.twin_no_price_feed":
            "submit: the perp lane `{key}` has no price for `{symbol}` in its engine yet, and no price feed can be \
             had for it: {why}. The order was REFUSED",
        /// A LIVE lane twin's extra symbol whose leverage the exchange has not confirmed.
        LeverageUnconfirmed { key: String, symbol: String } => "tradehub.symbol.leverage_unconfirmed":
            "submit: the perp lane `{key}` is a LIVE account and the exchange has not confirmed that it holds the \
             account's stated leverage for `{symbol}`: the figure is made real and read back when the symbol is \
             admitted, and a symbol that trades at whatever leverage the exchange already holds for it would be \
             leverage nobody chose on real money. The order was REFUSED; the next attempt asks the exchange again \
             (the gatewayhub's log says why it was not confirmed)",
        /// An admission that would commit a leverage while the engine is HALTED or REDUCING; `state` says which.
        AdmissionWhileNotActive { state: &'static str, symbol: String, route_key: String }
            => "tradehub.symbol.admission_while_not_active":
            "submit: {state}, and admitting `{symbol}` into `{route_key}` commits its leverage to the exchange, which \
             can change the account's leverage for that symbol there; a node that is not trading changes nothing on \
             the exchange. The order was REFUSED and nothing was sent: resume trading first (`vike-cli trade \
             resume`), or place the order on a symbol the account already trades",
        /// An opening order under the minimum quantity of the grid this node holds.
        BelowMinQty { verb: &'static str, name: String, typed: f64, min: f64 } => "tradehub.symbol.below_min_qty":
            "{verb}: `{name}`: the quantity {typed} is below the symbol's minimum {min}. The order was REFUSED and \
             nothing was sent",
        /// An opening order that rounds to nothing on the lot of the grid this node holds.
        BelowOneLot { verb: &'static str, name: String, typed: f64, lot: f64 } => "tradehub.symbol.below_one_lot":
            "{verb}: `{name}`: the quantity {typed} is below one lot ({lot}). The order was REFUSED and nothing was \
             sent",
        /// A priced opening order under the minimum notional of the grid this node holds.
        BelowMinNotional { verb: &'static str, name: String, notional: f64, min: f64 }
            => "tradehub.symbol.below_min_notional":
            "{verb}: `{name}`: the order's value {notional} is below the symbol's minimum notional {min}. The order \
             was REFUSED and nothing was sent",
        /// The order's price stream is down (the lane's own verdict); `held` is how long the door held it.
        PriceStreamDown { symbol: String, held: String, verdict: String } => "tradehub.symbol.price_stream_down":
            "submit: `{symbol}` has no price on this node{held}: {verdict}. The order was REFUSED: a retry cannot \
             help until that stream is back",
        /// The order's price feed was just subscribed and nothing has arrived; `held` is how long the door held it.
        NoPriceYet { symbol: String, held: String } => "tradehub.symbol.no_price_yet":
            "submit: `{symbol}` has no price on this node yet (its price feed was just subscribed and nothing has \
             arrived{held}). The order was REFUSED: retry in a moment",
    }
}
framed!(SymbolRefusal);

// ===== end of section 2 =====

// ===== section 3: the command's lowering, the control limits, the preview and the mount verb's wire answers
// (`crate::server::control`, `preview`, `crate::mount_factory`) =====

vike_utility::refusal_enum! {
    /// A control command the node refuses in its own words: the core that could not take it
    /// (`crate::server::control::ControlAcceptError`), what `crate::server::control`'s `accept_command` says inline,
    /// and a frame its `lower_command` declines to lower.
    #[derive(Debug, Clone, PartialEq)]
    pub enum CommandRefusal with ids CommandRefusalId {
        /// The core's ingest lane is CLOSED: the node is shutting down, nothing was executed, and the session ends.
        ShuttingDown => "tradehub.shutting_down": "core is shutting down",
        /// A field of `verb`'s frame that another crate's rule or reader refused (an account label, an order word, a
        /// lifetime, a display size, a release's price venue, the price a release's venue could not bring): `why` is
        /// that rule's own words.
        FieldRefused { verb: &'static str, field: &'static str, why: String }
            => "tradehub.command.field_refused": "{verb}: `{field}` — {why}",
        /// A verb whose one naming field is blank: it can name nothing.
        EmptyField { verb: &'static str, what: &'static str }
            => "tradehub.command.empty_field": "{verb}: empty {what}",
        /// A remote submit with no order client id: a peer must pre-mint its own, or it has no stable handle to cancel
        /// or deduplicate by.
        SubmitOrderClientIdMissing => "tradehub.command.submit.order_client_id_missing":
            "remote submit requires a pre-minted order_client_id",
        /// A submit's prices for its kind (`vike_model::order_terms_refusal`'s words in `why`). This and the three
        /// below are `crate::server::control`'s `submit_terms_refusal`, the one lowering refusal a preview says:
        /// their ids are its own, so a preview decides by the id whether the lowering's first fault is one of them.
        SubmitTerms { why: String } => "tradehub.command.submit.terms": "submit: order terms — {why}",
        /// A limit's attributes its rule refuses (`vike_model::attributes_refusal`'s words in `why`).
        SubmitAttributes { why: String } => "tradehub.command.submit.attributes":
            "submit: `attributes` — {why}",
        /// A stop's adjustment its rule refuses (`vike_model::stop_adjust_refusal`'s words in `why`).
        SubmitAdjust { why: String } => "tradehub.command.submit.adjust": "submit: `adjust` — {why}",
        /// A trail on a kind that is not a trailing one: a paper book would lower it as a trailing stop.
        StrayTrail { order_type: String, trail: f64 } => "tradehub.command.submit.stray_trail":
            "submit: order terms — a trail belongs to a trailing, trailing_limit or trailing_take_profit order; \
             this {order_type} order states {trail}",
        /// A peg whose anchor is not one of the four words, exactly.
        PegAnchorUnknown { anchor: String, accepted: String } => "tradehub.command.submit.peg_anchor_unknown":
            "submit: `peg` — the anchor {anchor:?} is not one of {accepted} (exactly)",
        /// A release's price condition at a level that is no price.
        ReleaseLevelNotAPrice { level: f64 } => "tradehub.command.submit.release.level_not_a_price":
            "submit: `release` — the price condition's level {level} is not a price above zero",
        /// A release names a symbol and states no price condition to read it.
        ReleaseSymbolWithoutCondition { symbol: String }
            => "tradehub.command.submit.release.symbol_without_condition":
            "submit: `release` — the symbol `{symbol}` belongs to a price condition, and this release states none",
        /// A release's price condition names a blank symbol.
        ReleaseSymbolBlank => "tradehub.command.submit.release.symbol_blank":
            "submit: `release` — the price condition's symbol is blank",
        /// A release names a venue and states no price condition to read on it.
        ReleaseVenueWithoutCondition { venue: String }
            => "tradehub.command.submit.release.venue_without_condition":
            "submit: `release` — the venue `{venue}` belongs to a price condition, and this release states none",
        /// A price condition read on another venue that names no symbol: the venues spell one instrument differently.
        ReleaseVenueWithoutSymbol { venue: String } => "tradehub.command.submit.release.venue_without_symbol":
            "submit: `release` — the price condition read on `{venue}` names no symbol, and the venues spell one \
             instrument differently: name it, e.g. above:BTCUSDT:100000",
        /// A release read on another venue, on a surface with no price source: no price would ever come.
        ReleaseNoPriceSource { symbol: String, venue: String }
            => "tradehub.command.submit.release.no_price_source":
            "submit: `release` — this node has no price source, so it never reads {symbol} on {venue}: REFUSED \
             rather than held on a price nobody reads",
        /// A modify's new trigger that is no price.
        ModifyTriggerNotAPrice { trigger: f64 } => "tradehub.command.modify.trigger_not_a_price":
            "modify: `new_trigger_price` must be a price above zero, got {trigger}",
        /// A params update whose payload is not the core's `StrategyParams` (`why` is the decoder's words).
        UpdateParamsUndecodable { why: String } => "tradehub.command.update_params.undecodable":
            "update_params: payload is not a vike StrategyParams \
             (expected the externally-tagged form, e.g. {{\"SpreadMaker\": {{…}}}}): {why}",
        /// A settings write handed to the core's lowering: it is lowered at the door, never there.
        SetSettingNotCore => "tradehub.command.set_setting.not_a_core_command":
            "set_setting: not a core command — it is lowered at the daemon edge (accept_command)",
        /// A settings write on a surface that serves no settings source.
        SettingsWriteUnavailable => "tradehub.command.set_setting.unavailable":
            "settings write unavailable: this surface serves no settings source",
        /// An order on a venue this paper-only node prices nothing on: it would rest unfilled without a word.
        /// `no_price` is `vike_tradehub_client::wire::no_price_feed`'s words.
        NoPriceFeed { no_price: String, venue: String } => "tradehub.command.no_price_feed":
            "{no_price}: a paper order fills on its symbol's quotes and bars, and this node prices only the keyless \
             venues its strategy mounts trade, through the datahub, so no order on {venue} can fill here. A \
             credentialed venue (its prices need its demo login) is priced only with the switch on (`vike-cli \
             config set flags.tradehub_live true`, then restart; it also runs this node's `demo` and `live` \
             accounts at their own tiers)",
    }
}
framed!(CommandRefusal);

vike_utility::refusal_enum! {
    /// The control door's own limits (`crate::server::control::ControlLimits`): the connection's rate token and the
    /// node's per-order notional ceiling, `policy.max_notional_per_order`.
    #[derive(Debug, Clone, PartialEq)]
    pub enum LimitRefusal with ids LimitRefusalId {
        /// The connection's rate token is spent.
        RateLimited { rate_per_sec: f64 } => "tradehub.limit.rate":
            "rate limited: control commands capped at {rate_per_sec}/s on this node",
        /// A qty-raising modify with no price, on an order with no standing price: its notional cannot be checked.
        ModifyUnpriced { qty: f64, max: f64 } => "tradehub.limit.modify_unpriced":
            "modify to qty {qty} names no price and the order has no standing price, so its notional cannot be \
             checked against the node's policy ceiling max_notional_per_order {max:.2} — re-send the modify with \
             an explicit price",
        /// A bracket leg over the ceiling; `note` says why the notional is not `qty × price` (empty at the default value).
        BracketLegOverCeiling { notional: f64, leg: &'static str, max: f64, note: String }
            => "tradehub.limit.over_ceiling.bracket_leg":
            "order notional {notional:.2} (the bracket's {leg} leg) exceeds the node's policy ceiling \
             max_notional_per_order {max:.2}{note}",
        /// An order list leg over the ceiling.
        OrderListLegOverCeiling { notional: f64, leg: usize, max: f64, note: String }
            => "tradehub.limit.over_ceiling.order_list_leg":
            "order notional {notional:.2} (the order list's leg {leg}) exceeds the node's policy ceiling \
             max_notional_per_order {max:.2}{note}",
        /// A combo leg priced by its own limit over the ceiling.
        ComboLegOverCeiling { notional: f64, leg: usize, max: f64, note: String }
            => "tradehub.limit.over_ceiling.combo_leg":
            "order notional {notional:.2} (the combo's leg {leg}) exceeds the node's policy ceiling \
             max_notional_per_order {max:.2}{note}",
        /// A combo leg sized at the price this node holds for it; `why` is the ceiling's own refusal.
        ComboLegAtHeldPrice { leg: usize, why: String } => "tradehub.limit.over_ceiling.combo_leg_held_price":
            "the combo's leg {leg}: {why}",
        /// A factor of the notional that is not a finite number: refused, never compared.
        NotionalNotFinite { qty: f64, price: f64, max: f64 } => "tradehub.limit.notional_not_finite":
            "order qty {qty} at price {price} has no finite notional, so it cannot be checked against the node's \
             policy ceiling max_notional_per_order {max:.2} — refused rather than passed (a NaN notional is never \
             above the ceiling)",
        /// An order over the ceiling.
        OverCeiling { notional: f64, max: f64, note: String } => "tradehub.limit.over_ceiling":
            "order notional {notional:.2} exceeds the node's policy ceiling max_notional_per_order {max:.2}{note}",
        /// A priceless opening order on a live lane twin's own symbol, with no price to size it against the ceiling.
        LaneTwinUnpriced { verb: &'static str, route_key: String, own: String, what: &'static str }
            => "tradehub.limit.lane_twin_unpriced":
            "{verb}: the perp lane `{route_key}` has no price for `{own}` to size this {what} against the node's \
             notional ceiling. The {what} was REFUSED: retry in a moment",
    }
}
framed!(LimitRefusal);

vike_utility::refusal_enum! {
    /// A runtime mount (`CommandWire::MountStrategy`) the door refuses before the core sees it
    /// (`crate::server::control`'s `mount_refusal` and `lower_command`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MountVerbRefusal with ids MountVerbRefusalId {
        /// The mount refused by a rule this verb does not word: the profile vocabulary a `[strategy]` table faces at
        /// load (`crate::mount_factory::validate_spec`), the strategy file's fence and read, its strategy code.
        Refused { why: String } => "tradehub.mount.refused": "mount_strategy: {why}",
        /// The mount's lowering handed a frame that is no mount.
        NotAMount => "tradehub.mount.not_a_mount": "mount_strategy: this frame is no mount",
        /// A mount's `params` that no parameter table can hold (a `null`, a non-object, an integer above `i64::MAX`).
        ParamsNotATable { why: String } => "tradehub.mount.params_not_a_table":
            "mount_strategy: `params` is not a parameter table: {why}",
    }
}
framed!(MountVerbRefusal);

vike_utility::refusal_enum! {
    /// Why a strategy file, or its strategy code, cannot be had for a mount (`crate::mount_factory`): a wire mount's
    /// answer (inside `tradehub.mount.refused`) and a boot mount's refusal alike.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MountFileRefusal with ids MountFileRefusalId {
        /// The process declared no project, so it has no `strategies/` folder.
        NoProject => "tradehub.mount.file.no_project":
            "this process declared no project, so it has no `strategies/` folder to read a strategy file from: \
             start it inside a project (the folder holding `settings/`)",
        /// A wire `file` that is not a `.rs` or `.rhai` file inside the strategies folder.
        OutsideStrategies { file: String } => "tradehub.mount.file.outside_strategies":
            "`file` {file:?} is not a `.rs` or `.rhai` file inside this node's `strategies/` folder, and a strategy \
             file named to the node may be nothing else. Put the file under the project's `strategies/` folder \
             and name its path there.",
        /// The strategy file could not be read (`why` is the operating system's words).
        ReadFailed { path: String, why: String } => "tradehub.mount.file.read_failed":
            "read strategy file {path}: {why}",
        /// The strategy file is not a regular file.
        NotARegularFile { path: String } => "tradehub.mount.file.not_a_regular_file":
            "read strategy file {path}: not a regular file",
        /// The strategy file is over the cap.
        TooBig { path: String, cap: u64 } => "tradehub.mount.file.too_big":
            "read strategy file {path}: larger than {cap} bytes",
        /// A mount that names no strategy, whose name its order client ids' strategy code is kept by.
        StrategyUnnamed { mount_id: String } => "tradehub.mount.strategy_code.unnamed":
            "mount `{mount_id}` names no strategy, and the strategy's name is what its order client ids' strategy \
             code is kept by: name the strategy",
        /// The settings store did not answer the mount's strategy code (`why` is the store's words).
        StrategyCodeUnread { mount_id: String, why: String } => "tradehub.mount.strategy_code.unread":
            "mount `{mount_id}`: its strategy code could not be read: {why}",
    }
}
framed!(MountFileRefusal);

// ===== end of section 3 =====

// ===== section 4: a profile's mount and strategy refusals (`crate::config`) =====

vike_utility::refusal_enum! {
    /// A daemon profile's refusal of a MOUNT or its STRATEGY file, at load (`crate::config::ProfileDaemon::from_toml_str`
    /// and its validators), at the file's resolve (`crate::config::ProfileDaemon::resolve_mount`) and at a `.rs`
    /// file's sandboxed open. A person reads it through `vike-cli config check` (`--check-profile`), the boot's
    /// refusal and a mount's NOT RUNNING row, and wrapped by the mount verb's answer. The profile's APIs carry it as
    /// its text (`Display`), so its bytes are what they always were.
    #[derive(Debug, Clone, PartialEq)]
    pub enum MountProfileRefusal with ids MountProfileRefusalId {
        /// The profile's TOML did not parse; `why` is the parser's own message.
        ParseFailed { why: String } => "tradehub.profile.parse_failed": "parse profile TOML: {why}",
        /// The maker's retired top-level knobs; `set` lists the ones this profile sets.
        RetiredMakerFields { set: String } => "tradehub.profile.mount.retired_maker_fields":
            "`qty`, `half_spread` and `resolution_ts_ms` are no longer profile fields (this \
             profile sets: {set}). The A-S maker reads its knobs from its own params table: \
             `[strategy] file = \"<the maker's file>\"` (`market_making/spread_maker.rs` as \
             shipped), then `[strategy.params]` `qty = …`, `half_spread = …`, \
             `resolution_ts = …` (the old \
             `resolution_ts_ms`, epoch-ms). A stored mount row's columns of those names are \
             refused the same way: store the profile again with the keys moved. Under any other \
             strategy these fields configured nothing: drop them",
        /// A mount with no `symbol`.
        NoSymbol => "tradehub.profile.mount.no_symbol":
            "a mount symbol is required: set `symbol` (on polymarket, the outcome token id)",
        /// A mount whose `symbol` is blank.
        EmptySymbol => "tradehub.profile.mount.empty_symbol": "the mount symbol must not be empty",
        /// A mount's `max_leverage` that is no leverage cap.
        MaxLeverageInvalid { cap: f64 } => "tradehub.profile.mount.max_leverage_invalid":
            "`max_leverage = {cap}` is not a leverage cap: it must be a finite number of at least 1 \
             (1 = no leverage). Drop the key to hold the mount to `policy.max_mount_leverage` alone",
        /// A mount symbol its venue's kept list spells another way; `why` is `vike_catalog`'s words.
        SymbolRespelled { why: String } => "tradehub.profile.mount.symbol_respelled":
            "the mount symbol: {why}",
        /// An `[inputs]` row the input grammar refuses; `why` is the grammar's words.
        InputRowUnparsed { name: String, read: String, why: String } =>
            "tradehub.profile.inputs.row_unparsed": "[inputs] {name:?} = {read:?}: {why}",
        /// `data_only = true` on a venue whose market data needs no key; `venues` lists the credentialed ones.
        DataOnlyKeylessVenue { venues: String, venue: String, upper: String } =>
            "tradehub.profile.mount.data_only_keyless_venue":
            "`data_only = true` is only meaningful for a venue whose MARKET DATA is \
             credentialed ({venues}), where the same credentials would otherwise arm exec; {venue} has a \
             keyless data plane, so its data-only mount is simply \"no {upper} credentials in the \
             store\" (absent credentials ARE the live gate). Drop the key",
        /// `[[mounts]]` beside top-level mount fields; `fields` lists them.
        BothSpellings { fields: String } => "tradehub.profile.mounts.both_spellings":
            "this profile sets BOTH spellings: `[[mounts]]` AND top-level mount field(s) {fields}. \
             With a `[[mounts]]` array every mount field lives inside its own row — a \
             top-level knob beside the array would configure nothing while reading as real. \
             Move the field(s) into a `[[mounts]]` row, or drop the array",
        /// Two `[[mounts]]` rows that both set `primary = true`.
        TwoPrimaries { first: usize, second: usize } => "tradehub.profile.mounts.two_primaries":
            "mounts[{first}] and mounts[{second}] both set `primary = true` — at most one mount may be \
             the primary, because the primary IS the daemon's singular identity (its summary \
             token, its mode line and its seed policy all read that one row). Drop the key \
             from every row but one; dropping it from all of them keeps the historical answer, \
             which is the FIRST row",
        /// One `[[mounts]]` row's own refusal (`why`), naming the row.
        RowRefused { index: usize, venue: String, symbol: String, why: String } =>
            "tradehub.profile.mounts.row_refused": "mounts[{index}] (venue={venue:?}, symbol={symbol:?}): {why}",
        /// Two `[[mounts]]` rows deriving one mount id.
        SameMountId { first: usize, second: usize, id: String } => "tradehub.profile.mounts.same_mount_id":
            "mounts[{first}] and mounts[{second}] derive the SAME mount id {id:?} — venue, symbol, \
             interval, strategy AND account all equal, so the two rows would share one \
             durable-state sidecar and one journal attribution key (the runtime refuses \
             that with a panic; this refusal is the load-time version that can name the \
             rows). Make one row distinct — a different interval, symbol, strategy or \
             `account` — or delete the duplicate. ⚠ Two rows on one venue and one symbol \
             that differ by `account` are an ordinary SPREAD and are NOT this error: the \
             account is part of the mount id, so they derive different ids",
        /// Two `[[mounts]]` rows on one venue answering `data_only` differently.
        DataOnlyDisagrees { first: usize, second: usize, venue: String } =>
            "tradehub.profile.mounts.data_only_disagrees":
            "mounts[{first}] and mounts[{second}] share venue {venue:?} but DISAGREE on `data_only` — \
             the declaration withholds that venue's credentials from the ONE exec engine \
             both rows share, so the two rows cannot have different answers. Set the same \
             value on both (or drop the key from both)",
        /// A mount that names no strategy file: there is no default strategy.
        NoStrategyFile => "tradehub.profile.strategy.no_file":
            "this mount names no strategy file: set `[strategy] file = \"<path>\"`, the path of a `.rs` or \
             `.rhai` file relative to the project's `strategies/` folder (for example \
             `file = \"trend/momentum.rs\"`). A mount without a file is not mounted: there is no default \
             strategy",
        /// The retired `[strategy] name` key.
        NameRetired { name: String } => "tradehub.profile.strategy.name_retired":
            "`[strategy] name = {name:?}` is retired: a strategy is a FILE, and nothing is mounted \
             by name. Set `[strategy] file = \"<path>\"` to the strategy's file under the \
             project's `strategies/` folder instead (a shipped `{name}` is `<folder>/{name}.rs` \
             there)",
        /// The retired `[strategy] rhai` key.
        RhaiKeyRetired { path: String } => "tradehub.profile.strategy.rhai_key_retired":
            "`[strategy] rhai = {path:?}` is retired: a script is a strategy FILE. Put it under \
             the project's `strategies/` folder and set `[strategy] file = \"<its path there>\"` \
             instead",
        /// A `.rhai` file's `[strategy.params]` values that are not numbers; `values` lists them.
        RhaiParamsNotNumbers { values: String } => "tradehub.profile.strategy.rhai_params_not_numbers":
            "a Rhai `[strategy.params]` value must be a NUMBER, got: {values}. Every key here is a \
             `param(name, default)` override baked into the script at compile; `param` takes \
             an f64, so any other TOML type would be silently dropped and the script would run \
             its own compiled default while this profile states otherwise.",
        /// A live mount on a venue this build has no feed arm for; `arms` lists the ones it has.
        VenueNotLiveWired { venue: String, arms: String } => "tradehub.profile.live.venue_not_wired":
            "venue {venue:?} is not live-wired in this build; the daemon can mount: {arms}",
        /// A live mount on a venue with a feed arm and no engine.
        LiveVenueNoEngine { venue: String } => "tradehub.profile.live.no_engine":
            "venue {venue:?} is live-wired for a feed but `assemble_runtime` builds no engine for it \
             — orders would have nowhere to go",
        /// An account-less live mount on another symbol than its venue's default account states.
        DefaultAccountSymbolDiffers { venue: String, stated: String, symbol: String } =>
            "tradehub.profile.live.default_account_symbol_differs":
            "{venue}'s default account is mounted on {stated:?} (the symbol its row states, \
             `account.symbol`), but this profile names {symbol:?}: that engine accepts exactly its \
             mounted symbol (`Engine::accepts_symbol` — no `extra_symbols` are wired), so every \
             order and fill would be SILENTLY DROPPED. A row naming a second `account` is exempt — \
             that account's engine is mounted on the row's own symbol",
        /// A live polymarket mount whose symbol is no outcome token id.
        NotAnOutcomeToken { symbol: String } => "tradehub.profile.live.not_an_outcome_token":
            "polymarket needs an outcome token id (a ≥20-digit decimal ERC-1155 id), got \
             {symbol:?}",
        /// A live hyperliquid mount on a symbol its kept list does not hold; `why` says why not.
        SymbolNotKept { symbol: String, why: String } => "tradehub.profile.live.symbol_not_kept":
            "hyperliquid mount symbol {symbol:?} is refused: {why}",
        /// A live mount's `seed_cash` that leaves the drawdown latch nothing to measure.
        SeedCashDisarmsLatch { seed: f64 } => "tradehub.profile.live.seed_cash_disarms_latch":
            "`seed_cash = {seed}` disarms the live daemon's 25% drawdown latch: it \
             measures the drop as a fraction of configured capital plus own PnL, so a \
             non-positive base leaves nothing to measure against. Omit it (default 1000) \
             or set the capital this mount is meant to risk",
        /// A strategy file whose header does not parse; `why` is the header reader's words.
        HeaderRefused { file: String, why: String } => "tradehub.profile.strategy.header_refused":
            "strategy file {file:?}: {why}",
        /// `[strategy.params]` keys the file's header does not declare.
        ParamsUnread { file: String, keys: String, reads: String } =>
            "tradehub.profile.strategy.params_unread":
            "strategy file {file:?} does not read these `[strategy.params]` keys: {keys}. A params \
             reader IGNORES what it does not recognise, so each of them would mount at the \
             strategy's COMPILED DEFAULT — a size knob among them is a live order at a size \
             nobody typed. It reads: {reads}.",
        /// `[strategy.params]` values of a type or range the file's reader cannot take.
        ParamsMistyped { file: String, values: String, reads: String } =>
            "tradehub.profile.strategy.params_mistyped":
            "strategy file {file:?} was handed `[strategy.params]` values of the wrong TYPE: {values}. A \
             params reader takes a value only when its TOML type matches, and IGNORES it \
             otherwise — exactly as it ignores an unknown key — so each of these would mount at \
             the strategy's COMPILED DEFAULT while the profile says otherwise. It reads: {reads}.",
        /// `[strategy.params]` keys naming a market the mount does not trade.
        ParamsMisrouted { file: String, keys: String, venue: String, symbol: String } =>
            "tradehub.profile.strategy.params_misrouted":
            "strategy file {file:?} was handed `[strategy.params]` keys naming a market this mount \
             does not trade: {keys}. A live mount trades its OWN (venue, symbol) only — \
             `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s \
             `resolve_intent_venue` stamps the mount's venue on every order and \
             `resolve_intent_symbol` REFUSES an order naming another symbol, because this \
             strategy declares no leg of its own (`vike_model::Strategy::symbols`, the one \
             source of `vike_tradehub_core::MountSpec`'s `legs`). So each \
             of these configures NOTHING while the mount line echoes it as real — the \
             operator reads one instrument while the orders go to another venue or are \
             refused. Set it to this mount's own \
             `venue = {venue:?}` / `symbol = {symbol:?}`, or drop the key.",
        /// A `.rhai` file that does not compile; `why` is `vike_rhai`'s words.
        RhaiCompileFailed { file: String, why: String } => "tradehub.profile.strategy.rhai_compile_failed":
            "rhai script {file} failed to compile: {why}",
        /// `[strategy.params]` overrides a `.rhai` file never asks for; `knobs` lists the ones it does.
        RhaiParamsUnasked { file: String, keys: String, knobs: String } =>
            "tradehub.profile.strategy.rhai_params_unasked":
            "rhai script {file} never asks for these `[strategy.params]` keys: {keys}. An \
             override applies only where the script itself calls `param(name, default)` at \
             top level, so each of these would configure NOTHING while the profile states a \
             number. The script's own knobs: {knobs}. (A `param()` call made only inside a hook \
             body is invisible to this check — bind it at top level, `const X = \
             param(\"x\", …);`, which is also what bakes the value in.)",
        /// A file whose header says `runs_on = "backtest"`; `why` is the header's own reason, ` (…)` or empty.
        BacktestOnly { file: String, why: String } => "tradehub.profile.strategy.backtest_only":
            "strategy file {file:?} runs in a backtest only{why}: its header says `runs_on = \
             \"backtest\"`, and this daemon does not link the simulator and could not mount it",
        /// A paper file on a daemon whose live gate is on.
        PaperOnlyOnLive { file: String } => "tradehub.profile.strategy.paper_only_on_live":
            "strategy file {file:?} cannot mount live: its header says `runs_on = \"paper\"` (a \
             header that says nothing is paper). A file trades on a live daemon only when its header \
             says `runs_on = \"live\"` and `why` it is safe there",
        /// A `.rs` file in a process started without the strategy builder.
        NoBuilder { file: String } => "tradehub.profile.strategy.no_builder":
            "strategy file {file}: a `.rs` strategy file runs in a sandboxed process of its own, built \
             by the strategy builder, and this process was started without one. Start the daemon as \
             `vike-backend tradehub`, or name a `.rhai` file",
        /// A `.rs` file whose strategies folder has no project above it to keep the build in.
        NoProjectForBuild { file: String, strategies: String } =>
            "tradehub.profile.strategy.no_project_for_build":
            "strategy file {file}: no project above {strategies} to keep its build in",
        /// A `.rs` file that does not run: a model run it reads could not be read, or its sandboxed process
        /// refused to open; `why` says which, under the child's own id when it sent one (`strategy.sandbox.*`).
        SandboxNotRunning { file: String, why: CarriedRefusal } => carries why,
            "tradehub.profile.strategy.not_running": "strategy file {file}: NOT RUNNING: {why}",
        /// A `.rs` file the compiler refused; `why` is the builder's refusal (`strategy.build.*`).
        BuildFailed { file: String, why: CarriedRefusal } => carries why,
            "tradehub.profile.strategy.build_failed": "strategy file {file}: NOT RUNNING: its build failed: {why}",
        /// A `.rs` file whose builder did not answer (asked again); `why` is the builder client's refusal.
        BuilderUnanswered { file: String, why: CarriedRefusal } => carries why,
            "tradehub.profile.strategy.builder_unanswered":
            "strategy file {file}: NOT RUNNING: building, and the strategy builder did not answer: {why}",
        /// A build of other text than the daemon read.
        BuiltOtherText { file: String, artifact: String, sha256: String } =>
            "tradehub.profile.strategy.built_other_text":
            "strategy file {file}: the builder built {artifact}, not the text this daemon read (sha256 {sha256})",
        /// A built artifact outside the builds directory.
        ArtifactOutsideBuilds { file: String, artifact: String, builds: String } =>
            "tradehub.profile.strategy.artifact_outside_builds":
            "strategy file {file}: the builder answered {artifact}, which is not under the builds \
             directory {builds}",
        /// This daemon's own program could not be read to start the sandboxed process.
        OwnProgramUnreadable { file: String, why: String } => "tradehub.profile.strategy.own_program_unreadable":
            "strategy file {file}: this daemon's own program cannot be read: {why}",
        /// A development tree's build on a live or demo daemon (decision 0296).
        DevelopmentSdkOnLive { file: String } => "tradehub.profile.strategy.development_sdk_on_live":
            "strategy file {file}: NOT RUNNING: it was built against the strategy builder's development \
             tree, not a numbered SDK, and a live or demo mount runs only a frozen SDK. Start the builder \
             on the installed SDKs (`vike-backend strategy-builder --sdk-root <project>/vendor/\
             vike-strategy-source`, as the deployed unit does) and restart the tradehub, or mount the file \
             on a paper daemon",
        /// A `.rs` file named by a RUNTIME (wire) mount.
        RsAtRuntime { file: String } => "tradehub.profile.strategy.rs_at_runtime":
            "strategy file {file}: a `.rs` strategy file mounts from the daemon's profile at startup (it is \
             built and started in a sandboxed process of its own, which a runtime mount would do on the \
             core's thread). Name it in the profile, or mount a `.rhai` file here",
    }
}
framed!(MountProfileRefusal);

// ===== end of section 4 =====

// ===== section 5: the session, settings, account and venue-read verbs (`crate::server::session`, `settings`,
// `accounts`, `venue_read`, `handshake`, `register`) =====

vike_utility::refusal_enum! {
    /// A request the node's authed session answers with a refusal before any verb's own source is asked
    /// (`crate::server::session`'s `serve_session`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SessionRefusal with ids SessionRefusalId {
        /// A well-framed body that is no request this build decodes; `why` is the decoder's words.
        UndecodableRequest { why: String } => "tradehub.session.undecodable_request":
            "unrecognized/undecodable request: {why}",
        /// A control command on a node that holds no command sink: control is authenticated but not enabled here.
        ControlNotEnabled => "tradehub.session.control_not_enabled": "control not enabled on this node",
        /// A `SettingsShow` on a node started without a settings source.
        SettingsUnavailable => "tradehub.session.settings_unavailable":
            "settings unavailable: this node was started without a settings source",
        /// A `Directory` on a node started without a settings source.
        DirectoryUnavailable => "tradehub.session.directory_unavailable":
            "directory unavailable: this node was started without a settings source",
        /// A `StrategyStatus` on a node that publishes no identity block.
        StrategyStatusUnavailable => "tradehub.session.strategy_status_unavailable":
            "strategy status unavailable: this node publishes no identity block",
    }
}
framed!(SessionRefusal);

vike_utility::refusal_enum! {
    /// A settings read or write the node refuses (`crate::server::settings::SettingsShowSource`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SettingsRefusal with ids SettingsRefusalId {
        /// The settings did not load; `why` is the loader's words.
        NotLoaded { why: String } => "tradehub.settings.not_loaded": "settings could not be loaded: {why}",
        /// A `Directory` on a node whose boot resolved no settings directory.
        DirectoryNoSettingsDir => "tradehub.settings.directory_no_settings_dir":
            "directory unavailable: this node resolved no settings directory at boot",
        /// A `Directory` on a node with no settings database (it answers no empty list in its place).
        DirectoryNoDatabase => "tradehub.settings.directory_no_database":
            "directory unavailable: no settings database on this node",
        /// A `Directory` over a settings database that exists and will not read (the store's own words, which name
        /// the file by path, go to the log).
        DirectoryUnreadable => "tradehub.settings.directory_unreadable":
            "directory unavailable: the settings database could not be read",
        /// A `SetSetting` on a node whose boot resolved no settings directory.
        WriteNoSettingsDir => "tradehub.settings.write_no_settings_dir":
            "settings write unavailable: this node resolved no settings directory at boot (no project above its \
             working directory)",
        /// A `SetSetting` the settings writer refused (`vike_config::write_setting_row`); `why` is its words, the
        /// same `vike-cli config set` prints.
        WriteRefused { why: String } => "tradehub.settings.write_refused": "{why}",
    }
}
framed!(SettingsRefusal);

vike_utility::refusal_enum! {
    /// An account-administration verb the node refuses (`crate::server::accounts`): its admission, its typed
    /// confirm, its values and the credential store's own answer. Nothing was written on any of them, and none
    /// quotes a credential value.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AccountVerbRefusal with ids AccountVerbRefusalId {
        /// The box declared no barrier, so this node holds no account writer at all.
        NotArmed => "tradehub.account.not_armed":
            "account administration is not armed on this node: it holds no account writer at all. It is armed by \
             DECLARING the barrier this wire's confidentiality comes from — the `config.tradehub_account_admin` \
             setting = \"loopback\" (checked against the bind) or \"contained\" (asserted by the operator). This \
             node advertised no `account-verbs` capability, which is how a conforming client knows without asking.",
        /// The writer exists and the peer is not on the admin scope.
        AdminScopeRequired => "tradehub.account.admin_scope_required":
            "account administration requires the ADMIN scope — a control device cannot reach key material on this \
             node. Only a device whose role is admin may: `vike-cli access ls` on the server shows each device's \
             role, and `vike-cli access role PERSON/DEVICE admin` there grants it.",
        /// A destructive verb sent without its typed confirm; `verb` is its word, `expected` the exact confirm.
        ConfirmMissing { verb: &'static str, expected: String } => "tradehub.account.confirm_missing":
            "`{verb}` changes something this node cannot put back — it must carry the typed confirm: re-send with \
             confirm set to the exact value `{expected}`",
        /// A destructive verb whose confirm is not the exact value; what was sent is not quoted back.
        ConfirmMismatch { expected: String } => "tradehub.account.confirm_mismatch":
            "account confirm mismatch: the confirm must equal the exact value `{expected}` — nothing was written. \
             (What was sent is deliberately not quoted back: on this verb the field beside it carries a \
             credential.)",
        /// An `Add` on a venue that is not on this node's roster; `roster` is the roster, joined.
        UnknownVenue { venue: String, roster: String } => "tradehub.account.unknown_venue":
            "unknown venue '{venue}' — nothing was written. The roster is: {roster}",
        /// An `Add` whose tier is not a tier word; `legal` is the tier words.
        UnknownTier { tier: String, legal: String } => "tradehub.account.unknown_tier":
            "unknown tier '{tier}' — nothing was written. It must be one of: {legal}. `paper` IS one of them: an \
             account at tier `paper` never trades.",
        /// A `List` on a node whose store has no account table; `why` is the store's answer.
        NoAccountTable { why: String } => "tradehub.account.no_account_table":
            "{why} — so this node has no account table to list. Move the box into the settings database first: \
             `vike-cli secrets init`.",
        /// A `SetCredential` on a name the wire may not set; `providers` is the data-provider key names, joined.
        CredentialNameNotSettable { key: String, providers: String } =>
            "tradehub.account.credential_name_not_settable":
            "'{key}' is not a name this node's wire may set, so nothing was written. The wire writes a DEPLOYMENT \
             name only — a data-provider key ({providers}) or a venue's attribution code. An account's credential \
             belongs to its ACCOUNT and is written to an EXISTING account by FIELD, never by a name (decision 0156 \
             item 6): create the account with its tier first (`vike-cli account add --venue <VENUE> --tier \
             <paper|demo|live>`), then write the field to it (`vike-cli secrets set --id <ID> <FIELD>`, or the \
             desktop's Connections panel). A typo'd name would sit in the store forever with nothing looking for \
             it.",
        /// A `SetCredential` on a node with no credential store, which this verb never creates; `remedy` is the
        /// store's creation remedy.
        NoCredentialStore { remedy: &'static str } => "tradehub.account.no_credential_store":
            "this node has no credential store at all, and nothing here will create one: a store is an operator's \
             only copy of their live venue keys. On the box, {remedy}, then retry.",
        /// A `RetireCredential` on a name that is neither a settable key nor one the store holds for an account.
        RetireUnknownName { key: String } => "tradehub.account.retire_unknown_name":
            "'{key}' is not a credential key this workspace reads, nor one this store holds for an account, so \
             nothing was changed. `vike-cli config show` lists the names this workspace reads.",
        /// A `RetireCredential` on a node with no credential store; `remedy` is the store's creation remedy.
        RetireNoCredentialStore { remedy: &'static str } => "tradehub.account.retire_no_credential_store":
            "this node has no credential store at all, so there is nothing to remove a credential from and nothing \
             here will create one. On the box, {remedy}.",
        /// A `RetireCredential` of a hub key, which is never deleted by name.
        RetireHubKey { key: String } => "tradehub.account.retire_hub_key":
            "refused: {key} is a hub key, and a hub key is not deleted by name — every client of that service signs \
             with the pair. Nothing was changed.",
        /// A `RetireCredential` of a name the store does not hold live.
        RetireNotHeld { key: String } => "tradehub.account.retire_not_held":
            "refused: {key} is not set in this node's credential store, so there is nothing to remove. Nothing was \
             changed.",
        /// A `RetireCredential` of one spelling while another still answers for the same account field; `others`
        /// is those spellings, joined, and `which` names them ("that name" or "those names").
        RetireLeavesASpelling { key: String, others: String, which: &'static str } =>
            "tradehub.account.retire_leaves_a_spelling":
            "refused: {key} is one spelling of a credential that {others} also answer(s) for at the same account \
             field — removing {key} alone would leave the account armed with it. Remove {which} first (one name per \
             call). Nothing was changed.",
        /// A `HeldCredentials` lookup of more names than one screen shows.
        LookupTooMany { max: usize, asked: usize } => "tradehub.account.lookup_too_many":
            "at most {max} names per lookup (this one asked {asked}) — the presence read answers for the few names \
             a screen shows, not for the store",
        /// A `HeldCredentials` lookup of a name that is no credential key.
        LookupUnknownName { name: String } => "tradehub.account.lookup_unknown_name":
            "'{name}' is not a credential key this workspace reads, so nothing was looked up.",
        /// The credential store refused or could not answer; `why` is its words, safe to print by construction (a
        /// key NAME, an id, a reason, never a value).
        StoreRefused { why: String } => "tradehub.account.store_refused": "{why}",
    }
}
framed!(AccountVerbRefusal);

vike_utility::refusal_enum! {
    /// An operator's venue read the node refuses before or after it asks the account's lease
    /// (`crate::server::venue_read`). Nothing was asked of the venue on the first three.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum VenueReadRefusal with ids VenueReadRefusalId {
        /// A venue that is not on the roster; `venue` is the peer's, bounded, and `roster` the roster, joined.
        UnknownVenue { venue: String, roster: String } => "tradehub.venue_read.unknown_venue":
            "`{venue}` is not a venue of this node's roster ({roster}); nothing was asked",
        /// An account word that is no account name; `account` is the peer's, bounded, `why` the parser's words.
        BadAccount { account: String, why: String } => "tradehub.venue_read.bad_account":
            "`{account}` is not an account name: {why}; nothing was asked",
        /// A node with no trade lease registry: it runs behind no gatewayhub, so no path reaches the venue.
        NoLeaseRegistry { route_key: String } => "tradehub.venue_read.no_lease_registry":
            "{route_key}: this node holds no trade lease registry (it is not running behind a gatewayhub), so \
             there is no path to the venue to read; nothing was asked",
        /// The venue's answer carried a figure that is not a number (it would write as `null` and end the
        /// connection that decodes it); `kind` is the read's word.
        NotANumber { route_key: String, kind: &'static str } => "tradehub.venue_read.not_a_number":
            "{route_key}: the venue's {kind} answer carried a figure that is not a number, so it is not passed on",
    }
}
framed!(VenueReadRefusal);

// ===== end of section 5 =====

// ===== section 6: the NOT RUNNING rows the node words itself (`crate::account_follow`, `crate::publish`) =====

vike_utility::refusal_enum! {
    /// Why an account is listed NOT RUNNING in the node's OWN words, for a cause no `vike_model::ArmingBlock` names
    /// (a row an arming block stopped reads the block's badge, `ArmingBlock::as_str`, and carries its word on the
    /// wire). The row's `reason` is this text (`vike_execution::ArmingNotRunningView::reason`) and its `refusal_id`
    /// this id, which every reader shows with it once (`vike_tradehub_client::wire::ArmingNotRunningViewWire::say`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NotRunningReason with ids NotRunningReasonId {
        /// An active account the running node cannot start: only a restart of the tradehub starts it
        /// (`crate::account_follow`'s `HotAccountStart::RestartRequired`).
        RestartRequired => "tradehub.not_running.restart_required": "restart required to start",
        /// Stopped and started again to follow the currency the venue now states (`crate::account_follow`'s
        /// `AccountFollowWatcher::remount_asked`): about to run again.
        Remount => "tradehub.not_running.remount":
            "restarting to follow the account's currency, which the venue now states",
        /// Stopped to start again at the tier its row now states.
        TierChanged { to: vike_model::AccountTier } => "tradehub.not_running.tier_changed":
            "tier changed, restarting at `{to}`",
        /// A lane twin whose default account has a demo row and a live row active.
        PerpLaneTwoTiers { venue: String } => "tradehub.not_running.perp_lane.two_tiers":
            "the perp lane is not running: the {venue} default account has two active tiers (a demo \
             row and a live row): deactivate the one you do not mean",
        /// A lane twin whose account states no leverage (`tier` is its tier's word, or `demo or live`).
        PerpLaneNoLeverage { venue: String, tier: &'static str } => "tradehub.not_running.perp_lane.no_leverage":
            "the perp lane is not running: state a leverage on the {venue} {tier} account to start it",
        /// Any of the above (or a block's badge), with each position the account still holds at the venue: a stop
        /// does not close a position and must not hide one (decision 0169 item 5).
        PositionOpen { reason: String, held: String } => "tradehub.not_running.position_open":
            "{reason}; position open at venue: {held}",
        /// A row whose key the journal restored holdings for that this boot runs no engine under
        /// (`crate::readmit`): nothing manages them.
        RestoredUnmanaged { reason: String, held: String } => "tradehub.not_running.restored_unmanaged":
            "{reason}; restored state is NOT managed: {held}",
        /// The journal restored holdings under a key no account runs at all.
        RestoredNoAccount { held: String } => "tradehub.not_running.restored_no_account":
            "no account runs under this key; restored state is NOT managed: {held}",
    }
}

// ===== end of section 6 =====

#[path = "refusals_tests.rs"]
#[cfg(test)]
mod refusals_tests;
