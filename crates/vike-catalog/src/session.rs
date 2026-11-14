//! The `(venue, asset-class) -> SessionCalendar` resolution — the keying authority for the ONE
//! session/market-hours law.
//!
//! The pure law and its rows live one layer down, in [`vike_venues::session`], which takes the
//! resolved [`SessionCalendar`] as a caller-supplied parameter (see that module's doc); THIS is the
//! caller that supplies it.
//!
//! ⚠ **This doc used to say `vike-model` CANNOT own the map, because [`AssetClass`] was a
//! `vike-catalog` concept and `vike-catalog -> vike-model` is down-only. That reason is DEAD** —
//! the taxonomy moved to `vike-model` (`crates/vike-model/src/instrument/asset_class.rs`) so the hist store
//! could name it, and both halves of the key are now visible one layer down. Nothing forces this
//! resolver up here any more; it stays because it is a CATALOG table and because moving it is a
//! behaviour change, not because the layering refuses the alternative. ⚠ Do not cite this module
//! as the layering precedent it was cited as — `docs/decisions/0061` leaned on exactly that
//! sentence, and its second reason (nothing forces a per-venue table into the vocabulary crate) is
//! the one that survived.
//!
//! ## Why venue alone is not the key
//!
//! [`vike_venues::session_for`] keys by venue and is deliberately fail-permissive for the mixed
//! venues, because venue alone cannot pick a session: **Alpaca** trades cash equities AND 24/7
//! crypto on one venue, and **IBKR** is a global multi-asset book. The fix is to key by
//! (venue, asset-class) — `(alpaca, CryptoSpot)` is 24/7 while `(alpaca, Equity)` is the US regular
//! session — which is exactly what [`session_calendar_for`] does.
//!
//! ## Fail-permissive, never over-block
//!
//! Every combination this resolver cannot pin answers [`SessionCalendar::ALWAYS_OPEN`]. Under-
//! blocking (closed time looking open) is the pre-existing no-gate behavior; OVER-blocking would
//! silently delete real in-session fills and corrupt a backtest, so an unmodeled market is left
//! open. In particular a **non-US equity on IBKR** cannot have its region inferred from
//! (venue, asset-class), so it resolves open — a per-symbol override (`StrategyEngineConfig::session_calendars`)
//! supplies the real regional session.
//!
//! ## Deferred (see `vike_venues::session`'s module doc)
//!
//! Holiday calendars, non-US regional equity sessions, and listed-derivative (CME/CBOE) extended
//! hours are follow-ups. Until their rows land in `vike-model`, they resolve fail-permissive here.

use vike_venues::SessionCalendar;
use vike_venues::session::{
    CRYPTO_24_7, FX_WEEK, US_EQUITY_REGULAR, is_crypto_native, session_for,
};

use vike_model::{AssetClass, Instrument};

// The venues whose LISTED DERIVATIVES (options/futures) and index products trade continuously
// because the venue itself is a 24/7 crypto-native exchange (Deribit options, for instance) are
// `vike_venues::session::is_crypto_native`'s set — the ONE copy, shared with the venue-only
// default `session_for`. This file used to carry its own list of the six, and spelling one here
// again is what `the_derivative_rule_reads_the_one_crypto_native_set` refuses.
// vike:new-venue:note decide `{venue}`'s (venue, asset-class) rows: give it an Equity/Etf arm if it lists cash equities in a modelled region. Whether its LISTED DERIVATIVES trade 24/7 is NOT decided here: it follows vike_venues::session::is_crypto_native, the one set `session_for` shares, so classify the venue THERE. NOTHING here iterates the roster, so an unclassified venue silently answers ALWAYS_OPEN — fail-permissive by design, and therefore invisible: crates/vike-catalog/src/session.rs's `session_calendar_for`

