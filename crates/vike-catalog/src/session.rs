//! The `(venue, asset-class) -> SessionCalendar` resolution — the keying authority for the ONE
//! session/market-hours law.
//!
//! The pure law and its rows live one layer down, in [`vike_model::session`]. That crate CANNOT
//! own this map: the session key is (venue, asset class), [`AssetClass`] is a `vike-catalog`
//! concept, and `vike-catalog -> vike-model` is a down-only edge — so resolving by asset class here
//! is the only place both facts are visible without inverting the layering. `vike-model` takes the
//! resolved [`SessionCalendar`] as a caller-supplied parameter (see that module's doc); THIS is the
//! caller that supplies it.
//!
//! ## Why venue alone is not the key
//!
//! [`vike_model::session_for`] keys by venue and is deliberately fail-permissive for the mixed
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
//! (venue, asset-class), so it resolves open — a per-symbol override (`EngineParams::session_calendars`)
//! supplies the real regional session.
//!
//! ## Deferred (see `vike_model::session`'s module doc)
//!
//! Holiday calendars, non-US regional equity sessions, and listed-derivative (CME/CBOE) extended
//! hours are follow-ups. Until their rows land in `vike-model`, they resolve fail-permissive here.

use vike_model::session::{CRYPTO_24_7, FX_WEEK, US_EQUITY_REGULAR};
use vike_model::SessionCalendar;

use crate::AssetClass;

/// Venues whose LISTED DERIVATIVES (options/futures) and index products trade continuously because
/// the venue itself is a 24/7 crypto-native exchange. Deribit options, for instance, are 24/7.
fn is_crypto_native(venue: &str) -> bool {
    matches!(venue, "deribit" | "binance" | "bybit" | "okx" | "aster" | "hyperliquid")
}
// vike:new-venue:note decide `{venue}`'s (venue, asset-class) rows: add it to `is_crypto_native` if its LISTED DERIVATIVES trade 24/7, and give it an Equity/Etf arm if it lists cash equities in a modelled region. NOTHING here iterates the roster, so an unclassified venue silently answers ALWAYS_OPEN — fail-permissive by design, and therefore invisible: crates/vike-catalog/src/session.rs's `session_calendar_for`

/// The [`SessionCalendar`] for an instrument identified by its `venue` slug and [`AssetClass`] —
/// the ONE (venue, asset-class) keying site. Pure and total: an unmodeled pair answers
/// [`SessionCalendar::ALWAYS_OPEN`] (fail-permissive, never over-block — see the module doc).
///
/// This resolves the venue-alone defect: `(alpaca, CryptoSpot)` and `(alpaca, Equity)` return
/// different calendars off the same venue, which venue-only keying ([`vike_model::session_for`])
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
        // Listed options / futures / index products: 24/7 on crypto-native venues (Deribit
        // options, perps' index products). Every OTHER listed-derivative session (CME/CBOE regular
        // + extended hours) is a documented follow-up — fail-permissive rather than approximate it
        // with the cash-equity row (which would over-block their long trading days).
        Option | Future | Index => {
            if is_crypto_native(venue) {
                CRYPTO_24_7
            } else {
                SessionCalendar::ALWAYS_OPEN
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epoch-ms for a UTC instant from vike-model's own civil math (no chrono).
    fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
        vike_model::days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
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

    #[test]
    fn unknown_venue_and_class_pair_is_always_open() {
        assert_eq!(
            session_calendar_for("mystery", AssetClass::Equity),
            SessionCalendar::ALWAYS_OPEN,
            "an unmodeled (venue, class) must never block"
        );
    }
}
