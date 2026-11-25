//! polymarket's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE
//! text, and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the seam carries a `String`
//! (`vike_bridge_core::symbol_properties::SymbolPropertiesError`, [`crate::catalog::market_ref::MarketNameRefusal`],
//! the adapter's `venue_read`, the reconcile client's reads, `vike_data::source::SourceError`, a `VenueApiError`'s
//! `msg`): its `Display`, byte for byte the words it had before it was typed. The id does not cross that seam yet.

vike_utility::refusal_enum! {
    /// Why a symbol the gatewayhub asks this venue's facts of is not admitted
    /// (`crate::symbol_properties::SymbolPropertiesListSpecPolymarket`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketPropertiesRefusal with ids PolymarketPropertiesRefusalId {
        /// The token's market is closed, inactive or paused: an order on it only reaches the venue's own rejection.
        /// `description` is the row's `"{question} — {outcome}"`.
        NotAcceptingOrders { symbol: String, description: String } =>
            "polymarket.properties.not_accepting_orders":
            "polymarket: the market of `{symbol}` («{description}») takes no orders right now (Gamma lists it \
             closed, inactive or with acceptingOrders false)",
    }
}

vike_utility::refusal_enum! {
    /// Why what an operator TYPED as a Polymarket symbol names no tradeable outcome token
    /// (`crate::catalog::market_ref`'s `parse_market_ref` and `resolve_market_ref`). Every refusal of a spelling says
    /// the spellings that would work.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketMarketRefRefusal with ids PolymarketMarketRefRefusalId {
        /// Nothing was typed.
        Empty => "polymarket.market_ref.empty":
            "polymarket: no market was named; give an outcome token id (a 20+ digit number), a market slug with \
             its outcome (`will-it-happen:Yes`), or the market's polymarket.com address",
        /// A condition id names a market, not one of its outcomes.
        ConditionId { typed: String } => "polymarket.market_ref.condition_id":
            "polymarket: `{typed}` is a condition id, which names a MARKET and not an outcome; give an outcome \
             token id (a 20+ digit number), a market slug with its outcome (`will-it-happen:Yes`), or the market's \
             polymarket.com address",
        /// No spelling this venue reads.
        NotAName { typed: String } => "polymarket.market_ref.not_a_name":
            "polymarket: `{typed}` is not a name this venue's orders take; give an outcome token id (a 20+ digit \
             number), a market slug with its outcome (`will-it-happen:Yes`), or the market's polymarket.com address",
        /// An address on another host.
        NotPolymarketAddress { url: String } => "polymarket.market_ref.not_polymarket_address":
            "polymarket: `{url}` is not a polymarket.com address; give an outcome token id (a 20+ digit number), a \
             market slug with its outcome (`will-it-happen:Yes`), or the market's polymarket.com address",
        /// A polymarket.com address that is no market or event page.
        NotAMarketPage { url: String } => "polymarket.market_ref.not_a_market_page":
            "polymarket: `{url}` is not a market or event page; the address of one market looks like \
             https://polymarket.com/event/<event>/<market>",
        /// A market or event page whose slug segment is no slug.
        NotASlugSegment { url: String } => "polymarket.market_ref.not_a_slug_segment":
            "polymarket: `{url}` has a segment that is not a slug; the address of one market looks like \
             https://polymarket.com/event/<event>/<market>",
        /// Gamma lists no market under the slug.
        MarketNotListed { slug: String } => "polymarket.market_ref.market_not_listed":
            "polymarket: Gamma lists no market with the slug `{slug}`; check the spelling (a slug is the last part \
             of the market's polymarket.com address), or give an outcome token id (a 20+ digit number), a market \
             slug with its outcome (`will-it-happen:Yes`), or the market's polymarket.com address",
        /// Gamma lists no event under the slug.
        EventNotListed { slug: String } => "polymarket.market_ref.event_not_listed":
            "polymarket: Gamma lists no event `{slug}`; check the address, or give an outcome token id (a 20+ digit \
             number), a market slug with its outcome (`will-it-happen:Yes`), or the market's polymarket.com address",
        /// Every market of the event is closed or inactive.
        EventAllClosed { slug: String } => "polymarket.market_ref.event_all_closed":
            "polymarket: every market of the event `{slug}` is closed or inactive, so nothing in it can be traded",
        /// The event holds several open markets: which one is a money decision no default makes. `listed` is the
        /// first ones, spelled; `more` the count beyond them (`, and 2 more`), or empty.
        EventHoldsMany { slug: String, count: usize, listed: String, more: String } =>
            "polymarket.market_ref.event_holds_many":
            "polymarket: the event `{slug}` holds {count} open markets, so name one market (then its outcome, \
             `<market>:Yes`): {listed}{more}",
        /// The market is resolved.
        MarketClosed { slug: String } => "polymarket.market_ref.market_closed":
            "polymarket: the market `{slug}` is CLOSED (resolved), so none of its tokens can be traded any more",
        /// The market is not active on Gamma.
        MarketInactive { slug: String } => "polymarket.market_ref.market_inactive":
            "polymarket: the market `{slug}` is not active on Gamma right now, so its tokens cannot be traded",
        /// The market is open and paused (`acceptingOrders` false).
        MarketNotAcceptingOrders { slug: String } => "polymarket.market_ref.market_not_accepting_orders":
            "polymarket: the market `{slug}` is open but Gamma says it is not accepting orders right now \
             (`acceptingOrders` is false), so an order on its tokens would be refused",
        /// Gamma's outcomes and token ids cannot be paired one to one.
        OutcomesUnmatched { outcomes: usize, tokens: usize, slug: String } =>
            "polymarket.market_ref.outcomes_unmatched":
            "polymarket: Gamma lists {outcomes} outcomes and {tokens} token ids for `{slug}`, which cannot be \
             matched one to one, so no outcome is named",
        /// A market was named without its outcome: which side to buy is a money decision no default makes.
        /// `listed` is the outcomes, spelled; `more` the count beyond them, or empty.
        ChooseOutcome { slug: String, question: String, count: usize, listed: String, more: String } =>
            "polymarket.market_ref.choose_outcome":
            "polymarket: the market `{slug}` («{question}») has {count} outcomes and an order buys or sells ONE of \
             them, so name which: {listed}{more}",
        /// The market has no outcome of the label typed; `choose` names the ones it has.
        NoSuchOutcome { slug: String, want: String, choose: Box<PolymarketMarketRefRefusal> } =>
            "polymarket.market_ref.no_such_outcome":
            "polymarket: the market `{slug}` has no outcome called `{want}`. {choose}",
        /// A Gamma READ failed (no answer, or one that did not parse): it says nothing about the name, so the
        /// caller asks again. `why` is the read's own words.
        GammaUnread { why: String } => "polymarket.market_ref.gamma_unread": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// What a live exec start says about the venue's region pre-flight (`crate::adapter`'s `geoblock_action` and
    /// `geoblock_preflight`, carried by its `GeoblockAction`). A `Refuse` is the account NOT RUNNING in these words;
    /// the two the start goes on after (`GeoblockUnread`, `GeoblockSkipped`) are its warnings.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketStartRefusal with ids PolymarketStartRefusalId {
        /// The venue's own geoblock endpoint refuses ORDER PLACEMENT from this egress; `geoblock` is where it
        /// refuses from. Reads are unaffected.
        GeoblockBlocked { geoblock: crate::transport::Geoblock } => "polymarket.start.geoblock_blocked":
            "polymarket exec REFUSED: the venue's own geoblock endpoint says ORDER PLACEMENT is blocked from this \
             egress — {geoblock}. Market data and AUTHENTICATED READS (balance, positions, orders, fills) are NOT \
             affected; only order submission is, and it would have failed one order at a time with a 403 on the \
             exec path. Egress from a permitted region (`venue.polymarket.proxy_enabled = true` with `proxy_host` / \
             `proxy_port`, or `socks_proxy`), or set POLY_GEOBLOCK_OVERRIDE=1 in the credential store to mount \
             anyway.",
        /// The geoblock address row was refused; `why` is the row's refusal.
        GeoblockRowRefused { why: String } => "polymarket.start.geoblock_row_refused":
            "polymarket exec is NOT RUNNING: {why}",
        /// The pre-flight could not be reached or read: evidence of nothing, so the start goes on.
        GeoblockUnread { why: String } => "polymarket.start.geoblock_unread":
            "polymarket: the geoblock pre-flight could not be reached or read ({why}) — mounting ANYWAY. An \
             unreachable probe is not evidence of a block, so this is deliberately not a refusal; if this region \
             is in fact restricted, the first order will 403 instead.",
        /// The operator's override skipped the pre-flight, and the start says so.
        GeoblockSkipped => "polymarket.start.geoblock_skipped":
            "polymarket: POLY_GEOBLOCK_OVERRIDE=1 — the venue geoblock pre-flight is SKIPPED, so this mount is NOT \
             checked against the venue's region policy. A restricted region refuses order placement per order, \
             with a 403 at submit.",
    }
}