/// The [`SessionCalendar`] for an instrument identified by its `venue` slug and [`AssetClass`] —
/// the ONE (venue, asset-class) keying site. Pure and total: an unmodeled pair answers
/// [`SessionCalendar::ALWAYS_OPEN`] (fail-permissive, never over-block — see the module doc).
///
/// This resolves the venue-alone defect: `(alpaca, CryptoSpot)` and `(alpaca, Equity)` return
/// different calendars off the same venue, which venue-only keying ([`vike_venues::session_for`])
/// cannot do.
pub fn session_calendar_for(venue: &str, class: AssetClass) -> SessionCalendar {
    use AssetClass::*;
    match class {
        // Continuously-traded markets — no weekly close, venue-independent.
        CryptoSpot | CryptoPerp | CryptoFuture | PredictionMarket => CRYPTO_24_7,
        // Spot FX and CFDs — the FX week (Sun 21:00 -> Fri 22:00 UTC, DST-union).
        Fx | Cfd => FX_WEEK,
        // Cash equities / ETFs — a regional REGULAR session, chosen by venue. Only the US session
        // is modeled, and only for venues confident to be US: Alpaca is US-only, so it is safe;
        // IBKR is global, so an IBKR equity's region is NOT inferable here — fail-permissive, and
        // a per-symbol override supplies the real session. (This asymmetry is the whole point of
        // keying by BOTH venue and class, then still allowing a per-symbol escape.)
        Equity | Etf => match venue {
            "alpaca" => US_EQUITY_REGULAR,
            _ => SessionCalendar::ALWAYS_OPEN,
        },
        // Bonds, funds, warrants and spot commodities: no session is modelled for any of them on
        // any venue, so they resolve fail-permissive rather than borrow the cash-equity row.
        Bond | Fund | Warrant | Commodity => SessionCalendar::ALWAYS_OPEN,
        // Listed options / futures / index products: 24/7 on crypto-native venues (Deribit
        // options, perps' index products). Every OTHER listed-derivative session (CME/CBOE regular
        // + extended hours) is a documented follow-up — fail-permissive rather than approximate it
        // with the cash-equity row (which would over-block their long trading days).
        // A venue-listed combo trades when its legs do, so it keys like them.
        Option | Future | Index | Combo => {
            if is_crypto_native(venue) {
                CRYPTO_24_7
            } else {
                SessionCalendar::ALWAYS_OPEN
            }
        }
    }
}

/// **Whether a KEPT row of `venue` can name a session class at all**: alpaca (its shares and its coins)
/// and the venues whose whole book runs the FX week (the venues [`vike_venues::session::session_for`] gives
/// [`FX_WEEK`], read from there, not listed here). Every other venue's answer is `None` whatever its row
/// says, so a process judging closed time reads only these venues' lists.
#[must_use]
pub fn session_venue(venue: &str) -> bool {
    venue == "alpaca" || session_for(venue) == FX_WEEK
}

/// **The asset class `venue`'s KEPT list states for a symbol, asked for one purpose: its session.** `row` is
/// the symbol's row of the list the datahub keeps (`<project>/instruments/<venue>.json`, found by
/// `crate::kept_row`), `None` when the list does not carry the symbol or no list is held: the class is then
/// UNKNOWN (the owner's ruling of 2026-11-13), which the closed-market note takes as "no calendar known: say
/// nothing" and the readers of [`trading_calendar`] as "the plain rule". A name is never read for a class.
///
/// - **An FX pair ([`AssetClass::Fx`]) on a [`session_venue`] whose whole book runs the FX week.** An index,
///   a metal, an oil or a share CFD on the same broker has hours of its own and is NOT FX-week, so it gets
///   `None`.
/// - **alpaca**: a coin ([`AssetClass::CryptoSpot`], always open), a US share or an ETF.
/// - **Everything else: `None`.** In particular IBKR (a global book: a stock's region is not in its row's
///   class, `session_calendar_for`'s own rule), the crypto exchanges (always open, so nothing to say) and
///   Polymarket.
#[must_use]
pub fn session_class_of(venue: &str, row: Option<&Instrument>) -> Option<AssetClass> {
    let class = row?.asset_class();
    let proved = match class {
        AssetClass::Fx => session_for(venue) == FX_WEEK,
        AssetClass::CryptoSpot | AssetClass::Equity | AssetClass::Etf => venue == "alpaca",
        _ => false,
    };
    proved.then_some(class)
}

