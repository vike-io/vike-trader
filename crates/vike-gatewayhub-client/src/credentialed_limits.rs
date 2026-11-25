//! The stream limits of the CREDENTIALED venues: how many streams a venue's data session may hold, all
//! holders together, and how fast it may open them (decision 0193 verdict 6).
//!
//! # Why a table, and why it lives here
//!
//! A credentialed venue's limits are per IP, per login, per socket or per account, and the venue sees
//! ONE gatewayhub. Two daemons need the same number: the datahub's broker refuses the key past the
//! ceiling before it asks (so a strategy or the recorder is told the number, in a cap refusal the
//! tradehub does not restart on), and the gatewayhub counts it again where the streams are really
//! opened and spaces the opens. One number written twice rots; this crate is the one place below both
//! daemons that neither forbids.
//!
//! It is NOT in `vike-venues`, where the per-venue tables usually live: that crate is in the live
//! core's normal closure, so a row edit there fires the latency gate for a number the core never reads
//! (decision 0193 verdict 6).
//!
//! # The shape: a row per credentialed venue, `None` until its slice
//!
//! [`ROWS`] names EVERY credentialed venue, and its names ARE the credentialed-stream roster
//! ([`CREDENTIALED_VENUES`]: the gatewayhub's `Lane::CredentialedStream` row of
//! `crates/vike-gatewayhub/src/registry/mod.rs`'s `ROSTERS` and the datahub's credentialed arm both read it),
//! so a venue joins the credentialed streams only with a decided row. A row is the gatewayhub's claim that it
//! serves the venue's credentialed stream, so its arm (`crates/vike-gatewayhub/src/data/credentialed.rs`'s
//! `build_credentialed`) must exist first. A `None` row is a venue with no ceiling and no spacing: today's
//! behaviour, byte for byte. A row is written in the venue's own slice, behind its own demo measurement.
//!
//! **A `Some` row is the venue's whole guard, on both daemons.** The datahub's broker counts its ceiling
//! (`crates/vike-datahub/src/feeds/mod.rs`'s rule seven), and the gatewayhub's session guard reads the same
//! row for the one-live-session rule, the ceiling and the spacing
//! (`crates/vike-gatewayhub/src/data/session_guard.rs`'s `CredentialedStreamGuard::admit`, which returns the bridge's
//! client untouched for a `None`). A venue's slice writes a named `const` here and one `Some`, and no other
//! code. `min_open_spacing` may be zero for a venue whose subscribe opens no connection.
//!
//! The numbers are PROVISIONAL (decision 0193: "starting values until measured"): the repository
//! says of OANDA's stream-session cap only "UNMEASURED" (`crates/bridges/oanda/CLAUDE.md`).
//!
//! **Three more columns both daemons and the tradehub read**: whose login the stream rides ([`CredentialedStreamLogin`],
//! [`stream_login_for`]), whether its first price comes only with a new stream
//! ([`first_price_comes_with_a_new_stream`]) and why the tradehub gives back an idle admitted symbol's pump
//! ([`reaps_idle_admitted`], the reason the row's own cap). Each is a fact of the venue's own bridge, written in the row.

use std::time::Duration;

/// One venue's stream limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialedStreamLimits {
    /// The most venue STREAMS the venue's data session holds at once, for all holders together. A
    /// stream is a tick-lane key (venue, symbol, lane): a bar lane polls REST and is not counted.
    pub max_streams: usize,
    /// The shortest span between two stream opens of the venue, across sessions. A daemon restart
    /// opens every key at once, and the venue limits NEW connections per second.
    pub min_open_spacing: Duration,
}

/// OANDA's allowance of active streams per IP, pricing and transactions streams together.
const OANDA_ACTIVE_STREAMS: usize = 20;
/// The transactions streams the exec side holds beside the pricing streams: one per armed OANDA account.
/// Two covers the default demo account and one more; a box that arms a third lowers the room by one, and
/// the margin below is what absorbs it.
const OANDA_TRANSACTION_STREAMS: usize = 2;
/// Room for a stream that is being replaced (a redial) or still winding down after a release (a join
/// takes up to a heartbeat), so a swap of keys never meets the venue's cap.
const OANDA_MARGIN: usize = 2;