vike_utility::refusal_enum! {
    /// Why one of the venue's READS has no answer (the data-api's `/v2/positions`, the CLOB's signed GETs): the
    /// read failed, or its answer is not the shape that can be believed. `path` is the venue path read. A read
    /// that fails says its status head, never the body.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketReadRefusal with ids PolymarketReadRefusalId {
        /// The request did not complete; `why` is the transport's words.
        Network { path: String, why: String } => "polymarket.read.network":
            "polymarket {path} could not be read: network: {why}",
        /// The venue answered a status that is no answer.
        HttpStatus { path: String, status: u16 } => "polymarket.read.http_status":
            "polymarket {path} could not be read: HTTP {status}",
        /// A signed read failed; `why` is its status head (`crate::account::preflight`'s `loggable`).
        Unread { path: String, why: String } => "polymarket.read.unread":
            "polymarket {path} could not be read: {why}",
        /// HTTP 410: the venue took the path away for good, which is not an empty answer and not a retry. It holds
        /// no `:`, so `crate::account::preflight`'s `loggable` keeps the whole sentence.
        Retired { path: String } => "polymarket.read.retired":
            "polymarket retired {path} (HTTP 410), so the answer is unknown and not empty; the bridge must move to \
             the venue's successor",
        /// The answer is not JSON.
        NotJson { path: String, status: u16 } => "polymarket.read.not_json":
            "polymarket's {path} answer (HTTP {status}) is not JSON",
        /// The answer is not the list it must be.
        NotAList { path: String } => "polymarket.read.not_a_list": "polymarket's {path} answer is not a list",
        /// The answer is not the object it must be.
        NotAnObject { path: String } => "polymarket.read.not_an_object":
            "polymarket's {path} answer is not an object",
        /// A balance answer with no balance figure: unknown, never a zero.
        NoBalanceFigure { path: String } => "polymarket.read.no_balance_figure":
            "polymarket's {path} answer carries no balance figure",
        /// A positions page with no pagination cannot be read as the whole wallet.
        NoPagination { path: String } => "polymarket.read.no_pagination":
            "polymarket's {path} answer carries no pagination, so it cannot be read as the whole wallet",
        /// A page that says more rows follow and names no cursor.
        MoreWithoutCursor { path: String } => "polymarket.read.more_without_cursor":
            "polymarket's {path} answer says more rows follow and names no cursor",
        /// A row whose holding cannot be read: a guess would report a held position as flat.
        UnreadableSize { path: String, asset: String } => "polymarket.read.unreadable_size":
            "polymarket's {path} row for token {asset} states no readable current_size, so this wallet's holding \
             is unknown",
        /// The cursor walk did not end within its page bound.
        WalkUnended { path: String, pages: usize } => "polymarket.read.walk_unended":
            "polymarket's {path} did not end within {pages} pages, so this wallet's holdings are unknown",
        /// The funder goes into a query string, so only a plain `0x` address is asked for.
        FunderNotAnAddress => "polymarket.read.funder_not_an_address":
            "the account's funder is not a 0x address, so its positions cannot be asked for",
    }
}

