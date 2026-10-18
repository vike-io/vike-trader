//! The pure core of the catalog refresh: outcomes, availability, the budget and the row model.

use super::*;

// ------------------------------------------------------------------------------------------------
// The pure core
// ------------------------------------------------------------------------------------------------

/// What one refresh attempt DID to the cache. Every arm carries the number the operator needs to
/// tell "it worked" from "it did nothing": the new count, or the count that was KEPT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The venue handed over a non-empty list; the cache now holds `count` of its instruments.
    ///
    /// `truncated` is the wire's own flag, carried rather than dropped: a `ServerBacked` listing
    /// that hit `vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS` is SHORT, and a picker
    /// silently missing a venue's tail is a bug report nobody can reproduce. A `Direct` fetch is
    /// never truncated — no cap sits between a linked provider and this fold.
    Refreshed { count: usize, previous: usize, truncated: bool },
    /// The venue answered, with nothing in it. **The cache was not touched** — see the module doc.
    Empty { kept: usize },
    /// The fetch failed. **The cache was not touched.**
    Failed { error: String, kept: usize },
    /// **Nothing was fetched, and that is not a failure.** An unarmed lane, a server whose build
    /// carries no provider for the venue, a credentialed venue, a venue with no bulk list, or a
    /// server that does not advertise the capability at all. **The cache was not touched.**
    ///
    /// ⚠ `why` is the SERVER-side type's own sentence
    /// (`vike_datahub_client::catalog::CatalogListing::describe`), never one assembled here — so
    /// the daemon, the CLI and this row cannot describe one refusal three different ways.
    Refused { why: String, kept: usize },
}

impl RefreshOutcome {
    /// Did this outcome change the cache? Only [`Self::Refreshed`] does.
    #[must_use]
    pub fn adopted(&self) -> bool {
        matches!(self, Self::Refreshed { .. })
    }

    /// One line for the row, in the operator's terms.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Refreshed { count, previous, truncated: false } => {
                format!("refreshed — {previous} → {count} instruments")
            }
            Self::Refreshed { count, previous, truncated: true } => format!(
                "refreshed — {previous} → {count} instruments, TRUNCATED at the wire cap: the tail \
                 of this venue's universe is missing"
            ),
            Self::Empty { kept } => {
                format!("the venue answered with nothing — kept the {kept} already cached")
            }
            Self::Failed { error, kept } => {
                format!("failed — kept the {kept} already cached: {error}")
            }
            Self::Refused { why, kept: 0 } => why.clone(),
            Self::Refused { why, kept } => format!("{why} (kept the {kept} already cached)"),
        }
    }
}

/// **Fold one venue's fetch into the whole-catalog cache.** The one place a cached list is
/// replaced, and the one place the [`VenueStamp`] is written.
///
/// ⚠ **The two early returns are the safety property, not tidiness.** A `Err` and an EMPTY `Ok`
/// both return before a single instrument is removed, so a venue that refuses, times out or
/// answers with nothing keeps exactly the list it had. Moving either `return` below the `retain`
/// is what `a_failed_refresh_keeps_the_previous_cache` mutation-proves against.
///
/// Every other venue's rows are untouched in every arm: the `retain` is keyed on THIS venue.
///
/// `truncated` is the wire's flag and reaches the outcome unchanged — it is a fact about the FETCH
/// rather than about the fold, and it is a parameter here rather than a field set afterwards so
/// that no caller can adopt a short list and forget to say so.
pub fn merge_refresh(
    cache: &mut CatalogCache,
    venue: &str,
    fetched: Result<Vec<Instrument>, String>,
    now_ms: i64,
    truncated: bool,
) -> RefreshOutcome {
    let previous = cache.instruments.iter().filter(|i| i.venue == venue).count();
    let list = match fetched {
        Err(error) => return RefreshOutcome::Failed { error, kept: previous },
        Ok(list) if list.is_empty() => return RefreshOutcome::Empty { kept: previous },
        Ok(list) => list,
    };
    let count = list.len();
    cache.instruments.retain(|i| i.venue != venue);
    cache.instruments.extend(list);
    match cache.fetched.iter_mut().find(|s| s.venue == venue) {
        Some(stamp) => {
            stamp.last_refreshed_ms = now_ms;
            stamp.count = count;
        }
        None => cache.fetched.push(VenueStamp {
            venue: venue.to_string(),
            last_refreshed_ms: now_ms,
            count,
        }),
    }
    RefreshOutcome::Refreshed { count, previous, truncated }
}

