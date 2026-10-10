//! The history table's vocabulary: door classes, kinds, lanes, depths, limits, evidence and the row.

/// What KIND of door a channel is — see this module's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelClass {
    /// The venue's own query API: recent or deep depending on the parameters.
    Request,
    /// A store the venue itself offers: files in a bucket, not a query.
    Bulk,
    /// A third party that republishes the history.
    Vendor,
}

/// What a channel serves, in the vocabulary of the hist store's `kind=` layouts and of
/// `crates/vike-backfill/src/caps.rs` (`bar`, `quote`, `trade`, `book`) plus the perpetual funding
/// series, which the datahub serves as a lane of its own.
///
/// ⚠ There is no `Ticks` variant, though the design spelt one: the store keeps a quote tick and a
/// trade print as different kinds, and a row that cannot say which is a row that has not been
/// read closely enough. A tick feed is [`HistoryKind::Quotes`], [`HistoryKind::Trades`] or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// OHLC bars.
    Bars,
    /// Bid/ask quote ticks.
    Quotes,
    /// Trade prints.
    Trades,
    /// Order-book updates.
    Book,
    /// The perpetual funding-rate series.
    Funding,
}

/// A datahub collector lane, spelt as `crates/vike-datahub/src/backfill.rs`'s `BackfillLane`
/// spells its variants. The gate holds the two sets of names equal, so this enum cannot grow a
/// lane the datahub lacks nor lose one it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLane {
    /// The venue's own OHLCV bars, over a public endpoint.
    Klines,
    /// Bars resampled from the venue's ticks, which are stored as quotes first.
    TickBars,
    /// The market funding-rate series.
    Funding,
    /// The venue's own OHLCV bars through a source that needs a CREDENTIAL the operator stored on
    /// the datahub's box. The one row is OANDA's; the lane answers a `Backfill` request and no
    /// chart open can start it. Every other lane is keyless, and
    /// `a_lane_needs_a_credential_exactly_when_its_row_says_so` holds that both ways.
    CredentialedKlines,
}

/// What vike can DO with a channel today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    /// A datahub lane fetches it — the lane is named, and the gate checks it exists.
    Built(HistoryLane),
    /// Nothing in vike fetches it. The reason says what it waits on, or that nothing is designed
    /// at all — a scaffolded venue's row and a finding that no channel exists both land here.
    Designed(&'static str),
}

/// One bar-size class with its own rolling window — [`HistoryDepth::LookbackByStep`]'s rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepLookback {
    /// Which bar sizes the window applies to, in words ("bars of 30 seconds or less").
    pub bars: &'static str,
    /// How many days back those bars are served. When the vendor states the window in another
    /// unit (IBKR says six months) this is the calendar conversion, and the row's note says so.
    pub days: u32,
}

/// How far back a channel goes. See this module's doc for why [`HistoryDepth::Unstated`] exists
/// and what it must never be rendered as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDepth {
    /// History starts on this UTC date (`YYYY-MM-DD`), for what `scope` names — a floor without
    /// its scope is a lie by omission, because the same channel usually starts later for
    /// everything the measurement did not cover.
    Since { date: &'static str, scope: &'static str },
    /// A rolling window: only the last `days` days are served, so the oldest date a caller can ask
    /// for moves every day. Stored as days and resolved to a date at RENDER time.
    Lookback { days: u32 },
    /// A rolling window that depends on the bar size: each of `steps` limits its own class of
    /// bars, and `otherwise` is the depth of every other bar size.
    LookbackByStep { steps: &'static [StepLookback], otherwise: &'static HistoryDepth },
    /// To the first data of THAT instrument, which differs per instrument; `probe` says how to
    /// ask for it.
    PerInstrument { probe: &'static str },
    /// The sources read say nothing. **Never "unlimited".**
    Unstated,
}