vike_utility::refusal_enum! {
    /// Why an operator's READ of the account (`crate::reconcile::venue_read`) has no answer: the sentence the
    /// gatewayhub hands back for `TradeUp::VenueRead`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketVenueReadRefusal with ids PolymarketVenueReadRefusalId {
        /// The account holds no private key.
        NoPrivateKey => "polymarket.venue_read.no_private_key": "the account holds no polymarket private key",
        /// The private key is not a usable key; `why` is the key reader's words.
        UnusableKey { why: String } => "polymarket.venue_read.unusable_key":
            "the account's PRIVATE_KEY is not a usable key ({why})",
        /// An address row the read dials was refused; `why` is the row's refusal.
        RowRefused { why: String } => "polymarket.venue_read.row_refused": "{why}",
        /// The wallet's L2 API key could not be derived.
        NoL2Key { why: String } => "polymarket.venue_read.no_l2_key":
            "could not obtain the wallet's L2 API key: {why}",
        /// The signed CLOB seam was asked for the positions, which are the public data-api's.
        PositionsNotSigned => "polymarket.venue_read.positions_not_signed":
            "polymarket's positions are read from the public data-api, not the signed CLOB",
        /// The venue's read failed or answered no believable shape.
        Read { why: PolymarketReadRefusal } => "polymarket.venue_read.read": "{why}",
    }
}