/// Whether this binary can refresh a venue at all, by which route, and why not when it cannot.
///
/// ⚠ **Six variants, and the last three are the ones 0062 exists for.** Three of these are answers
/// about THIS BUILD (`Direct`, `QueryBacked`, `NotInThisBuild`); `ServerBacked` is an answer about
/// a ROUTE; `Credentialed` and `NoBulkList` are facts about the VENUE that no build and no switch
/// can change. Collapsing the last two into "not in this build" is what told an operator that five
/// venues were a packaging problem when they are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshAvailability {
    /// Enumerable, with a provider linked into this binary: the button is live and the venue is
    /// called from THIS box.
    Direct,
    /// Publicly enumerable, no provider linked here: the button is live and the venue is called by
    /// the backend's datahub, from ITS box and against ITS budget. See [`SERVER_BACKED_NOTE`].
    ServerBacked,
    /// A LINKED provider whose [`CatalogMode`] is `QueryBacked` — there is no list to fetch. See
    /// [`QUERY_BACKED_NOTE`].
    QueryBacked,
    /// `vike_catalog::CatalogAvailability::Credentialed` — a bulk list exists and reaching it
    /// spends the OPERATOR's venue identity, which 0062's decision 3 says a server will not do on
    /// a client's request.
    ///
    /// ⚠ **This doc said "Nothing arms this" and that is no longer true.**
    /// `docs/decisions/0066`'s decision 9 armed a locally-credentialed refresh: on the box that
    /// holds this venue's keys the OPERATOR lists it themselves, with their own credentials, and
    /// nothing asks a server to spend anything. What is still true — and is the fence the record
    /// puts round that decision — is that no route from THIS binary arms, and no server switch
    /// arms in either direction.
    ///
    /// `own_keys` is whether this box holds credentials for the venue, and it changes only the
    /// SENTENCE: a venue with keys is told which act lists it, a venue without is told what would
    /// arm that act. Neither grows a control here, because this binary links none of those three
    /// bridges — `crates/vike-desktop/Cargo.toml`'s tombstone deleted them on the ground that a
    /// catalog fetch is a venue REST call, and re-linking them is a decision that record
    /// deliberately did not take.
    Credentialed { own_keys: bool },
    /// `vike_catalog::CatalogAvailability::NoBulkList` — the venue publishes no list at any price.
    /// `why` is the table's own operator-facing reason.
    NoBulkList { why: &'static str },
    /// Neither a linked provider nor a classified route — the fail-closed arm, reached by a venue
    /// off `vike_model::VENUES`. See [`NOT_LINKED_NOTE`].
    NotInThisBuild,
}

impl RefreshAvailability {
    /// Whether a press can do anything at all. The ONE predicate [`refresh_block`] gates on, so a
    /// new variant cannot silently become refreshable.
    #[must_use]
    pub fn refreshable(self) -> bool {
        matches!(self, Self::Direct | Self::ServerBacked)
    }