/// OANDA: 20 active streams per IP less the transactions streams of the armed accounts less a margin
/// (20 - 2 - 2 = 16 pricing streams), and the venue's 2 new connections per second per IP with room
/// (600 ms between opens, where 500 ms is the limit). Both numbers are the venue's own, from the owner's
/// research note (not re-measured here); the repository's own words for the cap are "UNMEASURED".
///
/// This is a ceiling on the strategy lane, the recorder and market data TOGETHER. Before decision 0193 the strategy
/// lane alone was allowed `STRATEGY_LANE_MAX_KEYS_PER_VENUE` (sixty-four) keys per venue, so a box that streams more
/// than sixteen OANDA symbols is refused the rest; a refused key is a cap refusal (the tradehub keeps the
/// venue up and drops THAT key's quotes, `crates/vike-tradehub/src/feeds/oanda.rs`'s `wire_oanda_feed`).
pub const OANDA: CredentialedStreamLimits = CredentialedStreamLimits {
    max_streams: OANDA_ACTIVE_STREAMS - OANDA_TRANSACTION_STREAMS - OANDA_MARGIN,
    min_open_spacing: Duration::from_millis(600),
};

// The pricing streams and the exec side's transactions streams fit the venue's allowance with room to spare.
const _: () = assert!(OANDA.max_streams + OANDA_TRANSACTION_STREAMS < OANDA_ACTIVE_STREAMS);

/// FXCM: PROVISIONAL, as every row here. Sixteen is also the bridge's own cap (its price desk, `vike_fxcm::feed`, reads
/// this row's `max_streams`, so the desk and the ceiling are one number): the pairs one poll of the
/// Offers table looks up, not streams at the venue. No spacing, because a subscribe here opens no connection (it adds a
/// pair to the list the running reconcile session reads, one request every two seconds whatever the number of pairs),
/// and a wait would only sleep the session's reader thread on each key a gatewayhub restart replays.
pub const FXCM: CredentialedStreamLimits =
    CredentialedStreamLimits { max_streams: 16, min_open_spacing: Duration::ZERO };

/// IG: sixteen quote keys for all holders together, 100 ms between two opens. ⚠ Both numbers are PROVISIONAL (decision
/// 0193 verdict 6): nothing here was measured.
///
/// **It is not a count of connections.** Every key of IG's market-data client is one subscription on its ONE login and
/// ONE Lightstreamer socket (`crates/bridges/ig/src/feed/link.rs`'s `LightstreamerLink`, decision 0202). A second connection of the
/// API key, which IG says may get the key suspended, could only come from a second client: the sharing row gives the
/// holders one, and this row turns on the session guard, which lets one live session stand.
///
/// **The real bound is IG's 40 subscriptions per connection** (the link's `MAX_SUBSCRIPTIONS`, which refuses the 41st
/// in words and never dials a second socket), and the candle keys share it: `CHART:{epic}:{scale}` is a subscription on
/// the same socket as `PRICE:{accountId}:{epic}`, not a REST poll, though this ceiling counts only quote keys. Sixteen
/// quote keys leave 24 places for candle lanes.
///
/// ⚠ **This widens the door.** `StrategyLaneQuote::with_lane_cap` reads this row, so the control door may hold 16 lanes for
/// epics that were not mounted, where it held 4 before decision 0193's IG slice. All holders count together, so a box
/// that mounts M epics has 16 - M door lanes, a key only the recorder still holds counts for up to a minute after the
/// strategies let it go, and a mounted epic past sixteen runs on its candles without quotes (the strategy lane alone
/// allowed 64 keys, which the link's 40 could not have carried anyway).
///
/// **What sixteen costs IG:** no lane reads REST (the link logs in once, and no lane seeds from `/prices`), reconcile's
/// reads are account-wide, and the updates are about 1 to 5 a second an epic (measured on the demo gateway; IG documents
/// no figure), so about 80 frames a second at worst on the one socket. What grows with the door is the facts lane: a
/// held position on a converted epic re-reads its rate once a minute, against the daemon's 12 lookups a minute. Past
/// 12 such positions at once a rate can age past its two minutes, and an OPENING order on that epic is denied
/// `quote-rate-stale` until it is read again; a reducing order never is.
///
/// The spacing is not IG's: a subscribe is a control line on the open socket, not a new connection. It only keeps the
/// guard's reaper out of a burst of subscribes.
pub const IG: CredentialedStreamLimits =
    CredentialedStreamLimits { max_streams: 16, min_open_spacing: Duration::from_millis(100) };

