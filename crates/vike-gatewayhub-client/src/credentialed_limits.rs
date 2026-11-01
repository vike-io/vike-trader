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
//! [`ROWS`] names EVERY credentialed venue, in the order of the gatewayhub's credentialed-stream roster
//! (`crates/vike-gatewayhub/src/registry/mod.rs`'s `ROSTERS`; `crates/vike-gatewayhub/src/registry/registry_tests.rs`'s
//! test holds the two equal), so a venue added to the roster fails a test until somebody decides its
//! row. A `None` row is a venue with no ceiling and no spacing: today's behaviour, byte for byte. A
//! row is written in the venue's own slice, behind its own demo measurement.
//!
//! The numbers are PROVISIONAL (decision 0193: "starting values until measured"): the repository
//! says of OANDA's stream-session cap only "UNMEASURED" (`crates/bridges/oanda/CLAUDE.md`).

use std::time::Duration;

/// One venue's stream limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamLimits {
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
/// This is a ceiling on the strategy lane, the recorder and md TOGETHER. Before decision 0193 the strategy
/// lane alone was allowed `SL_MAX_KEYS_PER_VENUE` (sixty-four) keys per venue, so a box that streams more
/// than sixteen OANDA symbols is refused the rest; a refused key is a cap refusal (the tradehub keeps the
/// venue up and drops THAT key's quotes, `crates/vike-tradehub/src/feeds/oanda.rs`'s `wire_oanda_feed`).
pub const OANDA: StreamLimits = StreamLimits {
    max_streams: OANDA_ACTIVE_STREAMS - OANDA_TRANSACTION_STREAMS - OANDA_MARGIN,
    min_open_spacing: Duration::from_millis(600),
};

// The pricing streams and the exec side's transactions streams fit the venue's allowance with room to spare.
const _: () = assert!(OANDA.max_streams + OANDA_TRANSACTION_STREAMS < OANDA_ACTIVE_STREAMS);

/// Every credentialed venue and its limits. See the module doc.
pub const ROWS: [(&str, Option<StreamLimits>); 7] = [
    ("alpaca", None),
    ("ctrader", None),
    ("ig", None),
    ("oanda", Some(OANDA)),
    ("ibkr", None),
    ("dukascopy", None),
    ("fxcm", None),
];

/// `venue`'s stream limits, `None` for a venue with no row to write (no ceiling, no spacing).
pub fn stream_limits_for(venue: &str) -> Option<StreamLimits> {
    ROWS.iter().find(|(name, _)| *name == venue).and_then(|(_, limits)| *limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oanda_has_a_row_in_wave_a_and_the_rest_keep_todays_behaviour() {
        for (venue, limits) in ROWS {
            assert_eq!(stream_limits_for(venue), limits);
            assert_eq!(limits.is_some(), venue == "oanda", "{venue}");
        }
        assert_eq!(stream_limits_for("binance"), None, "a keyless venue has no row");
    }

    /// The numbers the first slice's acceptance (A5 and A7) is written against.
    #[test]
    fn oanda_is_sixteen_streams_and_six_hundred_milliseconds() {
        assert_eq!(OANDA.max_streams, 16);
        assert_eq!(OANDA.min_open_spacing, Duration::from_millis(600));
        assert_eq!(stream_limits_for("oanda"), Some(OANDA));
    }

    #[test]
    fn every_row_is_listed_once() {
        for (i, (venue, _)) in ROWS.iter().enumerate() {
            assert!(!ROWS[..i].iter().any(|(v, _)| v == venue), "`{venue}` has two rows");
        }
    }

    #[test]
    fn a_row_is_usable() {
        for (venue, limits) in ROWS {
            if let Some(l) = limits {
                assert!(l.max_streams > 0, "{venue}: a zero ceiling refuses every key");
                assert!(l.min_open_spacing < Duration::from_secs(5), "{venue}: spacing {l:?}");
            }
        }
    }
}