    /// The reason text, or `None` when the refresh is available.
    ///
    /// ⚠ **It takes the venue and returns an owned `String`**, where it used to be a bare
    /// `Option<&'static str>`. Both changes are forced by the same thing: the two refusal arms
    /// render through `vike_datahub_client::catalog::CatalogRefusal::describe`, which names the
    /// venue in its sentence and is the ONE spelling of it in the workspace. A `&'static str`
    /// could not carry the venue and a local `format!` would be a second spelling nothing compares.
    #[must_use]
    pub fn note(self, venue: &str) -> Option<String> {
        match self {
            Self::Direct => None,
            Self::ServerBacked => Some(SERVER_BACKED_NOTE.to_string()),
            Self::QueryBacked => Some(QUERY_BACKED_NOTE.to_string()),
            // ⚠ The TRUNK is the server-side sentence, unchanged and still the ONE spelling —
            // `CatalogRefusal::NeedsCredentials::describe`, which names the ACT (`vike-backend catalog
            // refresh <venue>`) and no screen, so a daemon printing it says something true.
            // What is appended is a fact about THIS BOX, and it is marked as one: whether the keys
            // that act needs are here. A sentence that named Connections inside the shared spelling
            // would put a desktop screen in a daemon's log.
            Self::Credentialed { own_keys } => {
                let shared = crate::data::catalog_wire::local_refusal_line(
                    venue,
                    &CatalogRefusal::NeedsCredentials,
                );
                Some(format!("{shared} {}", credential_note(venue, own_keys)))
            }
            Self::NoBulkList { why } => Some(crate::data::catalog_wire::local_refusal_line(
                venue,
                &CatalogRefusal::NoBulkList { why: why.to_string() },
            )),
            Self::NotInThisBuild => Some(NOT_LINKED_NOTE.to_string()),
        }
    }
}

/// Why a press was (or would be) refused. `None` from [`refresh_block`] means "go".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshBlock {
    /// [`RefreshAvailability::refreshable`] is false.
    Unavailable(RefreshAvailability),
    /// A fetch for this venue is already running.
    InFlight,
    /// Inside [`REFRESH_COOLDOWN_MS`] of the last completed attempt.
    Cooldown { remaining_ms: i64 },
    /// The venue routes to a datahub and no datahub address is resolved — no backend is connected
    /// and `config.datahub_addr` is unset. The control stays on the row, disabled: this venue CAN
    /// be refreshed, just not from a desktop with nothing to ask.
    NoBackend,
}

impl RefreshBlock {
    /// The button's hover text.
    #[must_use]
    pub fn line(self, venue: &str) -> String {
        match self {
            Self::Unavailable(a) => a.note(venue).unwrap_or_else(|| "available".to_string()),
            Self::InFlight => "already fetching this venue".to_string(),
            // ⚠ NOT "just refreshed". Every completed attempt starts this cooldown — including a
            // failure and a refusal — so wording that claims a successful fetch is a sentence an
            // operator reads while retrying the one that did not work.
            Self::Cooldown { remaining_ms } => format!(
                "this venue was asked a moment ago — it may be asked again in {}s",
                remaining_ms.div_euclid(1000) + 1
            ),
            // The SAME sentence the Status cell carries — one spelling, in `catalog_wire`.
            Self::NoBackend => crate::data::catalog_wire::NO_BACKEND_NOTE.to_string(),
        }
    }
}

/// Where a venue's refresh state stands right now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum VenueRefreshState {
    /// Nothing has been attempted this session.
    #[default]
    Idle,
    /// A fetch is running; `since_ms` is when it started.
    InFlight { since_ms: i64 },
    /// An attempt finished at `at_ms` with this outcome.
    Done { at_ms: i64, outcome: RefreshOutcome },
}