impl From<PolymarketReadRefusal> for PolymarketVenueReadRefusal {
    fn from(why: PolymarketReadRefusal) -> Self {
        PolymarketVenueReadRefusal::Read { why }
    }
}

vike_utility::refusal_enum! {
    /// Why a settlement WRITE (a redeem, a split, a merge, a convert, the trading approvals: one relayer batch) was
    /// not built or not taken, and why the chain says a submitted redemption did not happen (`crate::settlement`'s
    /// `redeem`, `redeem_relayer`, `split_merge` and `redeem_confirm`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketRedeemRefusal with ids PolymarketRedeemRefusalId {
        /// An on-chain word or address that is not hex; `field` names it (`conditionId`, `negRiskMarketID`,
        /// `proxy_addr`, `call target`).
        NotHex { field: &'static str, why: String } => "polymarket.redeem.not_hex": "{field} not valid hex: {why}",
        /// A bytes32 that is not exactly 32 bytes: never truncated or zero-padded.
        NotThirtyTwoBytes { field: &'static str, bytes: usize } => "polymarket.redeem.not_thirty_two_bytes":
            "{field} must be 32 bytes, got {bytes}",
        /// An address that is not exactly 20 bytes: never signed as the zero address.
        NotAnAddress { field: &'static str, bytes: usize } => "polymarket.redeem.not_an_address":
            "{field} must be a 20-byte address, got {bytes} bytes",
        /// A relayer batch with no call.
        EmptyBatch => "polymarket.redeem.empty_batch": "a relayer batch needs at least one call",
        /// A `convertPositions` that converts nothing.
        EmptyIndexSet => "polymarket.redeem.empty_index_set": "convertPositions index set must not be empty",
        /// An outcome index past the encoder's 128-bit set: never wrapped into another outcome.
        IndexOutOfRange { index: u32 } => "polymarket.redeem.index_out_of_range":
            "outcome index {index} exceeds this encoder's 128-bit index set",
        /// The key could not derive its address or sign; `why` is the signer's words.
        Signing { why: String } => "polymarket.redeem.signing": "{why}",
        /// The request body could not be encoded.
        Encode { why: String } => "polymarket.redeem.encode": "{why}",
        /// The system clock reads before the epoch, so no deadline can be stated.
        Clock { why: String } => "polymarket.redeem.clock": "{why}",
        /// The account holds no relayer key pair.
        RelayerCredentialsUnset => "polymarket.redeem.relayer_credentials_unset":
            "relayer credentials unset (RELAYER_API_KEY / RELAYER_API_KEY_ADDRESS)",
        /// A relayer request did not complete; `step` is which (`nonce`, `submit`).
        Network { step: &'static str, why: String } => "polymarket.redeem.network": "network ({step}): {why}",
        /// The nonce answer is not JSON; `summary` is the body's, never the body.
        NonceBadJson { why: String, summary: String } => "polymarket.redeem.nonce_bad_json":
            "bad nonce json: {why}: {summary}",
        /// The nonce answer names a nonce that is no number.
        NonceNotANumber { why: String } => "polymarket.redeem.nonce_not_a_number": "nonce not a number: {why}",
        /// The nonce answer names no nonce; `summary` is the body's, never the body.
        NoNonce { summary: String } => "polymarket.redeem.no_nonce": "no nonce in relayer response: {summary}",
        /// The relayer refused the batch; `words` is its summary, with what the request carried redacted.
        RelayerStatus { status: u16, words: String } => "polymarket.redeem.relayer_status":
            "relayer {status}: {words}",
        /// The redemption's receipt states a failed transaction.
        ReceiptStatus { status: u64 } => "polymarket.redeem.receipt_status": "receipt status 0x{status:x}",
        /// The redemption was mined, and redeemed nothing of this condition.
        NoPayoutRedemption => "polymarket.redeem.no_payout_redemption":
            "mined, but no PayoutRedemption for this conditionId in the receipt",
    }
}

vike_utility::refusal_enum! {
    /// Why one JSON-RPC round trip to the Polygon endpoint the settlement cluster reads has no result
    /// (`crate::settlement::chain`'s `PolygonRpc::call`). `method` is the JSON-RPC method; no text names the
    /// endpoint, which may carry the provider's API key.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketChainRefusal with ids PolymarketChainRefusalId {
        /// The request was not sent or not answered; `why` is the transport's words without the URL.
        RpcNetwork { method: String, why: String } => "polymarket.chain.rpc_network":
            "rpc {method}: network: {why}",
        /// The answer's body could not be read.
        RpcBody { method: String, why: String } => "polymarket.chain.rpc_body": "rpc {method}: body: {why}",
        /// The answer is not JSON.
        RpcBadJson { method: String, why: String } => "polymarket.chain.rpc_bad_json":
            "rpc {method}: bad json: {why}",
        /// The endpoint answered a JSON-RPC `error` member, never read as "no events".
        RpcError { method: String, error: String } => "polymarket.chain.rpc_error": "rpc {method}: {error}",
        /// The answer carries no `result` member.
        RpcNoResult { method: String } => "polymarket.chain.rpc_no_result": "rpc {method}: no result member",
    }
}

vike_utility::refusal_enum! {
    /// Why a read of markets' RULES (`crate::catalog::rules`'s `MarketRulesSourcePolymarket`, the rules the
    /// gatewayhub serves the datahub) answers no rows: a request refused before anything was asked, or a closed
    /// listing that could not be read whole (never a short one).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketRulesRefusal with ids PolymarketRulesRefusalId {
        /// The archive window ends before it starts.
        WindowInverted { start_ms: i64, end_ms: i64 } => "polymarket.rules.window_inverted":
            "polymarket rules: window [{start_ms}, {end_ms}] is inverted; nothing was asked",
        /// A series window ends before it starts.
        SeriesWindowInverted { series: String, start_ms: i64, end_ms: i64 } =>
            "polymarket.rules.series_window_inverted":
            "polymarket rules {series}: window [{start_ms}, {end_ms}] is inverted; nothing was asked",
        /// The live read names no outcome token.
        NotAToken { token: String } => "polymarket.rules.not_a_token":
            "polymarket rules: `{token}` is not an outcome token id; nothing was asked",
        /// The closed listing was still going at the page ceiling.
        ListingPastCeiling { pages: usize } => "polymarket.rules.listing_past_ceiling":
            "gamma still listed closed markets after {pages} pages for one window; nothing of it was kept (a \
             narrower window, or a filter Gamma ignored)",
        /// Gamma named a page's own cursor as the next one.
        CursorRepeated { page: usize } => "polymarket.rules.cursor_repeated":
            "gamma named the cursor of closed-market page {page} as its next page; nothing of the window was kept",
        /// A page of the listing could not be read; `why` is the read's words.
        PageUnread { why: String } => "polymarket.rules.page_unread": "{why}",
    }
}

vike_utility::refusal_enum! {
    /// Why a wallet has no L2 API key from the CLOB's credential endpoints (`crate::transport::sign::l1`): the words
    /// a start that does NOT RUN logs, and an operator's read answers. The body is never quoted (it IS the
    /// credential trio): the status, WHICH fields were absent and, for a body holding no credential field, the
    /// venue's own sentence. `what` is the endpoint (`derive-api-key`, `api-key (create)`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketL2Refusal with ids PolymarketL2RefusalId {
        /// The answer is not JSON (a proxy's or a CDN's error page, which can echo the request).
        NotJson { what: &'static str, status: u16, bytes: usize, why: String } => "polymarket.l2.not_json":
            "{what} HTTP {status}: body is not JSON ({bytes} bytes, not shown): {why}",
        /// The answer lacks `missing` (the absent fields, `+`-joined).
        MissingFields { what: &'static str, status: u16, missing: String, bytes: usize } =>
            "polymarket.l2.missing_fields":
            "{what} HTTP {status}: response is missing {missing} ({bytes} bytes, not shown — the body carries \
             whichever credential fields ARE present)",
        /// The same, for a plain `{"error": …}` answer, quoting the venue's `sentence`.
        MissingFieldsVenueSays {
            what: &'static str,
            status: u16,
            missing: String,
            bytes: usize,
            sentence: String,
        } => "polymarket.l2.missing_fields_venue_says":
            "{what} HTTP {status}: response is missing {missing} ({bytes} bytes, not shown — the body carries \
             whichever credential fields ARE present) — the venue says: \"{sentence}\"",
        /// The derive said the wallet has no key, and the create that followed failed; `why` is the create's words.
        CreateFailed { why: String } => "polymarket.l2.create_failed":
            "this wallet has no API key and creating one failed: {why}",
        /// The request was not sent or not answered; `why` is the transport's words.
        Network { why: String } => "polymarket.l2.network": "network: {why}",
    }
}

vike_utility::refusal_enum! {
    /// Why this process's ONE Polymarket egress is not what a root asked (`crate::transport`'s `declare_egress`,
    /// the composition root's boot line), or not where a run expects it (`crate::transport::EgressCheck`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum PolymarketEgressRefusal with ids PolymarketEgressRefusalId {
        /// A second, different declaration: the first stands.
        AlreadyDeclared => "polymarket.egress.already_declared":
            "Polymarket egress was already declared with different settings; this second declaration is IGNORED \
             — one process has one egress",
        /// The first declaration came after a client had already resolved the built-in default.
        ResolvedBeforeDeclared => "polymarket.egress.resolved_before_declared":
            "Polymarket egress was resolved BEFORE it was declared, so the connections made until now used the \
             built-in default (a direct connection, no proxy) — declare it before the first Polymarket client is \
             built",
        /// The probed egress is not in the expected country: a misrouted run.
        Mismatch { observed: crate::transport::Egress, expected: String } => "polymarket.egress.mismatch":
            "egress is {observed} but the expected egress country is {expected} — this run is NOT routed through \
             the expected region (tunnel down? proxy disabled?)",
    }
}
