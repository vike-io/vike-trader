//! `DatahubClient`'s remaining data-plane verbs: the chart-gap seed (`seed_series`,
//! `seed_series_classed`), the venue instrument catalog (`venue_catalog`, `serves_venue_catalog`),
//! the options-chain snapshot (`options_chain`, `supports_options_chain`) and the options-listing
//! refresh (`options_refresh`), the three tool-feed
//! snapshots (`news_feed`, `economic_calendar`, `equity_calendar`, `supports_tool_feeds`), the
//! irreversible delete
//! (`delete_series`), the archive import (`import_archive`) and a user's CSV upload (`upload_csv`) —
//! each capability-checked and refused locally, sending nothing, when the server does not advertise
//! it. `use super::*` reaches the parent module's imports and shared helpers.
//!
//! Each verb with a SAME-function validator (`seed`, `catalog`, `options`, `archive`) calls it
//! locally for the MESSAGE, never for the enforcement: the server's door re-checks every rule.

use super::*;

vike_utility::refusal_enum! {
    /// What a remote delete ([`DatahubClient::delete_series`]) says when it reaches no delete verb or its answer
    /// was lost: ONE text for `vike-cli data history rm --addr` and MCP's `delete_series`, chosen by
    /// [`DeleteRefusal::of`] on the failure's type. `failure` is the door's own words for the failure.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeleteRefusal with ids DeleteRefusalId {
        /// A CONFIRMING delete that was sent and never answered (decision 0303's OUTCOME UNKNOWN): rows may be gone,
        /// so it is never resent blind; a fresh plan is the read that resolves it.
        OutcomeUnknown { failure: String } => "datahub.delete.outcome_unknown":
            "the delete went out and no answer came back ({failure}): OUTCOME UNKNOWN, and rows may be gone. Take \
             a fresh plan (a dry run) and read it BEFORE deleting again: the plan you confirmed describes a store \
             that may no longer be there.",
        /// This box's device key is not admitted with a write role (decision 0288 item 8: the datahub admits a device
        /// by its access list), or the datahub advertises no delete verb at all, a key-less build
        /// (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`), where no change on this side helps.
        NoDeleteVerb { failure: String } => "datahub.delete.no_delete_verb":
            "{failure}\nThe datahub delete needs this box's device key on that datahub's access list with the \
             control role or higher: `vike-cli backend connect HOST --server-project DIR` registers this \
             computer's key (`vike-cli access ls` on the server shows who may do what). If the datahub \
             advertises no delete verb at all, a key-less build, run the delete ON that box, over `ssh` if it is \
             remote: `vike-cli data history rm --store DIR --kind K --venue V --produced-by PREFIX --dry-run` \
             first, then the same line with `--yes` in place of `--dry-run`.",
    }
}

impl DeleteRefusal {
    /// The words a failed remote delete is shown in, decided on what the call DID: a key the datahub does not
    /// admit or a datahub with no delete verb (at the dial or at [`DatahubClient::delete_series`]) is
    /// [`Self::NoDeleteVerb`], with the remedy; a `deleting` call (not a dry run) sent and never answered is
    /// [`Self::OutcomeUnknown`]; every other failure deleted nothing (the datahub's own refusal, a dial nobody
    /// answered, a lost plan) and is `None`, shown in its own `failure` words.
    #[must_use]
    pub fn of(error: &HubCallError, failure: &str, deleting: bool) -> Option<Self> {
        let failure = failure.to_string();
        match error {
            HubCallError::AccessDenied(_) | HubCallError::NotOffered { .. } => {
                Some(Self::NoDeleteVerb { failure })
            }
            HubCallError::OutcomeUnknown(_) if deleting => Some(Self::OutcomeUnknown { failure }),
            _ => None,
        }
    }

    /// What a door shows for that failure, under ONE id: [`Self::of`]'s refusal, rendered, or `failure` itself. A
    /// refused key's `failure` already shows the door's id (`hub.access.denied`), so its remedy follows bare.
    #[must_use]
    pub fn shown(error: &HubCallError, failure: String, deleting: bool) -> String {
        match Self::of(error, &failure, deleting) {
            Some(refusal) if matches!(error, HubCallError::AccessDenied(_)) => refusal.to_string(),
            Some(refusal) => refusal.rendered(),
            None => failure,
        }
    }
}