/// One rendered row of the Instruments screen: everything the operator needs for one venue.
#[derive(Clone, Debug)]
pub struct VenueCatalogRow {
    pub venue: String,
    /// `None` ⇒ this binary links no provider for the venue, so its mode is unanswerable here.
    pub mode: Option<CatalogMode>,
    /// **The routing decision** — [`vike_catalog::catalog_source_for`] over this binary's linked
    /// provider set. `Some(Direct)` ⇒ fetch it here; `Some(ServerBacked)` ⇒ ask the backend's
    /// datahub; `None` ⇒ no route exists for anybody, and
    /// [`vike_catalog::catalog_availability`] says which of the two reasons applies.
    ///
    /// ⚠ It was written and read by nobody, and its old `provider.is_some()` derivation called
    /// alpaca/oanda/ctrader/ig/ibkr `ServerBacked` — a route that does not exist. It is now the
    /// field [`VenueCatalogRow::availability`] is computed from.
    pub source: Option<CatalogSource>,
    /// The resolved datahub address a `ServerBacked` press would dial, or `None` when none is
    /// resolved. Only [`CatalogSource::ServerBacked`] rows care; a `Direct` row ignores it.
    pub server: Option<String>,
    /// The cached stamp — when the list was last fetched and how many it holds. `None` ⇒ never.
    pub stamp: Option<VenueStamp>,
    /// **The SHIPPED baseline's row for this venue**, when one exists — a THIRD state beside
    /// "never fetched" and "fetched, measured N", and given its own field rather than folded into
    /// [`Self::stamp`] for the reason `vike_catalog::baseline`'s rule 2 gives: a `VenueStamp` has
    /// no provenance field, so a baseline wearing one renders as an operator's own refresh age.
    ///
    /// ⚠ **[`Self::stamp`] WINS wherever both exist**, and the row says which answered
    /// ([`Self::answer`]). Shipped data never overwrites operator data, even when the operator's
    /// own fetch measured ZERO — zero is a measured claim about a venue and nothing has measured
    /// the shipped one.
    pub baseline: Option<BaselineVenue>,
    /// **The operator's OWN CREDENTIALED list for this venue**, from `vike_catalog::LOCAL_FILE` —
    /// written by `vike-backend catalog refresh` on the box that holds the keys
    /// (`docs/decisions/0066` decision 9).
    ///
    /// Their data, so it outranks [`Self::baseline`] and is outranked by [`Self::stamp`]; the
    /// precedence is `vike_catalog::merge_sources`', spelled once so this row and the picker cannot
    /// disagree about which source is in force.
    pub local: Option<BaselineVenue>,
    /// **Whether THIS BOX holds credentials for this venue.** Changes no control and no route — it
    /// changes the SENTENCE a credentialed venue reads, from "what would arm it" to "it is armed".
    ///
    /// Set from `vike_gateway_panel::credentialed_venues` over the same account grid the
    /// Gateway destination renders, so the two cannot claim different things about one venue.
    pub own_keys: bool,
    pub state: VenueRefreshState,
}

impl VenueCatalogRow {
    /// **The ROUTE decides, uniformly**, and the local provider only refines a `Direct` answer.
    ///
    /// ⚠ A venue whose bridge this binary links but which the table routes NOWHERE reads its table
    /// reason rather than its provider's mode — deliberately, and
    /// [`vike_catalog::catalog_source_for`]'s own ⚠ argues it: for a credentialed venue the honest
    /// answer depends on credentials no table can see, and a control that silently returns
    /// `Ok(vec![])` is the failure 0062's decision 5 exists to prevent.
    #[must_use]
    pub fn availability(&self) -> RefreshAvailability {
        match self.source {
            Some(CatalogSource::Direct) => match self.mode {
                Some(CatalogMode::QueryBacked) => RefreshAvailability::QueryBacked,
                _ => RefreshAvailability::Direct,
            },
            Some(CatalogSource::ServerBacked) => RefreshAvailability::ServerBacked,
            None => match catalog_availability(&self.venue) {
                CatalogAvailability::Credentialed => {
                    RefreshAvailability::Credentialed { own_keys: self.own_keys }
                }
                CatalogAvailability::NoBulkList { why } => RefreshAvailability::NoBulkList { why },
                // `PublicBulk` with no route is unreachable through `catalog_source_for` (it
                // answers `Some` for every public venue); `UnknownVenue` is the fail-closed arm.
                // Either way: no control, and a note that does not claim a venue property.
                _ => RefreshAvailability::NotInThisBuild,
            },
        }
    }