/// **Whether a symbol on `venue` trades every calendar day** (a 24/7 book): the fact a live tearsheet's
/// one-sample-per-day curve is annualised by (`vike_analytics::report::periods_per_year_for_days`: 365
/// when every market on the sheet does, 252 otherwise). The class its kept `row` states
/// ([`session_class_of`]) keys [`session_calendar_for`] where it settles one (an alpaca coin yes, an alpaca
/// share or an FX pair no); otherwise only a venue whose whole book is 24/7 says yes: the crypto-native set
/// and polymarket, the venue-only 24/7 books `vike_venues::session_for` names. ⚠ Not
/// `session_for(venue) == CRYPTO_24_7`: that equals the fail-permissive `ALWAYS_OPEN` by value, so IBKR or
/// an unknown venue would read as 24/7 and their Sharpe would be scaled up by sqrt(365/252). Those answer
/// `false`, the session year.
#[must_use]
pub fn trades_every_day(venue: &str, row: Option<&Instrument>) -> bool {
    match session_class_of(venue, row) {
        Some(class) => session_calendar_for(venue, class) == CRYPTO_24_7,
        None => is_crypto_native(venue) || venue == "polymarket",
    }
}

/// **The trading calendar a series on `venue` is judged against**, when its kept `row` states a class with
/// closed time: that class ([`session_class_of`]) through [`session_calendar_for`]. `None` for a market open
/// every minute (crypto, `ALWAYS_OPEN` by value) and for one whose class is unknown. The one rule a
/// backtest's input coverage (`vike_backtest`'s `data_plan/inputs.rs`), a live mount's input freshness
/// (`vike_core`'s `InputRead::calendar`, filled by the tradehub) and a restart's closed-trade bar clock read
/// closed time by.
#[must_use]
pub fn trading_calendar(venue: &str, row: Option<&Instrument>) -> Option<SessionCalendar> {
    let class = session_class_of(venue, row)?;
    Some(session_calendar_for(venue, class)).filter(|c| *c != SessionCalendar::ALWAYS_OPEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epoch-ms for a UTC instant from vike-model's own civil math (no chrono).
    fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
        vike_utility::calendar::days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
    }

    #[test]
    fn alpaca_crypto_and_equity_resolve_to_different_calendars() {
        // THE fix: one venue, two asset classes, two sessions.
        assert_eq!(session_calendar_for("alpaca", AssetClass::CryptoSpot), CRYPTO_24_7);
        assert_eq!(session_calendar_for("alpaca", AssetClass::Equity), US_EQUITY_REGULAR);
        assert_ne!(
            session_calendar_for("alpaca", AssetClass::CryptoSpot),
            session_calendar_for("alpaca", AssetClass::Equity),
            "alpaca crypto must NOT share the equity session (the venue-alone defect)"
        );

        // and it bites at a real instant: on a Saturday the equity leg is CLOSED, the crypto leg OPEN
        let sat = ms(2024, 1, 6, 12); // 2024-01-06 is a Saturday
        assert!(!session_calendar_for("alpaca", AssetClass::Equity).is_open(sat));
        assert!(session_calendar_for("alpaca", AssetClass::CryptoSpot).is_open(sat));
    }

    #[test]
    fn ibkr_keys_by_class_and_is_fail_permissive_on_ambiguous_region() {
        // FX and crypto legs of a global book resolve confidently...
        assert_eq!(session_calendar_for("ibkr", AssetClass::Fx), FX_WEEK);
        assert_eq!(session_calendar_for("ibkr", AssetClass::CryptoPerp), CRYPTO_24_7);
        // ...but an IBKR equity's region is not inferable from (venue, class): fail-permissive, so
        // a non-US listing is never over-blocked. The per-symbol override is the escape hatch.
        assert_eq!(session_calendar_for("ibkr", AssetClass::Equity), SessionCalendar::ALWAYS_OPEN);
    }

    #[test]
    fn continuous_markets_are_24_7_regardless_of_venue() {
        for (v, c) in [
            ("binance", AssetClass::CryptoSpot),
            ("bybit", AssetClass::CryptoPerp),
            ("okx", AssetClass::CryptoFuture),
            ("polymarket", AssetClass::PredictionMarket),
            ("hyperliquid", AssetClass::CryptoPerp),
        ] {
            assert_eq!(session_calendar_for(v, c), CRYPTO_24_7, "{v}/{c:?}");
        }
    }

    #[test]
    fn fx_and_cfd_are_the_fx_week() {
        for v in ["dukascopy", "oanda", "ig", "fxcm", "ctrader"] {
            assert_eq!(session_calendar_for(v, AssetClass::Fx), FX_WEEK, "{v} Fx");
            assert_eq!(session_calendar_for(v, AssetClass::Cfd), FX_WEEK, "{v} Cfd");
        }
    }

    #[test]
    fn deribit_options_are_24_7_but_a_us_listed_option_is_fail_permissive() {
        assert_eq!(session_calendar_for("deribit", AssetClass::Option), CRYPTO_24_7);
        // listed equity options / index futures elsewhere: not modeled ⇒ fail-permissive
        assert_eq!(session_calendar_for("ibkr", AssetClass::Option), SessionCalendar::ALWAYS_OPEN);
        assert_eq!(session_calendar_for("cme", AssetClass::Future), SessionCalendar::ALWAYS_OPEN);
        assert_eq!(session_calendar_for("alpaca", AssetClass::Index), SessionCalendar::ALWAYS_OPEN);
    }

    /// The decision-0126 classes: a combo keys like its legs, the rest are unmodelled everywhere.
    #[test]
    fn the_0126_classes_resolve() {
        assert_eq!(session_calendar_for("deribit", AssetClass::Combo), CRYPTO_24_7);
        assert_eq!(session_calendar_for("ibkr", AssetClass::Combo), SessionCalendar::ALWAYS_OPEN);
        for class in
            [AssetClass::Bond, AssetClass::Fund, AssetClass::Warrant, AssetClass::Commodity]
        {
            for venue in ["ibkr", "alpaca", "deribit"] {
                assert_eq!(
                    session_calendar_for(venue, class),
                    SessionCalendar::ALWAYS_OPEN,
                    "{venue} {class:?}"
                );
            }
        }
    }

    /// The derivative classes follow `vike_venues::session::is_crypto_native` and nothing
    /// else: a crypto exchange's options/futures/index/combo are the 24/7 row and every other
    /// roster venue's are not modelled. Written against the roster so a new venue is judged here
    /// too. ⚠ `CRYPTO_24_7` equals `SessionCalendar::ALWAYS_OPEN` by value today, so this asserts
    /// the CONTRACT (it bites the day the two differ) and the set itself is pinned by name in
    /// `vike-model`'s `the_crypto_native_set_is_pinned_and_polymarket_is_the_one_24_7_book_outside_it`.
    #[test]
    fn the_derivative_rule_reads_the_one_crypto_native_set() {
        for &venue in vike_model::VENUES {
            let want =
                if is_crypto_native(venue) { CRYPTO_24_7 } else { SessionCalendar::ALWAYS_OPEN };
            for class in
                [AssetClass::Option, AssetClass::Future, AssetClass::Index, AssetClass::Combo]
            {
                assert_eq!(session_calendar_for(venue, class), want, "{venue} {class:?}");
            }
        }
    }

    /// polymarket: a 24/7 book by venue ([`vike_venues::session_for`]) that is NOT crypto-native —
    /// it lists no derivatives, and its outcome contracts are 24/7 by CLASS for every venue.
    #[test]
    fn polymarket_is_24_7_by_class_not_by_the_crypto_native_set() {
        assert!(!is_crypto_native("polymarket"));
        assert_eq!(vike_venues::session_for("polymarket"), CRYPTO_24_7);
        assert_eq!(session_calendar_for("polymarket", AssetClass::PredictionMarket), CRYPTO_24_7);
        assert_eq!(
            session_calendar_for("polymarket", AssetClass::Option),
            SessionCalendar::ALWAYS_OPEN,
            "a derivative class on polymarket is not modelled: the set that makes derivatives 24/7 \
             is the crypto EXCHANGES'"
        );
    }

    /// THE guard against the copy this file used to carry: the production half of this file names
    /// no crypto-native exchange as a string literal. The venue set has one owner, and a second
    /// list here would be a set that can drift from `vike-model`'s — the very thing it replaced.
    #[test]
    fn this_file_spells_no_crypto_native_venue_in_code() {
        let src = include_str!("session.rs");
        let production = src.split("#[cfg(test)]").next().expect("split yields a first part");
        for venue in ["deribit", "binance", "bybit", "okx", "aster", "hyperliquid"] {
            let quoted = format!("\"{venue}\"");
            assert!(
                !production.contains(&quoted),
                "{quoted} is spelled in `vike-catalog`'s session.rs: the crypto-native set is \
                 `vike_venues::session::is_crypto_native`'s alone"
            );
        }
    }

    /// A kept row of `class` on `venue`: what the datahub's list states for the symbol.
    fn row(venue: &str, symbol: &str, class: AssetClass) -> Instrument {
        crate::test_support::test_row(venue, symbol, "BASE", "USD", class)
    }

    /// The note's class from the KEPT ROW: an FX pair on the five FX-week brokers in each broker's own
    /// spelling, and nothing the row does not prove.
    #[test]
    fn an_fx_row_on_an_fx_week_broker_is_fx_and_nothing_else_is_guessed() {
        for (venue, symbol) in [
            ("dukascopy", "EUR/USD"),
            ("fxcm", "EUR/USD"),
            ("fxcm", "USDJPY"),
            ("oanda", "EUR_USD"),
            ("ctrader", "EURUSD"),
            ("ig", "CS.D.EURUSD.CFD.IP"),
        ] {
            let fx = row(venue, symbol, AssetClass::Fx);
            assert_eq!(
                session_class_of(venue, Some(&fx)),
                Some(AssetClass::Fx),
                "{venue} {symbol}"
            );
            assert_eq!(
                session_calendar_for(venue, AssetClass::Fx),
                FX_WEEK,
                "{venue} {symbol}: the class resolves to the FX week"
            );
        }
        // an index, a metal, an oil, a share or a crypto CFD: hours of their own, never FX-week
        for venue in ["oanda", "ctrader", "dukascopy", "ig", "fxcm"] {
            assert_eq!(session_class_of(venue, Some(&row(venue, "X", AssetClass::Cfd))), None);
        }
        // ...and a symbol the list does not carry is UNKNOWN: its name is never read.
        for (venue, symbol) in [("fxcm", "EURUSD"), ("oanda", "EUR_USD"), ("alpaca", "AAPL")] {
            assert_eq!(session_class_of(venue, None), None, "{venue} {symbol}");
            assert_eq!(trading_calendar(venue, None), None, "{venue} {symbol}");
        }
    }

    /// A venue whose book is not the FX week is never classified FX, whatever its row says, and a mixed
    /// or crypto venue says nothing: IBKR's region is not in a stock's class.
    #[test]
    fn only_the_fx_week_brokers_and_alpaca_have_a_class_from_a_row() {
        for venue in
            ["binance", "bybit", "okx", "deribit", "hyperliquid", "aster", "polymarket", "ibkr"]
        {
            assert!(!session_venue(venue), "{venue}");
            assert_eq!(session_class_of(venue, Some(&row(venue, "X", AssetClass::Fx))), None);
            assert_eq!(session_class_of(venue, Some(&row(venue, "X", AssetClass::Equity))), None);
        }
        for venue in ["alpaca", "dukascopy", "fxcm", "oanda", "ctrader", "ig"] {
            assert!(session_venue(venue), "{venue}");
        }
        for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::CryptoSpot] {
            assert_eq!(session_class_of("alpaca", Some(&row("alpaca", "X", class))), Some(class));
        }
        assert_eq!(session_class_of("alpaca", Some(&row("alpaca", "X", AssetClass::Option))), None);
    }

    /// The tearsheet's annualisation fact: a 24/7 book says yes; a session market, and a book whose class
    /// is unknown, say no (never read off `ALWAYS_OPEN`, which equals the 24/7 row).
    #[test]
    fn only_a_24_7_market_trades_every_day() {
        for venue in vike_model::VENUES.iter().copied().filter(|v| is_crypto_native(v)) {
            assert!(trades_every_day(venue, None), "{venue}");
        }
        assert!(trades_every_day("polymarket", None));
        assert!(trades_every_day(
            "alpaca",
            Some(&row("alpaca", "BTC/USD", AssetClass::CryptoSpot))
        ));
        for (venue, kept) in [
            ("alpaca", Some(row("alpaca", "AAPL", AssetClass::Equity))),
            ("oanda", Some(row("oanda", "EUR_USD", AssetClass::Fx))),
            ("oanda", Some(row("oanda", "SPX500_USD", AssetClass::Cfd))),
            ("oanda", None),
            ("ibkr", None),
            ("mystery", None),
        ] {
            assert!(!trades_every_day(venue, kept.as_ref()), "{venue} {kept:?}");
        }
    }

    #[test]
    fn unknown_venue_and_class_pair_is_always_open() {
        assert_eq!(
            session_calendar_for("mystery", AssetClass::Equity),
            SessionCalendar::ALWAYS_OPEN,
            "an unmodeled (venue, class) must never block"
        );
    }

    /// **The calendar a daily FX series is judged by rolls its day at 17:00 New York in both weeks the clocks
    /// change** (`trading_calendar`, the rule a backtest's coverage and a live input's freshness read): the week
    /// opens at 17:00 New York on the Sunday (22:00 UTC in March before the change, 21:00 after it; 21:00 UTC in
    /// November before, 22:00 after), and a day bar opened at the week's open closes 24 New York hours later. A
    /// coin has no calendar.
    #[test]
    fn a_daily_fx_series_rolls_at_17_new_york_in_both_change_weeks() {
        const DAY: i64 = 86_400_000;
        let fx = trading_calendar("fxcm", Some(&row("fxcm", "EUR/USD", AssetClass::Fx)))
            .expect("an FX pair keeps the FX week");
        for (friday_close, sunday_open) in [
            // 2024-03-08 17:00 EST; 2024-03-10 17:00 EDT (the clocks went forward that morning).
            (ms(2024, 3, 8, 22), ms(2024, 3, 10, 21)),
            // 2024-11-01 17:00 EDT; 2024-11-03 17:00 EST (the clocks went back that morning).
            (ms(2024, 11, 1, 21), ms(2024, 11, 3, 22)),
        ] {
            assert!(!fx.is_open_civil(friday_close), "the week closes at 17:00 New York");
            assert_eq!(
                fx.next_open_civil(friday_close),
                Some(sunday_open),
                "and opens at 17:00 New York"
            );
            let monday_close = fx.civil_after(sunday_open, DAY);
            assert_eq!(
                monday_close,
                sunday_open + DAY,
                "no change between Sunday and Monday 17:00"
            );
            assert_eq!(
                fx.civil_after(friday_close, 2 * DAY),
                sunday_open,
                "two New York days to the open"
            );
        }
        let coin = row("binance", "BTCUSDT", AssetClass::CryptoSpot);
        assert_eq!(trading_calendar("binance", Some(&coin)), None, "a coin is open every minute");
    }
}
