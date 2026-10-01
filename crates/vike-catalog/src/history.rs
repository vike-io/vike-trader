//! `history` — **through which DOORS each venue's history can be fetched, how far back each door
//! goes, what it costs and limits, and where each of those answers was read from.**
//!
//! The owner's question of 2026-09-30: *how does an end user know how many days he can get — from
//! a normal request or from a bulk download — before he spends an hour asking?* Nothing in this
//! workspace declared it. `crates/vike-backfill/src/caps.rs` says which KINDS a source serves per
//! venue and says nothing about depth (its own words: a `true` says nothing about whether a window
//! has data); the `intervals` table beside this one says which bar intervals a pair serves and has
//! no depth axis either. This is that axis, and it is the playbook's STEP 1 for a third table
//! (`docs/superpowers/specs/2026-09-30-history-channels-design.md`, Part A): **one row per
//! (venue, channel), each citing where it was read from, a verbatim pin, a completeness test over
//! [`vike_model::VENUES`] and a `vike:new-venue:row` marker.**
//!
//! # What a CHANNEL is
//!
//! A way to get history, of three classes ([`ChannelClass`]). A **request** is the venue's own
//! query API, recent or deep depending on the parameters. A **bulk** channel is a store the venue
//! itself offers — files in a bucket, not a query. A **vendor** channel is a third party that
//! republishes the history (Databento, Tardis, data.vike.io). "Archive versus recent" is therefore
//! not a switch: it is *which channels a venue has, and what each costs and reaches*, and one
//! venue can have all three.
//!
//! # Nothing here behaves differently
//!
//! STEP 1 merges byte-identical: no lane, no fetch and no refusal reads this table. Its one
//! consumer is `vike-cli data source show`, which renders a roster venue's rows — a rolling window
//! resolved to a DATE at render time, which is why [`HistoryDepth::Lookback`] stores days and no
//! date — and reaches nothing. What is STEP 2 and is NOT done here: a datahub verb carrying the
//! rows plus this server's overlay, a depth column in the GUI, and `Backfill` refusing a window a
//! row proves impossible, one venue at a time.
//!
//! `Built` is held equal to the datahub's collector table by
//! `crates/vike-ops/tests/history_channels_gate.rs`, a text scan of
//! `crates/vike-datahub/src/backfill.rs` in BOTH directions: a row cannot claim a lane that does
//! not exist, and a lane cannot exist without a row that claims it. [`HistoryLane`] is that file's
//! `BackfillLane` as this crate names it — a lower crate cannot name the higher one's type, and the
//! gate holds the two rosters equal rather than either crate re-exporting the other's.
//!
//! # ⚠ Three-valued on purpose, as `intervals` is
//!
//! Collapsing any of these to a `bool` or to "none" is a bug, in whichever direction it is
//! collapsed, and the types exist to make that a deliberate choice:
//!
//! * **[`HistoryDepth::Unstated`] means the sources read say nothing about how far back. It is
//!   NEVER rendered as "unlimited"** — the vendor's silence is not a promise, and a caller that
//!   reads it as one asks for an hour of data that does not exist.
//! * **[`HistoryEvidence::Unmeasured`] means nobody looked.** Every cell that would need a source
//!   (depth, per-request size, pace) is then `Unstated` and rendered "not known", which is a
//!   weaker statement than "not stated": the first says nobody read a page, the second says a
//!   page was read and is silent. The one exception is [`PerRequest::File`], the unit this
//!   workspace's own collector asks an archive for: a layout read off that code, not a limit.
//! * **A row with no kinds is one of two different things.** With a written source it is a
//!   FINDING — "no such channel was found in what was read", the shape of `oanda`'s and `ibkr`'s
//!   archive rows, scoped by its own note (an API reference lists no bulk download; that is not a
//!   claim about every product the vendor sells). With no source it is a BLANK — "not
//!   classified", the shape of a scaffolded venue. [`HistoryChannel::is_absent`] and
//!   [`HistoryChannel::is_unclassified`] are the two answers.
//!
//! # Evidence
//!
//! [`HistoryEvidence`] describes where a row's LIMITS — depth, per-request size, pace — were read.
//! What a row serves, what it needs and what vike does with it are read off this workspace's own
//! code and held by gates, not by this field. Four classes, weakest last: **Documented** (a vendor
//! page, with its URL and the day it was read), **Measured** (observed live, with the day and what
//! observed), **Reported** (a third party states it and the vendor does not — added because a
//! third-party instrument table is neither of the first two, and filing it as Documented would
//! overstate it) and **Unmeasured**. A row may carry several sources: a vendor's limits and its
//! measured depth are different pages, and IBKR's are four.
//!
//! ⚠ **Every date here is when a maintainer read or measured, never when this binary ran.**
//! `vike-cli data source show` says so in its own output, and nothing in this module is a probe.
//!
//! # What this table is NOT
//!
//! It is not `caps.rs` (kinds per source), not `intervals` (which bar sizes a pair serves) and not
//! the catalog (which instruments exist) — those answer neighbouring questions and one rendered
//! view is meant to join them. It carries no credential NAME and no value: an [`Access`] cell says
//! in words what kind of credential a channel needs, and which key holds it is the bridge's own
//! declaration. And it is code, not settings: vendor limits are facts with evidence, reviewed in
//! PRs, pinned verbatim and identical on every box, which a settings-database row can be none of
//! (`docs/decisions/0086-settings-live-only-in-the-database.md` puts SETTINGS there).

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
    const fn unclassified(why: &'static str) -> Self {
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
    const fn none_found(
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
    const fn built_keyless(
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

// The kind sets, named so a row reads as a declaration rather than a literal.
const BARS: &[HistoryKind] = &[HistoryKind::Bars];
const QUOTES: &[HistoryKind] = &[HistoryKind::Quotes];
const FUNDING_RATES: &[HistoryKind] = &[HistoryKind::Funding];
const QUOTES_AND_TRADES: &[HistoryKind] = &[HistoryKind::Quotes, HistoryKind::Trades];
const BOOK_TRADES_QUOTES: &[HistoryKind] =
    &[HistoryKind::Book, HistoryKind::Trades, HistoryKind::Quotes];

/// Nothing is declared — what an unknown venue string gets. Not the same as "none exist".
const NOT_DECLARED: &[HistoryChannel] = &[];

/// A venue nobody has looked at, and the row `just new-venue` renders an arm pointing at. It is a
/// real arm's row too (cTrader and Alpaca, which no datahub lane serves and whose vendors were not
/// read), so the scaffold's answer is a tested one rather than a constant only its template
/// mentions — and the tag the scaffold's marker carries is what
/// `no_scaffold_placeholder_survives_into_main` refuses until a human replaces the arm with what
/// was read.
const UNCLASSIFIED: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "no datahub lane serves this venue's history and nobody has read the vendor's history limits",
)];

// ---------------------------------------------------------------------------------------------
// The venues with a datahub lane today. Every row is `Built` over a public endpoint and every
// limit is blank, because no vendor page was read for it and nothing was measured: the lane exists
// (the gate holds that), how far back it reaches is a later row-by-row PR's question.
// ---------------------------------------------------------------------------------------------

const BINANCE: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("klines (spot and futures)", BARS, HistoryLane::Klines),
    HistoryChannel::built_keyless("funding-rate history", FUNDING_RATES, HistoryLane::Funding),
];