    /// **WHICH SOURCE answered for this venue** — the fact decision 7's rule 3 requires a row to be
    /// able to state, so two sources can never silently disagree about which one is on screen.
    #[must_use]
    pub fn answer(&self) -> CatalogAnswer {
        catalog_answer(self.stamp.is_some(), self.local.is_some(), self.baseline.is_some())
    }

    /// How many instruments this venue contributes to the picker — from whichever source
    /// [`Self::answer`] says answered.
    ///
    /// ⚠ `0` for [`CatalogAnswer::Nothing`] is NOT a claim about the venue, and the screen must not
    /// render it as one: `crates/vike-app-core/src/ui/tool_views/instruments.rs` renders `—` on
    /// exactly that argument, because zero is a measured claim and nothing has measured one.
    #[must_use]
    pub fn count(&self) -> usize {
        match self.answer() {
            CatalogAnswer::OperatorFetch => self.stamp.as_ref().map_or(0, |s| s.count),
            CatalogAnswer::LocalCredentialed => {
                self.local.as_ref().map_or(0, |b| b.instruments.len())
            }
            CatalogAnswer::ShippedBaseline => {
                self.baseline.as_ref().map_or(0, |b| b.instruments.len())
            }
            CatalogAnswer::Nothing => 0,
        }
    }

    /// The [`BaselineVenue`] whichever LIST source answered, or `None` when the cache answered (or
    /// nothing did). What the screen renders a date and a qualifier from.
    #[must_use]
    pub fn answering_list(&self) -> Option<&BaselineVenue> {
        match self.answer() {
            CatalogAnswer::LocalCredentialed => self.local.as_ref(),
            CatalogAnswer::ShippedBaseline => self.baseline.as_ref(),
            _ => None,
        }
    }
}

/// **May this venue be refreshed right now?** `None` = yes. The ONE decision site: the screen
/// disables the button on it and [`CatalogRefresh::request`] refuses on it, so a second caller
/// cannot route around the budget.
///
/// The order is deliberate. Availability first, because a venue that can never be refreshed has no
/// budget question to ask; then the budget, because an in-flight or just-asked venue is refused
/// whichever route it takes; then [`RefreshBlock::NoBackend`] last, so an idle `ServerBacked` row
/// with no datahub resolved reads the sentence that names the fix.
#[must_use]
pub fn refresh_block(row: &VenueCatalogRow, now_ms: i64) -> Option<RefreshBlock> {
    let availability = row.availability();
    if !availability.refreshable() {
        return Some(RefreshBlock::Unavailable(availability));
    }
    match &row.state {
        VenueRefreshState::InFlight { .. } => return Some(RefreshBlock::InFlight),
        VenueRefreshState::Done { at_ms, .. } => {
            let elapsed = now_ms.saturating_sub(*at_ms);
            if elapsed < REFRESH_COOLDOWN_MS {
                return Some(RefreshBlock::Cooldown {
                    remaining_ms: REFRESH_COOLDOWN_MS - elapsed,
                });
            }
        }
        VenueRefreshState::Idle => {}
    }
    if availability == RefreshAvailability::ServerBacked && row.server.is_none() {
        return Some(RefreshBlock::NoBackend);
    }
    None
}

/// "never" / "just now" / "6 min ago" / "3 h ago" / "2 d ago" — the stamp in the only terms that
/// answer *did my press do anything*. Pure, so it is unit-tested rather than eyeballed.
#[must_use]
pub fn refreshed_label(stamp: Option<&VenueStamp>, now_ms: i64) -> String {
    let Some(stamp) = stamp else { return "never".to_string() };
    let secs = now_ms.saturating_sub(stamp.last_refreshed_ms).div_euclid(1000).max(0);
    match secs {
        0..=9 => "just now".to_string(),
        10..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}