/// IBKR: PROVISIONAL, neither number is measured. Sixteen is the cap the bridge's own price stream holds its one data
/// client to (`vike_execution::MAX_EXTRA_SYMBOLS`, `crates/bridges/vike-ibkr/src/feed/price/mod.rs`'s `DataClientBusSpecIbkr`, whose
/// words are "the market-data line limit is UNMEASURED"), so the cap refusal is said here first. A subscribe opens no
/// connection on this venue (a `reqMarketData` on the gateway session's one data connection), so the spacing only keeps a
/// restart's burst of two messages a key well under the fifty a second IB's documentation allows a client.
pub const IBKR: CredentialedStreamLimits =
    CredentialedStreamLimits { max_streams: 16, min_open_spacing: Duration::from_millis(100) };

/// Dukascopy: PROVISIONAL, unmeasured. Eight is the bridge's guess (its quote bus, `vike_dukascopy::feed`, reads this
/// row's `max_streams`, and the sidecar's `TickLane.MAX_INSTRUMENTS` says the same; the vendor states no limit). No
/// spacing: a stream open is one `subscribe` line to a sidecar already logged in, and opens no connection.
pub const DUKASCOPY: CredentialedStreamLimits =
    CredentialedStreamLimits { max_streams: 8, min_open_spacing: Duration::ZERO };

/// The vendor's limit of non-historical requests per second on one connection (`https://help.ctrader.com/open-api/`,
/// "Rate limiting", read 2026-11-06). A symbol's subscribe is one such request.
const CTRADER_REQUESTS_PER_SECOND: usize = 50;

/// cTrader: thirty-two symbols, PROVISIONAL and the platform's own: neither the vendor's pages nor the proto state a
/// limit on subscribed symbols, and the bridge has no constant for one (`crates/bridges/ctrader/CLAUDE.md`, "The
/// data client counts its keys"). The number is the door's own bound on the pumps it keeps open for symbols it has
/// not admitted (`crates/vike-tradehub/src/venue_backend/gateway/trade_lease_registry.rs`'s `MAX_WATCHED`), so that an account can go on
/// trading any symbol. Before decision 0193 the strategy lane alone was allowed sixty-four keys; mounts, admitted
/// symbols and held lanes do not count under `MAX_WATCHED`, so this still refuses a box that holds more than
/// thirty-two distinct symbols at once.
///
/// No spacing: a subscribe is one frame on the one socket and opens no connection, and the actor's replay after
/// a reconnect writes them all at once whatever this row says, so what has to hold is that a whole ceiling's
/// burst fits the vendor's second (the assertion below).
///
/// ⚠ A BAR key is not counted, and on this venue it is no REST poll: it holds a spot and a trend-bar subscription
/// on the socket. No mount asks for one (`crates/vike-tradehub/src/feeds/ctrader.rs`'s `wire_ctrader_feed`), but a
/// mount's INPUT does (decision 0271, the tradehub's bar keeper): what that costs is
/// `crates/bridges/ctrader/CLAUDE.md`'s "The data client counts its keys: one wire stream however many hold it (decision 0193)".
pub const CTRADER: CredentialedStreamLimits =
    CredentialedStreamLimits { max_streams: 32, min_open_spacing: Duration::ZERO };

const _: () = assert!(CTRADER.max_streams < CTRADER_REQUESTS_PER_SECOND);

/// **Whose login a credentialed venue's stream rides at the gatewayhub** (`crates/vike-gatewayhub/src/data/credentialed.rs`'s
/// `build_credentialed`): what a stream needs running before it can price, and which account a credential change of the
/// venue reconnects (decision 0287).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialedStreamLogin {
    /// The venue's DEFAULT account at `demo` (`vike_secrets`' `AccountDirectory::data_feed_credentials`): the stream
    /// logs in as that account and no other.
    DefaultAccount,
    /// The session ANY running demo account's trade lane holds in the gatewayhub process (one per process: dukascopy's
    /// JForex sidecar, fxcm's reconcile session): the stream logs in as no account of its own.
    AnyDemoSession,
}