const BYBIT: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("kline history", BARS, HistoryLane::Klines)];

const OKX: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("history-candles", BARS, HistoryLane::Klines)];

const ASTER: &[HistoryChannel] =
    &[HistoryChannel::built_keyless("klines", BARS, HistoryLane::Klines)];

const DERIBIT: &[HistoryChannel] = &[HistoryChannel::built_keyless(
    "chart data (get_tradingview_chart_data)",
    BARS,
    HistoryLane::Klines,
)];

const HYPERLIQUID: &[HistoryChannel] = &[
    HistoryChannel::built_keyless("candleSnapshot candles", BARS, HistoryLane::Klines),
    HistoryChannel::built_keyless("funding history", FUNDING_RATES, HistoryLane::Funding),
];

// ---------------------------------------------------------------------------------------------
// Dukascopy — the venue with all three classes. Read 2026-09-30 from the vendor's own pages.
// ---------------------------------------------------------------------------------------------

/// The vendor's data-export page: the S3 archive, its layout and its prices.
const DUKASCOPY_DATA_EXPORT: &str = "https://www.dukascopy.com/wiki/en/development/data-export/";

/// The third-party instrument table the depth of the HTTP feeds is reported from.
const DUKASCOPY_NODE_TABLE: &str = "https://raw.githubusercontent.com/Leo4815162342/dukascopy-node/\
                                    master/src/utils/instrument-meta-data/generated/\
                                    instrument-meta-data.json";