/// How much one request carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerRequest {
    /// At most this many rows (candles, ticks) per request.
    Rows(u32),
    /// One request may span at most this many days.
    Span { days: u32 },
    /// One request fetches one file, described here ("one instrument-hour").
    File(&'static str),
    /// A rule too involved for one number; the text says where to read it.
    SeeVendor(&'static str),
    /// Not stated by the sources read, or not read at all (the row's evidence says which).
    Unstated,
}

/// How fast a channel may be pulled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// The limit as stated or measured, with its numbers in the text.
    Stated(&'static str),
    /// Not stated by the sources read, or not read at all (the row's evidence says which).
    Unstated,
}

/// What it takes to use a channel. Words, never a credential name or value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No credential of any kind.
    Keyless,
    /// A credential of the user's own — what kind, in words.
    Credential(&'static str),
    /// A running session with a broker gateway — what kind, in words.
    Session(&'static str),
    /// It costs money: what is paid for (`unit`) and anything else a buyer needs (`note`).
    Paid { unit: &'static str, note: &'static str },
    /// Not stated by the sources read, or not read at all (the row's evidence says which).
    Unstated,
}

/// One source of a row's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    /// A vendor page, read on `checked` (`YYYY-MM-DD`).
    Documented { url: &'static str, checked: &'static str },
    /// Observed live on `on` (`YYYY-MM-DD`) by `by` — what did the observing.
    Measured { on: &'static str, by: &'static str },
    /// A THIRD PARTY states it and the vendor does not; read on `checked`. `by` names who.
    Reported { by: &'static str, url: &'static str, checked: &'static str },
}

/// Where a row's limits were read — see this module's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEvidence {
    /// Nobody has read a source or measured anything for this row's limits.
    Unmeasured,
    /// One or more sources, each dated. Never empty.
    Sourced(&'static [EvidenceSource]),
}

/// One `(venue, channel)` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryChannel {
    /// What kind of door this is.
    pub class: ChannelClass,
    /// The channel's own name — unique within its venue. The vendor's word where there is one.
    pub name: &'static str,
    /// What it serves. Empty for a finding that no such channel exists and for an unclassified
    /// venue; [`HistoryChannel::is_absent`] and [`HistoryChannel::is_unclassified`] tell them apart.
    pub kinds: &'static [HistoryKind],
    /// How far back it goes.
    pub depth: HistoryDepth,
    /// How much one request carries.
    pub per_request: PerRequest,
    /// How fast it may be pulled.
    pub pace: Pace,
    /// What it takes to use it.
    pub access: Access,
    /// What vike can do with it today.
    pub state: ChannelState,
    /// Where the limits above were read.
    pub evidence: HistoryEvidence,
    /// What the cells cannot say — quirks, scope, caveats. Never restates a cell, and never a
    /// credential. Empty when there is nothing to add.
    pub note: &'static str,
}

impl HistoryChannel {
    /// The row for a venue nobody has looked at: the scaffolded row, and the honest answer for
    /// every roster venue whose history was not read. Every cell that needs a source is blank and
    /// says so; `why` is the one line saying what is known (and, for a scaffold, what is not).
    pub(super) const fn unclassified(why: &'static str) -> Self {
        Self {
            class: ChannelClass::Request,
            name: "history, not classified",
            kinds: &[],
            depth: HistoryDepth::Unstated,
            per_request: PerRequest::Unstated,
            pace: Pace::Unstated,
            access: Access::Unstated,
            state: ChannelState::Designed(why),
            evidence: HistoryEvidence::Unmeasured,
            note: "",
        }
    }

    /// A FINDING that no such channel exists in what was read. `evidence` is required to be
    /// `Sourced` by `an_absent_row_is_a_finding_with_a_source`, and `note` scopes the claim.
    pub(super) const fn none_found(
        class: ChannelClass,
        name: &'static str,
        evidence: &'static [EvidenceSource],
        note: &'static str,
    ) -> Self {
        Self {
            class,
            name,
            kinds: &[],
            depth: HistoryDepth::Unstated,
            per_request: PerRequest::Unstated,
            pace: Pace::Unstated,
            access: Access::Unstated,
            state: ChannelState::Designed("nothing to build: no such channel was found"),
            evidence: HistoryEvidence::Sourced(evidence),
            note,
        }
    }

    /// A request channel the datahub serves through `lane` over a public endpoint, whose depth and
    /// limits nobody has read or measured — the shape of every venue-direct collector today.
    pub(super) const fn built_keyless(
        name: &'static str,
        kinds: &'static [HistoryKind],
        lane: HistoryLane,
    ) -> Self {
        Self {
            class: ChannelClass::Request,
            name,
            kinds,
            depth: HistoryDepth::Unstated,
            per_request: PerRequest::Unstated,
            pace: Pace::Unstated,
            access: Access::Keyless,
            state: ChannelState::Built(lane),
            evidence: HistoryEvidence::Unmeasured,
            note: "",
        }
    }

    /// Is this a FINDING that no such channel exists? See this module's doc.
    #[must_use]
    pub fn is_absent(&self) -> bool {
        self.kinds.is_empty() && matches!(self.evidence, HistoryEvidence::Sourced(_))
    }

    /// Is this a BLANK — nobody has classified the channel at all? See this module's doc.
    #[must_use]
    pub fn is_unclassified(&self) -> bool {
        self.kinds.is_empty() && matches!(self.evidence, HistoryEvidence::Unmeasured)
    }
}