impl DatahubClient {
    /// **Tell the server a chart is open on a series its store cannot paint, and let it seed one
    /// window** — [`Request::SeedSeries`].
    ///
    /// The request names a series and NOTHING else: the window, the rate, the venue set and the
    /// interval set are all the server's. A `VerbScope::Read` verb, so unlike [`Self::backfill`] it
    /// is reachable from a connection holding only an observe key
    /// (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), which lets the desktop fill
    /// its own chart against a keyed server.
    ///
    /// **Three local refusals, all before a frame is sent**:
    ///
    /// 1. the capability check — its message names the SWITCH rather than a rebuild, because an
    ///    unadvertised lane on a modern server almost always means `flags.datahub_chart_seed` is
    ///    unset rather than that the server is old;
    /// 2. [`crate::seed::validate_seed_interval`] and 3. `vike_hub_protocol::wire_ident::validate_seed_symbol` —
    ///    the SAME functions the server's door calls (see [`crate::seed`]'s module doc for where the
    ///    security property lives).
    ///
    /// ⚠ **`Ok(done)` with `done.armed == false` is a SUCCESS and must not be rendered as a
    /// failure.** It means the operator has not armed the lane; nothing was fetched and nothing was
    /// written. See [`SeedDone::armed`].
    pub fn seed_series(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<SeedDone, String> {
        self.seed_series_classed(venue, symbol, interval, None)
    }

    /// [`Self::seed_series`], plus the caller's claim about **what KIND of instrument `symbol`
    /// names** — `docs/decisions/0061` Phase 3, negotiated by [`FEATURE_SEED_CLASS`].
    ///
    /// A second entry point rather than a fourth parameter, this workspace's idiom for this axis
    /// (`crates/bridges/bybit/src/history.rs`'s `fetch_klines_range` is
    /// `fetch_klines_range_classed(…, None)`).
    ///
    /// **A FOURTH local refusal, fired only when the class is actually named**: a server that does
    /// not advertise [`FEATURE_SEED_CLASS`] would DECODE this frame, drop the field and fetch the
    /// wrong book while reporting success. ⚠ A `None` request is sent to such a server unchanged;
    /// see that constant for why refusing it instead would be the worse bug.
    pub fn seed_series_classed(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        class: Option<vike_model::AssetClass>,
    ) -> Result<SeedDone, String> {
        if class.is_some() && !self.features().iter().any(|f| f == FEATURE_SEED_CLASS) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SEED_CLASS}` (advertised: {:?}) — \
                 nothing was sent. This request names the instrument's KIND, and a server that \
                 predates that field would DECODE the frame, DROP the claim, route on the symbol \
                 alone and report rows written — which is the wrong-book defect this field exists \
                 to close, wearing a successful answer. Upgrade the data daemon, or ask for this \
                 series without a class claim.",
                self.features()
            ));
        }
        if !self.features().iter().any(|f| f == FEATURE_SEED_SERIES) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SEED_SERIES}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the chart-seed lane, or — far more \
                 likely — its operator has not armed it: run `vike-cli config set \
                 flags.datahub_chart_seed true` on the data daemon and restart it. It is OFF by default deliberately, because an armed \
                 lane spends this box's venue-API budget on behalf of read-only clients.",
                self.features()
            ));
        }
        crate::seed::validate_seed_interval(interval)?;
        vike_hub_protocol::wire_ident::validate_seed_symbol(symbol)?;
        let request = Request::SeedSeries {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            class,
        };
        match self.exchange_shown(&request)? {
            Response::SeriesSeeded(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected SeriesSeeded, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Ask the server for one venue's instrument list** — [`Request::VenueCatalog`].
    ///
    /// The request names a VENUE and nothing else. A `VerbScope::Read` verb, reachable from a
    /// connection holding only an observe key
    /// (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`), so a
    /// desktop that links ONE venue bridge can refresh the catalogs of venues it does not link.
    ///
    /// **Two local refusals, both before a frame is sent**: the capability check (whose message
    /// names the SWITCH rather than a rebuild, as [`Self::seed_series`]' does) and
    /// `vike_hub_protocol::wire_ident::validate_catalog_venue`.
    ///
    /// ⚠ **The REFUSALS come back as `Ok`, not `Err`.** An unarmed lane, a venue with no bulk list,
    /// a credentialed venue and a venue this build does not serve are all
    /// [`crate::catalog::CatalogOutcome`] values on a successful [`crate::catalog::CatalogListing`]
    /// — see [`Response::VenueCatalog`] for why a routine question does not answer with errors.
    /// `Err` here means the venue string was malformed, the server is too old or unarmed to be
    /// asked at all, or a provider failed mid-fetch.
    pub fn venue_catalog(&mut self, venue: &str) -> Result<CatalogListing, String> {
        if !self.features().iter().any(|f| f == FEATURE_VENUE_CATALOG) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_VENUE_CATALOG}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the venue-catalog verb, or its \
                 operator has REFUSED the lane: the catalog is ON by default, and the switch is \
                 `vike-cli config set flags.venue_catalog_off true` on that box. Run `vike-cli \
                 config set flags.venue_catalog_off false` and restart it. The refusal exists because an armed \
                 lane spends that box's venue-API budget on behalf of read-only clients; it is not \
                 what bounds the cost.",
                self.features()
            ));
        }
        vike_hub_protocol::wire_ident::validate_catalog_venue(venue)?;
        let request = Request::VenueCatalog { venue: venue.to_string() };
        match self.exchange_shown(&request)? {
            Response::VenueCatalog(listing) => Ok(listing),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected VenueCatalog, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised the venue-catalog capability — the question a picker asks
    /// BEFORE it offers a refresh control, so an old or unarmed server produces a sentence rather
    /// than a button that always fails.
    ///
    /// ⚠ The leg that stops an un-advertised capability rendering as an EMPTY instrument list,
    /// which for `ig` and `ibkr` would be indistinguishable from the truth.
    pub fn serves_venue_catalog(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_VENUE_CATALOG)
    }

    /// **Ask the server for one underlying's option-chain snapshot** — [`Request::OptionsChain`].
    ///
    /// The request names a VENUE, an UNDERLYING and the EXPIRIES wanted (empty = the front expiry
    /// only; one the server does not list is omitted; more than
    /// [`crate::options::MAX_OPTIONS_EXPIRIES`] is refused) and nothing else. A `VerbScope::Read`
    /// verb, so a desktop holding only an observe key may ask; the server does the venue work
    /// (`crate::proto::required_scope`'s arm), so the desktop links no venue bridge for it.
    ///
    /// **Two local refusals, both before a frame is sent**: the capability check — an old server
    /// is refused BY NAME, with no fallback — and [`validate_options_request`], the function the
    /// server's door calls too.
    ///
    /// ⚠ **The REFUSALS come back as `Ok`, not `Err`.** An unarmed build, a venue or underlying the
    /// server does not serve, too many expiries and a snapshot not yet landed are all
    /// [`crate::options::OptionsChainOutcome`] values on a successful [`OptionsChainReply`], the
    /// [`Self::venue_catalog`] rule. `Err` here means a malformed request, a server too old to be
    /// asked, or a transport failure.
    pub fn options_chain(
        &mut self,
        venue: &str,
        underlying: &str,
        expiries: &[String],
    ) -> Result<OptionsChainReply, String> {
        if !self.supports_options_chain() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_OPTIONS_CHAIN}` (advertised: {:?}) — \
                 nothing was sent. The server predates the options-chain verb: every build that \
                 has the verb advertises it, armed or not, so its absence means an OLD data daemon \
                 rather than a switch. Upgrade the data daemon this client dials \
                 (`config.datahub_addr`).",
                self.features()
            ));
        }
        validate_options_request(venue, underlying, expiries)?;
        let request = Request::OptionsChain {
            venue: venue.to_string(),
            underlying: underlying.to_string(),
            expiries: expiries.to_vec(),
        };
        match self.exchange_shown(&request)? {
            Response::OptionsChain(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected OptionsChain, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised the options-chain capability — the question the Options tool
    /// asks BEFORE it polls, so an old server produces a sentence rather than an error a second.
    pub fn supports_options_chain(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_OPTIONS_CHAIN)
    }

    /// **Ask the server to read the venue's option listing again NOW** — [`Request::OptionsRefresh`],
    /// the "refresh the option list" a user asks for when an underlying was listed or delisted. The
    /// answer is the underlyings the venue lists now, in the listing's order, which the server's options
    /// lane serves from then on.
    ///
    /// A server that does not advertise [`FEATURE_OPTIONS_REFRESH`] is refused HERE, by name, before a
    /// frame is sent. `Err` is also the server's own words when the listing could not be had (its
    /// gatewayhub is down, the venue read failed, a build without the options lane, a venue it does
    /// not serve), never an empty list.
    pub fn options_refresh(&mut self, venue: &str) -> Result<Vec<String>, String> {
        if !self.features().iter().any(|f| f == FEATURE_OPTIONS_REFRESH) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_OPTIONS_REFRESH}` (advertised: {:?}) — \
                 nothing was sent. The server predates the options-listing refresh; upgrade the data \
                 daemon this client dials (`config.datahub_addr`).",
                self.features()
            ));
        }
        let request = Request::OptionsRefresh { venue: venue.to_string() };
        match self.exchange_shown(&request)? {
            Response::OptionsRefreshed { underlyings, .. } => Ok(underlyings),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected OptionsRefreshed, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised [`FEATURE_GATEWAY_STATUS`]: whether it is NEWER than the verb. A
    /// caller reads `false` as "unknown", never as a gatewayhub that is down.
    pub fn serves_gateway_status(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_GATEWAY_STATUS)
    }

    /// **Is the gatewayhub up, as this datahub sees it?** [`Request::GatewayStatus`].
    ///
    /// Refused LOCALLY, sending nothing, unless the server advertised [`FEATURE_GATEWAY_STATUS`]. ⚠ A
    /// gatewayhub that is down is `Ok`, with [`GatewayStatusReply::reached`] false: `Err` is this datahub
    /// too old to be asked, or the datahub's own transport.
    pub fn gateway_status(&mut self) -> Result<GatewayStatusReply, String> {
        if !self.serves_gateway_status() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_GATEWAY_STATUS}` (advertised: {:?}) — \
                 nothing was sent. The server predates this read; upgrade the datahub to ask it.",
                self.features()
            ));
        }
        match self.exchange_shown(&Request::GatewayStatus)? {
            Response::GatewayStatus(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected GatewayStatus, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Tell the running gatewayhub, through this datahub, that account row `account`'s credentials changed**
    /// ([`Request::CredentialsChanged`], decision 0287): it reconnects that account's sessions on what the store holds
    /// now. Only the row id is sent, never a value.
    ///
    /// Refused LOCALLY, sending nothing, unless the server advertised [`FEATURE_CREDENTIALS_CHANGED`]. ⚠ A gatewayhub
    /// that is down is `Ok`, with [`CredentialsChangedReply::reached`] false: `Err` is this datahub too old to relay
    /// it, or the datahub's own transport.
    pub fn credentials_changed(&mut self, account: i64) -> Result<CredentialsChangedReply, String> {
        if !self.features().iter().any(|f| f == FEATURE_CREDENTIALS_CHANGED) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_CREDENTIALS_CHANGED}` — nothing was sent. The \
                 server predates this relay; upgrade the datahub to use it."
            ));
        }
        match self.exchange_shown(&Request::CredentialsChanged { account })? {
            Response::CredentialsChanged(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected CredentialsChanged, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised [`FEATURE_GATEWAY_LINKS`]: whether it is NEWER than the verb.
    pub fn serves_gateway_links(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_GATEWAY_LINKS)
    }

    /// **The venue sessions the gatewayhub recorded** — [`Request::GatewayLinks`]. Refused LOCALLY, sending nothing, unless
    /// the server advertised [`FEATURE_GATEWAY_LINKS`].
    pub fn gateway_links(
        &mut self,
        since_ms: i64,
        route: Option<String>,
        venue: Option<String>,
    ) -> Result<GatewayLinksReply, String> {
        if !self.serves_gateway_links() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_GATEWAY_LINKS}` (advertised: {:?}) — \
                 nothing was sent. The server predates this read; upgrade the datahub to ask it.",
                self.features()
            ));
        }
        match self.exchange_shown(&Request::GatewayLinks { since_ms, route, venue })? {
            Response::GatewayLinks(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected GatewayLinks, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Ask the server for the news feed snapshot** — [`Request::NewsFeed`].
    ///
    /// `if_generation` is the generation the caller already holds (its last
    /// [`crate::tool_feeds::DatahubPollOutcome::generation`]): equal to the server's, the answer is
    /// `Unchanged` and carries no rows. A `VerbScope::Read` verb (`crate::proto::required_scope`'s
    /// arm), so a desktop holding only an observe key may ask; the desktop fetches nothing itself.
    ///
    /// **One local refusal, before a frame is sent**: the capability check — an old server is
    /// refused BY NAME, with no fallback. ⚠ **Every other outcome comes back as `Ok`**: an unarmed
    /// build, a first poll not yet landed and a feed that needs a key are
    /// [`crate::tool_feeds::DatahubPollOutcome`] values on a successful reply, [`Self::options_chain`]'s
    /// rule. `Err` is a server too old to be asked, or a transport failure.
    pub fn news_feed(&mut self, if_generation: Option<u64>) -> Result<NewsReply, String> {
        self.require_tool_feeds()?;
        match self.exchange_shown(&Request::NewsFeed { if_generation })? {
            Response::NewsFeed(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected NewsFeed, got {}", response_kind(&other)))
            }
        }
    }

    /// **Ask the server for the economic-calendar snapshot** — [`Request::EconomicCalendar`].
    /// [`Self::news_feed`]'s refusals, outcomes and generation rule, verbatim.
    pub fn economic_calendar(
        &mut self,
        if_generation: Option<u64>,
    ) -> Result<CalendarEconomicReply, String> {
        self.require_tool_feeds()?;
        let request = Request::EconomicCalendar { if_generation };
        match self.exchange_shown(&request)? {
            Response::EconomicCalendar(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected EconomicCalendar, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Ask the server for one week's earnings, dividends and IPOs** —
    /// [`Request::EquityCalendar`]. `week_start` is the week's Monday, ISO `YYYY-MM-DD`.
    ///
    /// [`Self::news_feed`]'s refusals, outcomes and generation rule, plus a SECOND local refusal:
    /// [`validate_equity_week_shape`], the function the server's door calls too. Whether the date is
    /// a Monday near enough to the server's today is the SERVER's question (its clock, not this
    /// client's): a week it will not serve comes back as `Ok` with every feed
    /// `Refused(WeekOutOfRange)`, naming the Mondays it does serve.
    pub fn equity_calendar(
        &mut self,
        week_start: &str,
        if_generation: Option<u64>,
    ) -> Result<CalendarEquityReply, String> {
        self.require_tool_feeds()?;
        validate_equity_week_shape(week_start)?;
        let request = Request::EquityCalendar { week_start: week_start.to_string(), if_generation };
        match self.exchange_shown(&request)? {
            Response::EquityCalendar(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected EquityCalendar, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised the tool-feed capability — the question the News, Calendar,
    /// Earnings, Dividends and IPO windows ask BEFORE they poll, so an old server produces a
    /// sentence rather than an error a minute.
    pub fn supports_tool_feeds(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_TOOL_FEEDS)
    }

    /// The capability refusal the three tool-feed verbs share: an OLD data daemon, refused by name
    /// before any frame ([`Self::options_chain`]'s argument — every build that has the verbs
    /// advertises them, armed or not).
    fn require_tool_feeds(&self) -> Result<(), String> {
        if self.supports_tool_feeds() {
            return Ok(());
        }
        Err(format!(
            "datahub server does not advertise `{FEATURE_TOOL_FEEDS}` (advertised: {:?}) — \
             nothing was sent. The server predates the news / calendar verbs: every build that has \
             them advertises them, armed or not, so their absence means an OLD data daemon rather \
             than a switch. Upgrade the data daemon this client dials (`config.datahub_addr`).",
            self.features()
        ))
    }

    /// The capability refusal of a bar read that names a `source=` lane ([`FEATURE_BAR_SOURCE`]): an OLD
    /// data daemon, refused by name before any frame, because it would ignore the lane and answer the
    /// venue's candles as the lane's bars.
    pub(super) fn require_bar_source(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_BAR_SOURCE) {
            return Ok(());
        }
        Err(format!(
            "datahub server does not advertise `{FEATURE_BAR_SOURCE}` (advertised: {:?}) — nothing was \
             sent. The server predates bar lanes on this wire: it would DROP the lane and answer the \
             venue's candles in its place. Upgrade the data daemon this client dials \
             (`config.datahub_addr`).",
            self.features()
        ))
    }

    /// The capability refusal of a request that names a `source=` lane ([`FEATURE_SOURCE`]): an OLD data
    /// daemon, refused by name before any frame, because it would ignore the lane and act on the archive.
    pub(super) fn require_source(&self) -> Result<(), String> {
        if self.features().iter().any(|f| f == FEATURE_SOURCE) {
            return Ok(());
        }
        Err(format!(
            "datahub server does not advertise `{FEATURE_SOURCE}` (advertised: {:?}) — nothing was \
             sent. The server predates `source=` lanes on this wire: it would DROP the lane and read or \
             delete the archive's series in its place. Upgrade the data daemon this client dials \
             (`config.datahub_addr`).",
            self.features()
        ))
    }

    /// **Delete stored series on the server, IRREVERSIBLY** — `dry_run: true` returns the plan and
    /// removes nothing.
    ///
    /// ⚠ The advertisement check REFUSES LOCALLY, without sending, and its message says what is
    /// missing: this verb is served only by a KEYED datahub
    /// ([`FEATURE_DELETE_SERIES`] carries why), so its absence means a key-less build: a datahub
    /// of today always authenticates by its access list (decision 0288 item 8) and advertises it.
    ///
    /// The provenance rule is deliberately NOT duplicated here: it is the one gate that matters,
    /// and the server checks it under the series lock.
    ///
    /// ⚠ **The failure is the TYPED [`HubCallError`]**, because the doors decide on what the call DID
    /// (decision 0301): a confirming delete sent and never answered is OUTCOME UNKNOWN, rows may be gone,
    /// and [`DeleteRefusal::of`] is the one place that says so for `vike-cli data history rm --addr` and
    /// MCP's `delete_series` alike.
    pub fn delete_series(
        &mut self,
        selector: &SeriesSelector,
        produced_by: Option<&str>,
        dry_run: bool,
    ) -> Result<DeleteDone, HubCallError> {
        if !self.features().iter().any(|f| f == FEATURE_DELETE_SERIES) {
            let text = format!(
                "datahub server does not advertise `{FEATURE_DELETE_SERIES}` (advertised: {:?}) — \
                 nothing was sent. This verb is served ONLY by a KEYED datahub: a server with no \
                 hub keys has no way to say 'reads yes, deletes no', so it serves no delete at \
                 all. A current datahub always authenticates by its access list and serves it: \
                 upgrade that server, or run the delete on the box with \
                 `vike-cli data history rm --store DIR`.",
                self.features()
            );
            return Err(HubCallError::NotOffered {
                feature: FEATURE_DELETE_SERIES.to_string(),
                text,
            });
        }
        if selector.source.is_some() {
            self.require_source().map_err(|text| HubCallError::NotOffered {
                feature: FEATURE_SOURCE.to_string(),
                text,
            })?;
        }
        let request = Request::DeleteSeries {
            selector: selector.clone(),
            produced_by: produced_by.map(str::to_string),
            dry_run,
        };
        // NOT `exchange_shown`: this verb decides on what the call DID, so it keeps the `HubCallError` and reads
        // the refusal frame itself (that function's own rule), shown in the same `error [<id>]: <text>` words.
        match self.exchange(&request)? {
            Response::Deleted(done) => Ok(done),
            Response::Error(text) => Err(HubCallError::Refused { id: None, text }),
            Response::Refused(refusal) => {
                Err(HubCallError::Refused { id: Some(refusal.id), text: refusal.text })
            }
            other => Err(HubCallError::Protocol(
                format!("protocol desync: expected Deleted, got {}", response_kind(&other)).into(),
            )),
        }
    }

    /// **Import one archive dataset from the SERVER's own imports directory** —
    /// [`Request::ImportArchive`]. `spec.dry_run` returns the plan and writes nothing; see
    /// [`crate::archive`] for the whole contract.
    ///
    /// **Three local refusals, all before a frame is written** (and so before a server-side
    /// directory is touched):
    ///
    /// 1. the capability check — the server must advertise [`FEATURE_ARCHIVE_IMPORT`], which only
    ///    a datahub with an import lane MOUNTED does;
    /// 2. the FORMAT check — the server must advertise `import_format=<spec.format>`
    ///    ([`crate::proto::advertised_import_formats`]); the refusal names the formats it does
    ///    import, so a misspelling is told apart from a format this server lacks;
    /// 3. [`crate::archive::validate_import_spec`] — the dataset, the bars and the window (the
    ///    server also resolves the omitted bounds this side cannot see).
    ///
    /// ⚠ **This request is SYNCHRONOUS and its read is unbounded by default**
    /// ([`crate::proto::reply_wait`] classes it `Unbounded`): an import holds the connection until
    /// the last day is stored, which is why a decoding request is capped at
    /// [`crate::archive::IMPORT_MAX_DAYS`] days — a caller importing a long window sends one
    /// request per calendar month.
    pub fn import_archive(&mut self, spec: &ImportSpec) -> Result<ImportDone, String> {
        if !self.features().iter().any(|f| f == FEATURE_ARCHIVE_IMPORT) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_ARCHIVE_IMPORT}` (advertised: {:?}) — \
                 nothing was sent. Either the server predates the archive-import verb, or it mounts \
                 no import lane: a datahub mounts one only when its build carries an archive format \
                 and a project directory sits above it, whose `market_data/imports/` is the one \
                 place it reads archive files from.",
                self.features()
            ));
        }
        let formats = advertised_import_formats(self.features());
        if !formats.contains(&spec.format) {
            return Err(format!(
                "datahub server does not import the archive format {:?} — it advertises {formats:?} \
                 — nothing was sent. A format is matched exactly against the server's own registry; \
                 name one it advertises.",
                spec.format
            ));
        }
        if spec.replace_holed && !self.features().iter().any(|f| f == FEATURE_IMPORT_REPLACE_HOLED)
        {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_IMPORT_REPLACE_HOLED}` — nothing was \
                 sent. It predates the option to replace a holed day, and would ignore it and run a \
                 plain import. Upgrade the datahub, or import without it."
            ));
        }
        if let Some(target) = &spec.target {
            self.refuse_newer_target_words(target)?;
        }
        validate_import_spec(spec)?;
        let request = Request::ImportArchive(spec.clone());
        match self.exchange_shown(&request)? {
            Response::ArchiveImported(done) => Ok(*done),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ArchiveImported, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Download one dataset's daily files of a vendor's bucket into the SERVER's imports folder** —
    /// [`Request::FetchArchive`]. `spec.go == false` lists the bucket and answers the plan, downloading
    /// nothing; see [`crate::archive_fetch`] for the whole contract.
    ///
    /// Refused locally, before a frame is written, when the server does not advertise
    /// [`crate::proto::FEATURE_ARCHIVE_FETCH`] or the spec breaks
    /// [`crate::archive_fetch::validate_fetch_spec`]. Unbounded read, as [`Self::import_archive`]:
    /// a downloading request holds the connection until its last file is written.
    pub fn fetch_archive(
        &mut self,
        spec: &crate::archive_fetch::FetchSpec,
    ) -> Result<crate::archive_fetch::FetchDone, String> {
        use crate::proto::FEATURE_ARCHIVE_FETCH;
        if !self.features().iter().any(|f| f == FEATURE_ARCHIVE_FETCH) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_ARCHIVE_FETCH}` (advertised: {:?}) — \
                 nothing was sent. Either it predates the archive download, or it mounts none: it \
                 mounts one only when its build carries the import lane and its \
                 `venue_url.dukascopy.live.history.rest.archive` row names the vendor's bucket.",
                self.features()
            ));
        }
        crate::archive_fetch::validate_fetch_spec(spec)?;
        let request = Request::FetchArchive(spec.clone());
        match self.exchange_shown(&request)? {
            Response::ArchiveFetched(done) => Ok(*done),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ArchiveFetched, got {}",
                response_kind(&other)
            )),
        }
    }

    /// A `csv-bars` target's column mapping or the `user` venue, refused locally when the server does
    /// not advertise [`FEATURE_CSV_COLUMNS`]: an older one ignores the mapping and has no such venue.
    fn refuse_newer_target_words(&self, target: &ImportTarget) -> Result<(), String> {
        let newer =
            target.columns.is_some() || target.venue == vike_data::store::store_kind::USER_VENUE;
        if newer && !self.features().iter().any(|f| f == FEATURE_CSV_COLUMNS) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_CSV_COLUMNS}` — nothing was sent. It \
                 predates a CSV's column mapping and the `user` venue, and would read the file by \
                 its header's names. Upgrade the datahub, or send the file without them."
            ));
        }
        Ok(())
    }

    /// **Upload a user's CSV from this machine into a `user-*` lane of the datahub's store**
    /// ([`Request::CsvUpload`], decision 0272): open the upload, send `text` in
    /// [`crate::upload::UPLOAD_CHUNK_BYTES`] pieces (calling `progress(sent, total)` after each), and
    /// end it. A `VerbScope::Read` verb: the observe key suffices. [`crate::upload`]'s module doc
    /// carries the exchange and why nothing is stored when it breaks off.
    ///
    /// `dry_run` decodes and plans and stores nothing; `head` says `text` is only the file's first
    /// [`crate::upload::UPLOAD_HEAD_BYTES`] (a preview, a dry run only).
    ///
    /// `Ok` carries how the file was read, a file the decoder refused included
    /// ([`CsvUploadDone::refused`]); `Err` is the upload refused (the lane unarmed, the series already
    /// holding rows, the file over the server's cap …) or broken off, with nothing stored.
    pub fn upload_csv(
        &mut self,
        target: &ImportTarget,
        text: &str,
        dry_run: bool,
        head: bool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<CsvUploadDone, String> {
        if !self.features().iter().any(|f| f == FEATURE_CSV_UPLOAD) {
            return Err(format!(
                "this datahub takes no CSV upload — `{FEATURE_CSV_UPLOAD}` is not among its \
                 capabilities, so nothing was sent. Its operator turns uploads on with \
                 `vike-cli config set flags.datahub_csv_upload true` on the datahub's box and a \
                 restart; a datahub older than the upload has no such switch."
            ));
        }
        let spec =
            CsvUploadSpec { target: target.clone(), bytes: text.len() as u64, dry_run, head };
        validate_upload_spec(&spec)?;
        let total = spec.bytes;
        match self.exchange_shown(&Request::CsvUpload(spec))? {
            Response::CsvUploadReady => {}
            Response::Error(msg) => return Err(msg),
            other => {
                return Err(format!(
                    "protocol desync: expected CsvUploadReady, got {}",
                    response_kind(&other)
                ));
            }
        }
        for piece in crate::upload::chunks(text) {
            let request = Request::CsvUploadChunk { text: piece.to_string() };
            match self.exchange(&request).map_err(|e| broken_off(&e.to_string()))? {
                Response::CsvUploadReceived { bytes } => progress(bytes, total),
                Response::Error(msg) => return Err(msg),
                Response::Refused(refusal) => return Err(crate::refusals::shown(&refusal)),
                other => {
                    return Err(format!(
                        "protocol desync: expected CsvUploadReceived, got {}",
                        response_kind(&other)
                    ));
                }
            }
        }
        match self.exchange_shown(&Request::CsvUploadEnd)? {
            Response::CsvUploaded(done) => Ok(*done),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected CsvUploaded, got {}", response_kind(&other)))
            }
        }
    }

    /// Whether this server advertised [`FEATURE_SYMBOL_SETS`]: whether it holds the user's symbol lists.
    pub fn serves_symbol_sets(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_SYMBOL_SETS)
    }

    /// **The user's named symbol lists** — [`Request::SymbolSets`]. Empty is the ordinary first answer.
    pub fn symbol_sets(&mut self) -> Result<Vec<vike_data::datasets::SymbolSet>, String> {
        self.symbol_set_exchange(&Request::SymbolSets)
    }

    /// **Save one list**, replacing the list of its name — [`Request::SetSymbolSet`], a Control verb. Answers every
    /// list after the write.
    pub fn set_symbol_set(
        &mut self,
        set: vike_data::datasets::SymbolSet,
    ) -> Result<Vec<vike_data::datasets::SymbolSet>, String> {
        self.symbol_set_exchange(&Request::SetSymbolSet { set })
    }

    /// **Remove the list called `name`** — [`Request::DeleteSymbolSet`], a Control verb. Answers every list after it.
    pub fn delete_symbol_set(
        &mut self,
        name: &str,
    ) -> Result<Vec<vike_data::datasets::SymbolSet>, String> {
        self.symbol_set_exchange(&Request::DeleteSymbolSet { name: name.to_string() })
    }

    /// Whether this server advertised [`FEATURE_INSTRUMENT_LISTS`]: whether it keeps the venues' instrument lists.
    pub fn serves_instrument_lists(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_INSTRUMENT_LISTS)
    }

    /// **Every venue's kept instrument list and the operator's own lists** — [`Request::InstrumentLists`]. Refused
    /// LOCALLY, sending nothing, unless the server advertised [`FEATURE_INSTRUMENT_LISTS`].
    pub fn instrument_lists(&mut self) -> Result<crate::catalog::InstrumentListsReply, String> {
        self.require_instrument_lists()?;
        match self.exchange_shown(&Request::InstrumentLists)? {
            Response::InstrumentLists(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected InstrumentLists, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **The kept instrument lists of `venues` alone** — [`Request::InstrumentListsOfVenues`], the answer
    /// [`Self::instrument_lists`] gives passed through `InstrumentListsReply::of_venues`. A datahub that does not
    /// advertise [`crate::proto::FEATURE_INSTRUMENT_LISTS_OF_VENUES`] (one too old to filter) is asked the whole
    /// lists and they are filtered HERE by the same filter, so the answer is the same either way; one that keeps no
    /// list is refused locally as [`Self::instrument_lists`] is.
    pub fn instrument_lists_of_venues(
        &mut self,
        venues: &[String],
    ) -> Result<crate::catalog::InstrumentListsReply, String> {
        let filters = crate::proto::FEATURE_INSTRUMENT_LISTS_OF_VENUES;
        if !self.features().iter().any(|f| f == filters) {
            return self.instrument_lists().map(|whole| whole.of_venues(venues));
        }
        self.require_instrument_lists()?;
        let request = Request::InstrumentListsOfVenues { venues: venues.to_vec() };
        match self.exchange_shown(&request)? {
            Response::InstrumentLists(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected InstrumentLists, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised [`crate::proto::FEATURE_KEPT_ROWS_STAMPED`]: whether it answers the kept rows in
    /// binary with a stamp per venue.
    pub fn serves_kept_rows_stamped(&self) -> bool {
        self.features().iter().any(|f| f == crate::proto::FEATURE_KEPT_ROWS_STAMPED)
    }

    /// **The kept rows and share splits of `venues` in binary, each with its stamp** — [`Request::KeptRowsOfVenues`]:
    /// a venue whose stamp `known` holds comes back "unchanged" (`changed: None`), every other with its rows. Refused
    /// LOCALLY, sending nothing, unless the server advertised [`crate::proto::FEATURE_KEPT_ROWS_STAMPED`]; a refusal
    /// comes back as `Response::Error` JSON, told from the binary body by [`crate::proto::KEPT_ROWS_MAGIC`].
    pub fn kept_rows_of_venues(
        &mut self,
        venues: &[String],
        known: &std::collections::BTreeMap<String, String>,
    ) -> Result<Vec<crate::proto::KeptRowsOfVenue>, String> {
        let stamped = crate::proto::FEATURE_KEPT_ROWS_STAMPED;
        if !self.serves_kept_rows_stamped() {
            return Err(format!(
                "datahub server does not advertise `{stamped}` (advertised: {:?}) — nothing was sent",
                self.features()
            ));
        }
        let request = Request::KeptRowsOfVenues { venues: venues.to_vec(), known: known.clone() };
        let reply = self
            .conn
            .exchange_frame_raw(
                request_kind(&request),
                reply_wait(&request).default_bound(),
                &request,
            )
            .map_err(|e| e.to_string())?;
        if reply.starts_with(&crate::proto::KEPT_ROWS_MAGIC) {
            return crate::proto::decode_kept_rows(&reply);
        }
        // A refusal comes in the form this client writes (JSON, or the msgpack it negotiated).
        match vike_hub_protocol::frame::decode_body::<Response>(&reply) {
            Ok(Response::Error(msg)) => Err(msg),
            Ok(Response::Refused(refusal)) => Err(crate::refusals::shown(&refusal)),
            Ok(other) => {
                Err(format!("protocol desync: expected kept rows, got {}", response_kind(&other)))
            }
            Err(e) => Err(format!(
                "the reply to a kept-rows read is neither kept rows nor a reply frame ({} bytes): {e}",
                reply.len()
            )),
        }
    }

    /// **Refresh one venue's list now, or every venue's** (`None`) — [`Request::RefreshInstrumentLists`], a Control
    /// verb. Refused locally as [`Self::instrument_lists`] is.
    pub fn refresh_instrument_lists(
        &mut self,
        venue: Option<&str>,
    ) -> Result<Vec<crate::catalog::VenueRefresh>, String> {
        self.require_instrument_lists()?;
        let request = Request::RefreshInstrumentLists { venue: venue.map(str::to_string) };
        match self.exchange_shown(&request)? {
            Response::InstrumentListsRefreshed(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected InstrumentListsRefreshed, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **The instruments the operator adds or removes by hand** — [`Request::OperatorInstruments`], a Control verb; an
    /// empty change reads every row. Refused LOCALLY, sending nothing, unless the server advertised
    /// [`FEATURE_OPERATOR_INSTRUMENTS`].
    pub fn operator_instruments(
        &mut self,
        venue: &str,
        add: Vec<String>,
        remove: Vec<String>,
        stated: Option<crate::catalog::StatedInstrumentFacts>,
    ) -> Result<crate::catalog::OperatorInstrumentsReply, String> {
        if !self.features().iter().any(|f| f == FEATURE_OPERATOR_INSTRUMENTS) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_OPERATOR_INSTRUMENTS}` (advertised: {:?}) — nothing was \
                 sent. The server predates the operator's own instruments; upgrade the datahub to keep them.",
                self.features()
            ));
        }
        let request =
            Request::OperatorInstruments { venue: venue.to_string(), add, remove, stated };
        match self.exchange_shown(&request)? {
            Response::OperatorInstruments(reply) => Ok(reply),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected OperatorInstruments, got {}",
                response_kind(&other)
            )),
        }
    }

    /// Whether this server advertised [`FEATURE_SPLIT_WARNINGS`]: whether it can be asked for its split warnings.
    pub fn serves_split_warnings(&self) -> bool {
        self.features().iter().any(|f| f == FEATURE_SPLIT_WARNINGS)
    }

    /// **The open split warnings** — [`Request::SplitWarnings`], an Observe verb. Refused LOCALLY, sending nothing,
    /// unless the server advertised [`FEATURE_SPLIT_WARNINGS`].
    pub fn split_warnings(&mut self) -> Result<Vec<crate::catalog::SplitWarning>, String> {
        if !self.serves_split_warnings() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SPLIT_WARNINGS}` (advertised: {:?}) — nothing was \
                 sent. The server predates the split warnings; upgrade the datahub to read them.",
                self.features()
            ));
        }
        match self.exchange_shown(&Request::SplitWarnings)? {
            Response::SplitWarnings(warnings) => Ok(warnings),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected SplitWarnings, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **The live tokens of the rolling families the datahub's recorder records** — [`Request::LiveFamilyTokens`], an
    /// Observe verb answered from the recorder's memory. Refused LOCALLY, sending nothing, unless the server
    /// advertised [`FEATURE_LIVE_FAMILY_TOKENS`]: a datahub without it is older than this client.
    pub fn live_family_tokens(&mut self) -> Result<Vec<crate::family::LiveFamilyToken>, String> {
        if !self.features().iter().any(|f| f == FEATURE_LIVE_FAMILY_TOKENS) {
            return Err(format!(
                "the datahub is older than this desktop: it does not advertise `{FEATURE_LIVE_FAMILY_TOKENS}`, so it \
                 cannot say which tokens its recorder follows — nothing was sent. Upgrade the datahub."
            ));
        }
        match self.exchange_shown(&Request::LiveFamilyTokens)? {
            Response::LiveFamilyTokens(tokens) => Ok(tokens),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected LiveFamilyTokens, got {}",
                response_kind(&other)
            )),
        }
    }

    fn require_instrument_lists(&self) -> Result<(), String> {
        if self.serves_instrument_lists() {
            return Ok(());
        }
        Err(format!(
            "datahub server does not advertise `{FEATURE_INSTRUMENT_LISTS}` (advertised: {:?}) — nothing was sent. \
             The server keeps no instrument lists: it runs with no project folder, or it predates them (then \
             upgrade the datahub to read them).",
            self.features()
        ))
    }

    /// The one exchange of the three symbol-list verbs, refused LOCALLY, sending nothing, unless the server
    /// advertised [`FEATURE_SYMBOL_SETS`].
    fn symbol_set_exchange(
        &mut self,
        request: &Request,
    ) -> Result<Vec<vike_data::datasets::SymbolSet>, String> {
        if !self.serves_symbol_sets() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_SYMBOL_SETS}` (advertised: {:?}) — nothing was sent. \
                 The server predates the symbol lists; upgrade the datahub to keep them there.",
                self.features()
            ));
        }
        match self.exchange_shown(request)? {
            Response::SymbolSets(sets) => Ok(sets),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SymbolSets, got {}", response_kind(&other)))
            }
        }
    }
}