const DUKASCOPY_HTTP_EVIDENCE: &[EvidenceSource] = &[
    EvidenceSource::Measured {
        on: "2026-09-30",
        by: "requests for hour files from a development machine (HTTP 429 with a body that points \
             at the vendor's data-export page, then 503 \"No server is available\"), a CI runner's \
             log (503 on most attempts), and the retry study in the Dukascopy bridge's own \
             documentation (bursts of instant 503s, resets and 30 s timeouts; a 24-hour chunk got \
             through on one attempt in six)",
    },
    EvidenceSource::Reported {
        by: "the dukascopy-node instrument table, a downloader's metadata",
        url: DUKASCOPY_NODE_TABLE,
        checked: "2026-09-30",
    },
];

const DUKASCOPY: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "HTTP datafeed, one .bi5 file per instrument-hour",
        kinds: QUOTES,
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: PerRequest::File("one instrument-hour"),
        pace: Pace::Stated(
            "throttled, and the vendor points bulk users at its S3 archive instead: the feed \
             answers HTTP 429 (Too Many Requests) or 503 (No server is available) in bursts that \
             last seconds to minutes, so a long pull needs patient retries",
        ),
        access: Access::Keyless,
        state: ChannelState::Built(HistoryLane::TickBars),
        evidence: HistoryEvidence::Sourced(DUKASCOPY_HTTP_EVIDENCE),
        note: "Ticks carry bid, ask and per-side volumes; vike stores them as quotes and derives \
               bars from them by resampling. The Dukascopy code in this workspace says in its module \
               doc that history reaches back to about 2003 and lags by a day, a claim nothing here \
               has measured. A third-party instrument table (not a vendor statement) lists a \
               first-tick date per instrument: 2003-05-04 for EUR/USD, USD/JPY and GBP/USD, and \
               2003-08-03 for AUD/USD.",
    },
    HistoryChannel {
        class: ChannelClass::Bulk,
        name: "S3 bulk archive (requester pays)",
        kinds: QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one instrument-day"),
        pace: Pace::Stated("the vendor's page recommends 20 to 30 concurrent requests"),
        access: Access::Paid {
            unit: "requester pays — $0.0004 per 1,000 GET requests plus $0.02 per GB transferred \
                   (eu-west-1 rates, the vendor page's figures)",
            note: "The downloader needs their own AWS credentials and the eu-west-1 region. The \
                   page's own figures: EUR/USD is 2.6 GB in 26,586 objects, about $0.06, and the \
                   whole archive about 400 GB in 20 million objects, about $16.",
        },
        state: ChannelState::Designed(
            "nothing in vike fetches or imports it; a user can download it with their own AWS \
             account",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: DUKASCOPY_DATA_EXPORT,
            checked: "2026-09-30",
        }]),
        note: "Daily files named SYMBOL/YEAR/MONTH/DAY_ticks.bi5 in the bucket \
               cfg-public-proper-wallaby, with a zero-based month and 20-byte big-endian records \
               whose time is milliseconds since the start of the day. The page warns that older \
               files may be hourly files whose time is milliseconds since the start of the hour, \
               and that neither of its decoders detects it; its own EUR/USD object count is well \
               above one file per day since 2003, which fits that warning (our inference, not the \
               page's statement). The page states no depth, so none is claimed here.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "HTTP candle files",
        kinds: BARS,
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's getTimeOfFirstCandle (the JForex row below); no probe is wired \
                    for this feed itself",
        },
        per_request: PerRequest::Unstated,
        pace: Pace::Unstated,
        access: Access::Unstated,
        state: ChannelState::Designed(
            "the channel itself is unverified, because the vendor's pages do not mention it and \
             the feed's throttling blocked a check",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Reported {
            by: "the dukascopy-node instrument table, a downloader's metadata",
            url: DUKASCOPY_NODE_TABLE,
            checked: "2026-09-30",
        }]),
        note: "Third-party downloaders report candle series beside the tick files. The same table \
               lists candle starts per instrument and period: for EUR/USD, minute candles from \
               2003-05-04 and daily candles from 1973-03-01. Reported, never confirmed by the \
               vendor or by a request of ours.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "JForex history service",
        kinds: &[HistoryKind::Bars, HistoryKind::Quotes],
        depth: HistoryDepth::PerInstrument {
            probe: "JForex's IDataService getTimeOfFirstCandle, per instrument and period, ticks \
                    included",
        },
        per_request: PerRequest::Unstated,
        pace: Pace::Unstated,
        access: Access::Session(
            "a JForex account login: the history service runs inside a session",
        ),
        state: ChannelState::Designed("the JForex sidecar is not wired for history"),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: "https://www.dukascopy.com/wiki/en/development/strategy-api/historical-data/\
                  historical-data-service/",
            checked: "2026-09-30",
        }]),
        note: "Bars from ten seconds to monthly, and ticks. The service answers how far back the \
               data goes for each instrument and period; the page read gives no depth, request \
               size or pace of its own.",
    },
];