/// One credentialed venue's row. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialedVenueRow {
    /// The venue's `vike_model::VENUES` slug.
    pub venue: &'static str,
    /// Its stream limits; `None` = no ceiling and no spacing.
    pub limits: Option<CredentialedStreamLimits>,
    /// Whose login its stream rides.
    pub login: CredentialedStreamLogin,
    /// Whether its QUOTE stream gives its first price to a NEW stream only ([`first_price_comes_with_a_new_stream`]).
    pub first_price_with_a_new_stream: bool,
    /// Why the tradehub gives back the pump of an ADMITTED symbol the engine holds nothing in
    /// ([`reaps_idle_admitted`]); `None`: an admitted symbol's pump is permanent.
    pub reaps_idle_admitted: Option<&'static str>,
}

/// Every credentialed venue, its limits, its login and its first price. See the module doc.
///
/// The first-price column is each venue's reading of its own bridge: OANDA's on-connect snapshot and the bridge's
/// first-price read belong to the start of the stream's thread; IG's new subscription asks for `LS_snapshot=true` and an
/// open one says nothing until the next update; IB states a contract's current top of book to a new `reqMarketData` and
/// then ticks only on a change (`crates/bridges/vike-ibkr/CLAUDE.md`); cTrader's last price is the first
/// `ProtoOASpotEvent` after a subscribe, "even if market is closed" (the proto's words), and a joiner of an open
/// subscription is written nothing.
pub const ROWS: [CredentialedVenueRow; 7] = [
    CredentialedVenueRow {
        venue: "alpaca",
        limits: None,
        login: CredentialedStreamLogin::DefaultAccount,
        first_price_with_a_new_stream: false,
        reaps_idle_admitted: None,
    },
    CredentialedVenueRow {
        venue: "ctrader",
        limits: Some(CTRADER),
        login: CredentialedStreamLogin::DefaultAccount,
        first_price_with_a_new_stream: true,
        reaps_idle_admitted: None,
    },
    CredentialedVenueRow {
        venue: "ig",
        limits: Some(IG),
        login: CredentialedStreamLogin::DefaultAccount,
        first_price_with_a_new_stream: true,
        reaps_idle_admitted: None,
    },
    CredentialedVenueRow {
        venue: "oanda",
        limits: Some(OANDA),
        login: CredentialedStreamLogin::DefaultAccount,
        first_price_with_a_new_stream: true,
        reaps_idle_admitted: None,
    },
    CredentialedVenueRow {
        venue: "ibkr",
        limits: Some(IBKR),
        login: CredentialedStreamLogin::DefaultAccount,
        first_price_with_a_new_stream: true,
        reaps_idle_admitted: Some(
            "its price stream holds its one data client to sixteen market-data lines for the whole process (`IBKR`), \
             and nothing else ever lets an admitted symbol go: one held for ever keeps its line, and a seventeenth \
             distinct symbol is refused for the life of the process",
        ),
    },
    CredentialedVenueRow {
        venue: "dukascopy",
        limits: Some(DUKASCOPY),
        login: CredentialedStreamLogin::AnyDemoSession,
        first_price_with_a_new_stream: false,
        reaps_idle_admitted: Some(
            "its bus prices eight instruments for the whole process (`DUKASCOPY`), and nothing else ever lets an \
             admitted pair go: one held for ever keeps its place, and a ninth distinct admitted pair, or a conversion \
             pair for a new currency, is refused for the life of the process",
        ),
    },
    CredentialedVenueRow {
        venue: "fxcm",
        limits: Some(FXCM),
        login: CredentialedStreamLogin::AnyDemoSession,
        first_price_with_a_new_stream: false,
        reaps_idle_admitted: Some(
            "its price lane is ONE permanent poll of the account's own reconcile session (a request every two seconds for \
             as long as any pair is wanted, at most sixteen pairs for the whole process), and nothing else ever lets an \
             admitted pair go",
        ),
    },
];

/// The credentialed venues, [`ROWS`]' names in its order: the venues whose live stream the gatewayhub logs in
/// for, as the account the strategy trades (alpaca, ctrader, ig, oanda), the account whose IB Gateway session
/// prices a stock the engine was not mounted on (ibkr) or what a session the trade lane already started serves
/// (dukascopy's sidecar, fxcm's reconcile session). The datahub opens each as a CREDENTIALED stream under its
/// control key and reads no credential for it; the gatewayhub reads the account row itself. ORDER IS OBSERVABLE:
/// the gatewayhub advertises its roster in this order.
pub const CREDENTIALED_VENUES: [&str; ROWS.len()] = {
    let mut out = [""; ROWS.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = ROWS[i].venue;
        i += 1;
    }
    out
};