/// A transport failure in the middle of sending the file: the server dropped the upload with it.
fn broken_off(why: &str) -> String {
    format!(
        "the upload broke off while sending the file ({why}). Nothing was stored: the datahub holds \
         an upload in memory until its end and drops it with the connection. Send it again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A failed delete is shown by what it DID** (decisions 0301, 0303): a confirming delete whose answer was
    /// lost is OUTCOME UNKNOWN and a lost PLAN is not; a key not admitted or no delete verb carries the remedy on
    /// either pass; a failure that deleted nothing keeps its own words.
    #[test]
    fn a_failed_delete_is_shown_by_what_it_did() {
        let lost = || HubCallError::OutcomeUnknown(io::Error::from(io::ErrorKind::TimedOut));
        let unknown = DeleteRefusal::of(&lost(), "the words", true).expect("rows may be gone");
        assert_eq!(unknown.id(), DeleteRefusalId::OutcomeUnknown);
        assert!(
            unknown.to_string().contains("(the words)") && unknown.to_string().contains("UNKNOWN")
        );
        assert_eq!(
            DeleteRefusal::of(&lost(), "the words", false),
            None,
            "a lost plan deleted nothing"
        );
        let no_verb = || HubCallError::NotOffered {
            feature: FEATURE_DELETE_SERIES.to_string(),
            text: "the words".to_string(),
        };
        for error in [HubCallError::AccessDenied("the words".to_string()), no_verb()] {
            for deleting in [false, true] {
                let refusal = DeleteRefusal::of(&error, "the words", deleting).expect("the remedy");
                assert!(
                    refusal
                        .rendered()
                        .starts_with("error [datahub.delete.no_delete_verb]: the words\n"),
                    "{refusal}"
                );
            }
        }
        for nothing_deleted in [
            HubCallError::NotSent(io::Error::from(io::ErrorKind::ConnectionRefused)),
            HubCallError::Refused { id: None, text: "the words".to_string() },
            HubCallError::Protocol("the words".into()),
        ] {
            assert_eq!(DeleteRefusal::of(&nothing_deleted, "x", true), None, "{nothing_deleted:?}");
        }
    }

    /// **A refused key's delete shows ONE id, the door's**: its failure already reads `error [hub.access.denied]`,
    /// so the remedy follows it bare; an older datahub's missing verb shows the delete's own id.
    #[test]
    fn a_refused_keys_delete_shows_the_doors_one_id() {
        let denied =
            HubCallError::AccessDenied("datahub auth denied (Write): access denied".into());
        for deleting in [false, true] {
            let shown = DeleteRefusal::shown(&denied, denied.to_string(), deleting);
            assert!(
                shown.starts_with(
                    "error [hub.access.denied]: datahub auth denied (Write): access denied\nThe datahub delete \
                     needs this box's device key"
                ),
                "{shown}"
            );
            assert_eq!(shown.matches("error [").count(), 1, "ONE id: {shown}");
        }
        let older = HubCallError::NotOffered {
            feature: FEATURE_DELETE_SERIES.to_string(),
            text: "the words".to_string(),
        };
        let shown = DeleteRefusal::shown(&older, older.to_string(), true);
        assert!(shown.starts_with("error [datahub.delete.no_delete_verb]: the words\n"), "{shown}");
    }
}