// ---------------------------------------------------------------------------------------------
// OANDA — one channel, deep and recent at once. Read 2026-09-30 from the vendor's docs; the depth
// was also measured by a 5-second-candle probe (an example program that is not on main).
// ---------------------------------------------------------------------------------------------

const OANDA_INTRODUCTION: &str = "https://developer.oanda.com/rest-live-v20/introduction/";

const OANDA: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "v20 REST candles",
        kinds: BARS,
        depth: HistoryDepth::Since {
            date: "2005-01-03",
            scope: "5-second candles of the majors the probe covered; the vendor's page says only \
                    that history dates back to 2005",
        },
        per_request: PerRequest::Rows(5000),
        pace: Pace::Stated(
            "120 requests per second per IP address (an excess request answers HTTP 429) and at \
             most 2 new connections per second, as OANDA documents; our probe ran clean at up \
             to 25 requests per second",
        ),
        access: Access::Credential(
            "a practice-tier OANDA API token stored on the datahub's own box, which the datahub \
             reads when a Backfill request arrives; this platform reaches only the practice tier",
        ),
        state: ChannelState::Built(HistoryLane::CredentialedKlines),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: OANDA_INTRODUCTION, checked: "2026-09-30" },
            EvidenceSource::Documented {
                url: "https://developer.oanda.com/rest-live-v20/pricing-ep/",
                checked: "2026-09-30",
            },
            EvidenceSource::Documented {
                url: "https://developer.oanda.com/rest-live-v20/development-guide/",
                checked: "2026-09-30",
            },
            EvidenceSource::Measured {
                on: "2026-09-30",
                by: "a 5-second-candle probe run against OANDA's practice endpoint (an example \
                     program on the probe/oanda-s5 branch, not on main): the earliest candle per \
                     instrument, a 5000-candle page ceiling, and a request burst with no 429",
            },
        ]),
        note: "Candles only, no ticks: 5-second candles are the finest history OANDA serves, in \
               granularities from S5 to monthly and as mid, bid or ask prices; the datahub's lane \
               stores mid only. Only the 5-second depth was measured; coarser candles and other \
               instruments were not. The lane answers a Backfill request and is never started by \
               opening a chart. It fetches a whole UTC day at a time and refuses two requests \
               before fetching anything: a symbol that is not OANDA's own instrument name \
               (EUR_USD, not EURUSD) and a window in seconds that starts before 2005-01-03. One \
               pair's whole 5-second history is one long request that prints nothing until it \
               ends, estimated at one to two hours from the probe's figures and not yet measured \
               on the lane.",
    },
    HistoryChannel::none_found(
        ChannelClass::Bulk,
        "bulk archive",
        &[EvidenceSource::Documented { url: OANDA_INTRODUCTION, checked: "2026-09-30" }],
        "OANDA's v20 API reference offers history only as candle requests and lists no bulk \
         download. No other OANDA history product was looked for.",
    ),
];

// ---------------------------------------------------------------------------------------------
// IBKR — a request API with hard per-bar-size limits and no bulk channel. Read 2026-09-30.
// ---------------------------------------------------------------------------------------------