/// `venue`'s row, `None` for a venue that is not credentialed.
fn row_for(venue: &str) -> Option<&'static CredentialedVenueRow> {
    ROWS.iter().find(|row| row.venue == venue)
}

/// `venue`'s stream limits, `None` for a venue with no row to write (no ceiling, no spacing).
pub fn stream_limits_for(venue: &str) -> Option<CredentialedStreamLimits> {
    row_for(venue).and_then(|row| row.limits)
}

/// Whose login `venue`'s credentialed stream rides; `None` for a venue with no credentialed stream (a keyless one logs
/// in to nothing).
pub fn stream_login_for(venue: &str) -> Option<CredentialedStreamLogin> {
    row_for(venue).map(|row| row.login)
}

/// Whether `venue`'s QUOTE stream gives its first price to a NEW stream only: what the venue says on connect or on
/// subscribe reaches no subscriber that joins an OPEN stream, which then waits for the venue's next price, and on a
/// quiet pair that is long. A strategy that joins a quote key only the recorder holds, and that has gone quiet, is
/// given a fresh stream (decision 0193, the datahub's `FeedBroker::acquire`'s rule eight). `false` for a venue with no
/// row.
pub fn first_price_comes_with_a_new_stream(venue: &str) -> bool {
    row_for(venue).is_some_and(|row| row.first_price_with_a_new_stream)
}

