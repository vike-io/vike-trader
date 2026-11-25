//! alpaca's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String` (the symbol facts' `Option<String>`
//! answers, `vike_catalog::CatalogError`, the data-client spec, `venue_read`, the reconcile client, the startup credential
//! probe, the shared pump's open outcome and close reason): its `Display`, byte for byte the words it had before it was
//! typed. An order's refusal is step 2's `RejectCause`, and the words of the shared transport (`ALPACA_HTTP`) and of
//! the kept list (`vike_catalog`) are theirs: neither is here.

use crate::names::ADD_A_ROW;
use crate::reconcile::{FILL_PAGE_CAP, FILL_PAGE_SIZE, ORDER_PAGE_CAP, ORDER_PAGE_LIMIT};

vike_utility::refusal_enum! {
    /// Why a USER name has no alpaca spelling (decision 0290, `crate::names::wire_of`): nothing is sent, read or dialled
    /// for it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaNameRefusal with ids AlpacaNameRefusalId {
        /// No list is kept, no kept row answers the name, or two do; `why` is the kept list's words.
        NotKept { why: String } => "alpaca.name.not_kept": "alpaca: {why}; {ADD_A_ROW}",
        /// The kept row states no user name; `why` is the catalog's words (`vike_catalog::names_of_row`).
        RowUnnamed { why: String } => "alpaca.name.row_unnamed": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecAlpaca`, decision 0173).
    #[derive(Debug, Clone, PartialEq)]
    pub enum AlpacaPropertiesRefusal with ids AlpacaPropertiesRefusalId {
        /// The venue has no tradable asset of that name.
        UnknownAsset { symbol: String } => "alpaca.properties.unknown_asset":
            "alpaca: unknown asset {symbol}: the venue has no tradable US equity or crypto pair of that name",
        /// A price grid that is not whole cents: the adapter writes a price to the cent only.
        NotWholeCents { symbol: String, tick: f64 } => "alpaca.properties.not_whole_cents":
            "alpaca: {symbol} has a price increment of {tick}, and this adapter writes a price to the cent only, so \
             an instrument whose price grid is not whole cents is not admitted",
    }
}

vike_utility::refusal_enum! {
    /// Why the credentialed asset read (`crate::catalog::CatalogProviderAlpaca`) answers no rows, carried as
    /// `vike_catalog::CatalogError`'s text.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaCatalogRefusal with ids AlpacaCatalogRefusalId {
        /// The account holds no complete trio: the fields are named, never a value.
        NoCredentials => "alpaca.catalog.no_credentials":
            "alpaca: this account holds no complete sandbox credentials (CLIENT_ID, CLIENT_SECRET and ACCOUNT_ID)",
        /// The asset read failed; `path` is the read's, `why` the transport's words.
        Unread { path: String, why: String } => "alpaca.catalog.unread": "alpaca {path}: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the market-data feed refuses a subscription before any lane spawns, or ends a socket
    /// (`crate::feed::client::DataClientSpecAlpaca`, `crate::feed::connection`'s `RouteLink`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaFeedRefusal with ids AlpacaFeedRefusalId {
        /// A bars interval other than one minute: the socket serves fixed 1-minute bars.
        BarsOnlyOneMinute => "alpaca.feed.bars_only_one_minute": "alpaca market-data WS serves 1m bars only",
        /// A lane alpaca streams nothing for (book, depth, mark); `lane` is its word.
        NoLane { lane: String } => "alpaca.feed.no_lane": "alpaca serves no {lane} lane",
        /// The kept row's class has no stream here.
        ClassNotStreamed => "alpaca.feed.class_not_streamed": "alpaca streams no market data for this asset class",
        /// The socket was silent past the pump row's idle bound, so its reader ended it.
        SilentStall { secs: u64 } => "alpaca.feed.silent_stall": "no frames within {secs}s (silent stall)",
    }
}

vike_utility::refusal_enum! {
    /// Why the exec event lane's SSE open did not stream (`crate::execution::user_data`'s `open_trade_events`): the
    /// shared pump's refusal or transport failure, which it waits out and reopens.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaStreamRefusal with ids AlpacaStreamRefusalId {
        /// A 401: the token was refreshed, and the next open uses it.
        TokenRefreshed => "alpaca.stream.token_refreshed": "401, the token was refreshed",
        /// A 3xx, 4xx or 5xx answer.
        NonSuccess { status: u16 } => "alpaca.stream.non_success": "non-2xx response {status}",
        /// The dial failed; `why` is ureq's words, which name the host and the cause, never a header.
        ConnectFailed { why: String } => "alpaca.stream.connect_failed": "connect failed: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why the startup credential probe failed (`crate::adapter`'s `credential_probe`): the words an operator reads in
    /// the account's start log.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaStartRefusal with ids AlpacaStartRefusalId {
        /// The balance read failed; `why` is its words, and both hosts are named because either can be the one that
        /// is unreachable.
        ProbeFailed { why: String, authx: String, broker: String } => "alpaca.start.probe_failed":
            "{why} (OAuth2 token mint host {authx}, balance read host {broker}; credentials: the account's \
             CLIENT_ID/CLIENT_SECRET/ACCOUNT_ID)",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's read of the account was not made (`crate::adapter`'s `venue_read`,
    /// `crate::reconcile::venue_read`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaVenueReadRefusal with ids AlpacaVenueReadRefusalId {
        /// A sandbox host row was refused; `why` is the refusal's words.
        HostRefused { why: String } => "alpaca.venue_read.host_refused": "alpaca: {why}; nothing was asked",
        /// A `live` account: this bridge reaches only the sandbox.
        LiveTier => "alpaca.venue_read.live_tier":
            "this bridge reaches only Alpaca's sandbox, so an account on the live tier has no host to read; nothing \
             was asked",
        /// The account holds no complete trio.
        NoTrio => "alpaca.venue_read.no_trio":
            "the account holds no alpaca CLIENT_ID, CLIENT_SECRET and ACCOUNT_ID trio; nothing was asked",
        /// The open-orders read failed; `why` is the transport's words.
        OrdersUnread { why: String } => "alpaca.venue_read.orders_unread":
            "alpaca: the open orders could not be read: {why}",
        /// The answer is not a JSON list.
        NotAList => "alpaca.venue_read.not_a_list": "alpaca's orders answer is not a list",
    }
}

vike_utility::refusal_enum! {
    /// Why a reconcile pass's paged read reports nothing (`crate::reconcile`'s `read_order_pages`, `read_fill_pages`):
    /// never a short list read as complete.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AlpacaReconcileRefusal with ids AlpacaReconcileRefusalId {
        /// A page fetch failed; `why` is the fetch's words, unchanged.
        PageUnread { why: String } => "alpaca.reconcile.page_unread": "{why}",
        /// A page that is no row list; `why` is the shared reader's words (`vike_bridge_core::json::parse_rows`).
        PageNotRows { why: String } => "alpaca.reconcile.page_not_rows": "{why}",
        /// A full order page whose last row has no `submitted_at`: the read cannot page on.
        OrderPageNoCursor => "alpaca.reconcile.order_page_no_cursor":
            "alpaca order read: a full page's last row has no `submitted_at`, so the read cannot page on",
        /// Still a full order page after the page cap.
        OrderPagesCapped => "alpaca.reconcile.order_pages_capped":
            "alpaca order read: still a full page after {ORDER_PAGE_CAP} pages of {ORDER_PAGE_LIMIT} (the page \
             cap); refusing to report a short list",
        /// A full fill page whose last activity has no `id`: the read cannot page on.
        FillPageNoCursor => "alpaca.reconcile.fill_page_no_cursor":
            "alpaca fill read: a full page's last activity has no `id`, so the read cannot page on",
        /// Still a full fill page after the page cap.
        FillPagesCapped => "alpaca.reconcile.fill_pages_capped":
            "alpaca fill read: still a full page after {FILL_PAGE_CAP} pages of {FILL_PAGE_SIZE} (the page cap); \
             refusing to report a short list",
    }
}