/// The TWS API pages IBKR's rows read from: the index of the historical data it offers (which also
/// states the market-data subscription requirement), the limitations, the bars, the earliest-date
/// probe and the ticks. Each is marked deprecated by IBKR, which points at its Campus
/// documentation; the Campus site refuses an automated read, so these stay the citation.
const IBKR_HISTORICAL_DATA: &str =
    "https://interactivebrokers.github.io/tws-api/historical_data.html";
const IBKR_LIMITATIONS: &str =
    "https://interactivebrokers.github.io/tws-api/historical_limitations.html";
const IBKR_BARS: &str = "https://interactivebrokers.github.io/tws-api/historical_bars.html";
const IBKR_HEAD_TIMESTAMP: &str =
    "https://interactivebrokers.github.io/tws-api/head_timestamp.html";
const IBKR_TICKS: &str =
    "https://interactivebrokers.github.io/tws-api/historical_time_and_sales.html";

const IBKR_SESSION: &str = "a running TWS or IB Gateway session; the API needs the same \
                            market-data subscription as live top-of-book data";

const IBKR_BAR_DEPTH: HistoryDepth = HistoryDepth::LookbackByStep {
    steps: &[StepLookback { bars: "bars of 30 seconds or less", days: 183 }],
    otherwise: &HistoryDepth::PerInstrument {
        probe: "reqHeadTimestamp, per instrument and data type",
    },
};

const IBKR: &[HistoryChannel] = &[
    HistoryChannel {
        class: ChannelClass::Request,
        name: "reqHistoricalData bars",
        kinds: BARS,
        depth: IBKR_BAR_DEPTH,
        per_request: PerRequest::SeeVendor(
            "a request's duration must fit its bar size (IBKR's step-size table): see IBKR docs",
        ),
        pace: Pace::Stated(
            "for bars of 30 seconds or less: at most 60 requests in any 10 minutes, no identical \
             request within 15 seconds, and six or more requests for one contract within 2 \
             seconds is a violation (a BID_ASK request counts twice); at most 50 requests open at \
             once. IBKR says the limits for 1-minute and larger bars have been lifted, though \
             soft throttling remains",
        ),
        access: Access::Session(IBKR_SESSION),
        state: ChannelState::Designed(
            "the bridge has a historical fetcher behind its ibkr-socket feature, but no datahub \
             lane reaches it, and a lane that needs a gateway session is the case decision 0094 \
             leaves reopened",
        ),
        evidence: HistoryEvidence::Sourced(&[
            EvidenceSource::Documented { url: IBKR_LIMITATIONS, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_BARS, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_HEAD_TIMESTAMP, checked: "2026-09-30" },
            EvidenceSource::Documented { url: IBKR_HISTORICAL_DATA, checked: "2026-09-30" },
        ]),
        note: "Bars from 1 second to 1 month; forex bars are MIDPOINT, BID, ASK or BID_ASK only. \
               IBKR states the small-bar window as six months and this table converts it to 183 \
               days. Expired futures are served for two years after expiry, expired options not \
               at all, and instruments that no longer trade not at all. The pages read are marked \
               deprecated by IBKR.",
    },
    HistoryChannel {
        class: ChannelClass::Request,
        name: "reqHistoricalTicks ticks",
        kinds: QUOTES_AND_TRADES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::Rows(1000),
        pace: Pace::Unstated,
        access: Access::Session(IBKR_SESSION),
        state: ChannelState::Designed(
            "no code fetches historical ticks: the bridge's fetcher does bars only",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Documented {
            url: IBKR_TICKS,
            checked: "2026-09-30",
        }]),
        note: "Trade, bid/ask and midpoint ticks; which of them an instrument has is not stated on \
               the page. A request never spans two trading sessions, and more ticks than asked \
               for may come back to complete a whole second. The pacing rules on IBKR's limits \
               page are written for small bars, so none is claimed for ticks.",
    },
    HistoryChannel::none_found(
        ChannelClass::Bulk,
        "bulk archive",
        &[EvidenceSource::Documented { url: IBKR_HISTORICAL_DATA, checked: "2026-09-30" }],
        "The TWS API's own list of the historical data it offers is bars, histograms, time and \
         sales, and the earliest-date probe — all requests. No other IBKR history product was \
         looked for.",
    ),
];