/// **Why the tradehub gives back `venue`'s pump of an ADMITTED symbol once the engine holds nothing in it** (A39:
/// `TradeLeaseRegistry::reap_idle_admitted`), or `None` where an admitted symbol's pump is permanent (every venue
/// without a row too). The reason is the row's cap: the pairs a fixed allowance prices for the whole process, which
/// nothing else frees.
pub fn reaps_idle_admitted(venue: &str) -> Option<&'static str> {
    row_for(venue).and_then(|row| row.reaps_idle_admitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A venue's limits are its own row's; alpaca (deferred by the owner) and a keyless venue have none, which
    /// is today's behaviour, byte for byte.
    #[test]
    fn a_venue_is_limited_by_its_own_row_and_nothing_else() {
        for row in ROWS {
            assert_eq!(stream_limits_for(row.venue), row.limits);
        }
        assert_eq!(stream_limits_for("alpaca"), None);
        assert_eq!(stream_limits_for("binance"), None, "a keyless venue has no row");
    }

    /// A subscribe of this venue opens no connection, so its row spaces nothing.
    #[test]
    fn fxcm_is_sixteen_streams_and_no_spacing() {
        assert_eq!(stream_limits_for("fxcm"), Some(FXCM));
        assert_eq!((FXCM.max_streams, FXCM.min_open_spacing), (16, Duration::ZERO));
    }

    /// The numbers the first slice's acceptance (A5 and A7) is written against.
    #[test]
    fn oanda_is_sixteen_streams_and_six_hundred_milliseconds() {
        assert_eq!(OANDA.max_streams, 16);
        assert_eq!(OANDA.min_open_spacing, Duration::from_millis(600));
        assert_eq!(stream_limits_for("oanda"), Some(OANDA));
    }

    /// IG's row is the provisional sixteen and 100 ms, and the row is what `stream_limits_for` answers.
    #[test]
    fn ig_is_sixteen_streams_and_a_hundred_milliseconds() {
        assert_eq!(IG.max_streams, 16);
        assert_eq!(IG.min_open_spacing, Duration::from_millis(100));
        assert_eq!(stream_limits_for("ig"), Some(IG));
    }

    /// IBKR's provisional row; `crates/vike-gatewayhub/src/data/credentialed_tests.rs`'s
    /// `ibkrs_ceiling_is_the_cap_its_stream_holds_itself_to` holds the sixteen to the bridge's own cap.
    #[test]
    fn ibkr_is_sixteen_streams_and_a_hundred_milliseconds() {
        assert_eq!(IBKR.max_streams, 16);
        assert_eq!(IBKR.min_open_spacing, Duration::from_millis(100));
        assert_eq!(stream_limits_for("ibkr"), Some(IBKR));
    }

    #[test]
    fn ctrader_is_thirty_two_streams_and_no_spacing() {
        assert_eq!(CTRADER.max_streams, 32);
        assert_eq!(CTRADER.min_open_spacing, Duration::ZERO);
        assert_eq!(stream_limits_for("ctrader"), Some(CTRADER));
    }

    /// The roster is the rows' names in their order, and the order the strategy lane has always advertised.
    #[test]
    fn the_credentialed_venues_are_the_rows_in_order() {
        assert_eq!(
            CREDENTIALED_VENUES,
            ["alpaca", "ctrader", "ig", "oanda", "ibkr", "dukascopy", "fxcm"]
        );
        for v in CREDENTIALED_VENUES {
            assert!(vike_model::VENUES.contains(&v), "`{v}` is not in vike_model::VENUES");
        }
    }

    #[test]
    fn every_row_is_listed_once() {
        for (i, row) in ROWS.iter().enumerate() {
            assert!(
                !ROWS[..i].iter().any(|earlier| earlier.venue == row.venue),
                "`{}` has two rows",
                row.venue
            );
        }
    }

    #[test]
    fn a_row_is_usable() {
        for CredentialedVenueRow { venue, limits, .. } in ROWS {
            if let Some(l) = limits {
                assert!(l.max_streams > 0, "{venue}: a zero ceiling refuses every key");
                assert!(l.min_open_spacing < Duration::from_secs(5), "{venue}: spacing {l:?}");
            }
        }
    }

    /// **The login column**: dukascopy's and fxcm's streams ride the trade lane's running session and log in as no
    /// account of their own; every other credentialed stream logs in as the venue's default account; a keyless venue
    /// has no row.
    #[test]
    fn only_dukascopy_and_fxcm_ride_a_running_session() {
        for venue in CREDENTIALED_VENUES {
            let want = if matches!(venue, "dukascopy" | "fxcm") {
                CredentialedStreamLogin::AnyDemoSession
            } else {
                CredentialedStreamLogin::DefaultAccount
            };
            assert_eq!(stream_login_for(venue), Some(want), "{venue}");
        }
        assert_eq!(stream_login_for("binance"), None, "a keyless venue logs in to nothing");
    }

    /// **The first-price column** names the four venues whose bridges were read to give their first price to a new
    /// stream only, and no other.
    #[test]
    fn the_first_price_comes_with_a_new_stream_on_oanda_ig_ibkr_and_ctrader_only() {
        let fresh: Vec<&str> = CREDENTIALED_VENUES
            .into_iter()
            .filter(|v| first_price_comes_with_a_new_stream(v))
            .collect();
        assert_eq!(fresh, ["ctrader", "ig", "oanda", "ibkr"]);
        assert!(!first_price_comes_with_a_new_stream("binance"), "a venue with no row");
    }

    /// **The reap column** names the three venues whose price lane holds a fixed allowance for the whole process that
    /// nothing else frees (fxcm's poll, dukascopy's eight instruments, ibkr's sixteen lines), each with its reason, and
    /// no other; a keyless venue has no row.
    #[test]
    fn only_fxcm_dukascopy_and_ibkr_reap_an_idle_admitted_pump() {
        let reaping: Vec<&str> =
            CREDENTIALED_VENUES.into_iter().filter(|v| reaps_idle_admitted(v).is_some()).collect();
        assert_eq!(reaping, ["ibkr", "dukascopy", "fxcm"]);
        for venue in reaping {
            assert!(
                reaps_idle_admitted(venue)
                    .is_some_and(|why| why.contains("nothing else ever lets an")),
                "{venue}: the reason names what nothing else frees"
            );
        }
        assert_eq!(
            reaps_idle_admitted("fxcm"),
            Some(
                "its price lane is ONE permanent poll of the account's own reconcile session (a request every two \
                 seconds for as long as any pair is wanted, at most sixteen pairs for the whole process), and nothing \
                 else ever lets an admitted pair go"
            ),
            "fxcm's sentence, moved verbatim from the tradehub"
        );
        assert_eq!(reaps_idle_admitted("binance"), None, "a keyless venue has no row");
    }
}