// ---------------------------------------------------------------------------------------------
// Polymarket — no request channel in the bridge, and two archives, one of which has stopped.
// ---------------------------------------------------------------------------------------------

const POLYMARKET: &[HistoryChannel] = &[
    HistoryChannel::unclassified(
        "the bridge implements no price-history request, and the venue's own API was not read \
         for this table",
    ),
    HistoryChannel {
        class: ChannelClass::Vendor,
        name: "data.vike.io archive",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one UTC day of one stream"),
        pace: Pace::Unstated,
        access: Access::Credential(
            "a data.vike.io API key: the archive is keyed and its manifest of available dates is \
             not",
        ),
        state: ChannelState::Designed(
            "a vike-backfill program pulls it, but no datahub lane and no fetch --source value \
             reaches it",
        ),
        evidence: HistoryEvidence::Unmeasured,
        note: "Full order-book updates, the taker trade tape and derived top-of-book quotes, one \
               UTC day per partition.",
    },
    HistoryChannel {
        class: ChannelClass::Vendor,
        name: "pmxt archive (stopped publishing)",
        kinds: BOOK_TRADES_QUOTES,
        depth: HistoryDepth::Unstated,
        per_request: PerRequest::File("one UTC hour"),
        pace: Pace::Unstated,
        access: Access::Unstated,
        state: ChannelState::Designed(
            "the archive stopped publishing after 2026-08-10 and the owner ruled it obsolete on \
             2026-09-21; its collector is kept only as the reference for the archive's Parquet \
             schema",
        ),
        evidence: HistoryEvidence::Sourced(&[EvidenceSource::Measured {
            on: "2026-09-21",
            by: "requests to the real bucket, where every hour from 2026-08-10T01 on answered 404 \
                 while older objects still answered 200 (recorded in the module doc of the pmxt \
                 collector in vike-backfill)",
        }]),
        note: "Licensed CC BY 4.0 and unaffiliated with this project. History up to the stop is \
               real and already ingested where it was pulled.",
    },
];

// ---------------------------------------------------------------------------------------------
// The venues nobody has looked at. NAMED rows, because a named row is what proves a venue was
// classified rather than forgotten — and each says the one thing that is known.
// ---------------------------------------------------------------------------------------------

const IG: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "the bridge turns IG prices into bars over a logged-in session, but no datahub lane reaches \
     it and IG's history limits were not read",
)];

const FXCM: &[HistoryChannel] = &[HistoryChannel::unclassified(
    "the bridge has no data path at all, so nothing here fetches this venue's history, and the \
     vendor's history was not read",
)];

/// **The history channels `venue` declares.** A roster venue always has at least one NAMED row; an
/// unknown venue string gets an EMPTY slice, which says nothing is declared and is not the same as
/// "no channel exists".
///
/// Every row's evidence is its own; nothing is inferred from a sibling venue.
#[must_use]
pub fn history_channels_for(venue: &str) -> &'static [HistoryChannel] {
    match venue {
        // The six venues whose kline collector `crates/vike-datahub/src/backfill.rs` dispatches,
        // plus binance's and hyperliquid's funding lanes. Built, keyless, limits unread.
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        // Read 2026-09-30: one deep-and-recent request channel and a finding that there is no
        // archive. No lane yet — the credential seam is a separate decision.
        "oanda" => OANDA,
        // Named rows with nothing read; each row's reason says the one thing that is known.
        "ig" => IG,
        "fxcm" => FXCM,
        // Read 2026-09-30: three doors, one built over a throttled feed, one designed over the
        // vendor's own S3 bucket, one behind a JForex login.
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        // Read 2026-09-30: hard per-bar-size limits, a session rather than a token, no archive.
        "ibkr" => IBKR,
        "ctrader" => UNCLASSIFIED,
        "alpaca" => UNCLASSIFIED,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue has no history channel anybody has read.
        // vike:new-venue:row // Read the vendor's history documentation and the bridge's own collector, then replace
        // vike:new-venue:row // UNCLASSIFIED with one row per channel — each with the source its limits were read from
        // vike:new-venue:row // (or `Unmeasured` and blank cells, never a guess) — and pin every row in
        // vike:new-venue:row // `history_matrix_is_pinned`.
        // vike:new-venue:row "{venue}" => UNCLASSIFIED,
        _ => NOT_DECLARED,
    }
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
