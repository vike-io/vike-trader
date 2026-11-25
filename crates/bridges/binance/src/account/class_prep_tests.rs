//! Offline pins for `crate::account::class_prep` over a scripted venue (NO network): the first-order leverage write of a
//! symbol the account never traded, the words of a `-4411` reject (the bridge signs no agreement), and of `-2027`.
//! Every shape below is the one MEASURED on the demo host on 2026-10-30
//! (`crates/bridges/binance/tests/binance_smokes/binance_classes_smoke.rs`).
//!
//! | test | what it pins |
//! |---|---|
//! | [`a_never_traded_symbol_gets_one_leverage_write_capped_at_its_bracket`] | `positionRisk` leverage `0` + flat -> ONE `POST /fapi/v1/leverage` of `min(configured, bracket max)` |
//! | [`a_symbol_that_holds_a_leverage_is_never_written`] | a healthy symbol costs one read and no write |
//! | [`a_symbol_that_is_not_provably_flat_or_unreadable_is_never_written`] | a position, a missing row, a failed read: nothing written |
//! | [`an_unreadable_bracket_writes_the_configured_figure_and_a_refusal_is_named`] | the bracket is optional; the venue's refusal is worded |
//! | [`the_mainnet_book_is_never_read_or_written`] | the owner's ruling: no write on a live host |
//! | [`a_tradfi_reject_signs_nothing_and_names_the_hand_step_on_either_network`] | `-4411` -> the symbol's hand step, no request, demo and mainnet |
//! | [`the_position_at_leverage_refusal_is_worded_for_both_networks`] | `-2027` explained, demo and mainnet |

use crate::family::signer::BinanceHmacSigner;
use serde_json::json;

use super::*;
use crate::account::modes::modes_tests::{Answer, DEMO_USDM, LIVE_USDM, Venue, perp_rest, refusal};
use crate::account::modes::{order_reject_reason, reject_reason};
use vike_bridge_core::transport::VenueApiError;
use vike_model::Margining;
use vike_venues::venue_urls::UrlType;

fn rest_on(
    venue: Venue,
    book: (UrlType, Margining),
    leverage: f64,
) -> VenueRestBinancePerp<BinanceHmacSigner, Venue> {
    perp_rest(venue, book, "TSLAUSDT", leverage)
}

const GET_RISK: &str = "GET /fapi/v2/positionRisk";
const GET_BRACKET: &str = "GET /fapi/v1/leverageBracket";
const POST_LEVERAGE: &str = "POST /fapi/v1/leverage";

/// `TSLAUSDT` as the demo host answered it before any leverage was set: leverage `"0"`, flat, cross.
fn never_traded() -> Answer {
    Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.00", "marginType": "cross", "positionSide": "BOTH",
        "leverage": "0", "isolatedMargin": "0.00000000"
    }]))
}

/// The bracket the demo host stated for `TSLAUSDT` (first bracket 5x).
fn tsla_bracket() -> Answer {
    Ok(json!([{
        "symbol": "TSLAUSDT",
        "brackets": [
            {"bracket": 1, "initialLeverage": 5, "notionalCap": 5000, "notionalFloor": 0},
            {"bracket": 2, "initialLeverage": 4, "notionalCap": 10000, "notionalFloor": 5000}
        ]
    }]))
}

fn leverage_ok() -> Answer {
    Ok(json!({"leverage": 2, "maxNotionalValue": "80000", "symbol": "TSLAUSDT"}))
}

fn venue_for_a_write() -> Venue {
    Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", tsla_bracket())
        .on("POST", "/fapi/v1/leverage", leverage_ok())
}

#[test]
fn a_never_traded_symbol_gets_one_leverage_write_capped_at_its_bracket() {
    // Configured 2x, bracket maximum 5x -> 2.
    let rest = rest_on(venue_for_a_write(), DEMO_USDM, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
    assert_eq!(rest.transport.sent(), [GET_RISK, GET_BRACKET, POST_LEVERAGE]);
    assert_eq!(
        rest.transport.params_of("POST", "/fapi/v1/leverage"),
        [("symbol".to_string(), "TSLAUSDT".to_string()), ("leverage".to_string(), "2".to_string())],
    );
    // Configured 20x, bracket maximum 5x -> 5, never above what the symbol allows.
    let rest = rest_on(venue_for_a_write(), DEMO_USDM, 20.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(5));
    assert_eq!(
        rest.transport.params_of("POST", "/fapi/v1/leverage")[1],
        ("leverage".to_string(), "5".to_string())
    );
    // A fractional configured figure is written as the whole number below it (the venue takes integers).
    let rest = rest_on(venue_for_a_write(), DEMO_USDM, 2.9);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
}

#[test]
fn a_symbol_that_holds_a_leverage_is_never_written() {
    let held = Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.00", "marginType": "cross", "positionSide": "BOTH",
        "leverage": "3"
    }]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", held), DEMO_USDM, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Held(3.0));
    assert_eq!(rest.transport.sent(), [GET_RISK], "one read, no bracket, no write");
}

#[test]
fn a_symbol_that_is_not_provably_flat_or_unreadable_is_never_written() {
    // A leverage of 0 with a position is a surprise: left alone.
    let open = Ok(json!([{
        "symbol": "TSLAUSDT", "positionAmt": "0.10", "positionSide": "BOTH", "leverage": "0"
    }]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", open), DEMO_USDM, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
    // No amount at all is not "flat".
    let no_amount = Ok(json!([{"symbol": "TSLAUSDT", "leverage": "0"}]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", no_amount), DEMO_USDM, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
    // No row for the symbol, and a failed read.
    let other = Ok(json!([{"symbol": "ETHUSDT", "positionAmt": "0", "leverage": "0"}]));
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", other), DEMO_USDM, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    let failed = refusal(-1003, "Too many requests");
    let rest = rest_on(Venue::new().on("GET", "/fapi/v2/positionRisk", failed), DEMO_USDM, 2.0);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK]);
}

#[test]
fn an_unreadable_bracket_writes_the_configured_figure_and_a_refusal_is_named() {
    let venue = Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", refusal(-1000, "unknown"))
        .on("POST", "/fapi/v1/leverage", leverage_ok());
    let rest = rest_on(venue, DEMO_USDM, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2));
    // The venue refuses the write: the sentence carries its words and the hand fix.
    let venue = Venue::new()
        .on("GET", "/fapi/v2/positionRisk", never_traded())
        .on("GET", "/fapi/v1/leverageBracket", tsla_bracket())
        .on("POST", "/fapi/v1/leverage", refusal(-4028, "Leverage 2 is not valid"));
    let rest = rest_on(venue, DEMO_USDM, 2.0);
    assert_eq!(
        ensure_symbol_leverage(&rest),
        LeverageHeal::Refused(BinanceLeverageRefusal::FirstWriteRefused {
            symbol: "TSLAUSDT".to_string(),
            figure: 2,
            error: VenueApiError { code: -4028, msg: "Leverage 2 is not valid".to_string() },
        })
    );
    // A configured figure below 1 fits no symbol: nothing is written.
    let rest = rest_on(venue_for_a_write(), DEMO_USDM, 0.5);
    assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)));
    assert_eq!(rest.transport.sent(), [GET_RISK, GET_BRACKET]);
}

/// A write whose answer was lost may have landed, so it is never called refused (S4b BN-3): the row is read back
/// once, a held leverage is the write landing, a `0` (or no read) is unread with the words.
#[test]
fn a_write_with_no_verdict_is_read_back_and_never_called_refused() {
    use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
    let after = |leverage: &str| {
        Ok(json!([{
            "symbol": "TSLAUSDT", "positionAmt": "0.00", "marginType": "cross", "positionSide": "BOTH",
            "leverage": leverage
        }]))
    };
    for code in [E_TIMEOUT_AMBIGUOUS, -1007, 408] {
        let venue = |read_back: Answer| {
            Venue::new()
                .on("GET", "/fapi/v2/positionRisk", never_traded())
                .on("GET", "/fapi/v2/positionRisk", read_back)
                .on("GET", "/fapi/v1/leverageBracket", tsla_bracket())
                .on("POST", "/fapi/v1/leverage", refusal(code, "no verdict"))
        };
        let rest = rest_on(venue(after("2")), DEMO_USDM, 2.0);
        assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::Written(2), "{code}");
        assert_eq!(rest.transport.sent(), [GET_RISK, GET_BRACKET, POST_LEVERAGE, GET_RISK]);
        for read_back in [after("0"), refusal(-1003, "Too many requests")] {
            let rest = rest_on(venue(read_back), DEMO_USDM, 2.0);
            assert!(matches!(ensure_symbol_leverage(&rest), LeverageHeal::Unread(_)), "{code}");
        }
    }
}

#[test]
fn the_mainnet_book_is_never_read_or_written() {
    // The script names nothing: any request would panic.
    let rest = rest_on(Venue::new(), LIVE_USDM, 2.0);
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::LeftAlone);
    assert!(rest.transport.sent().is_empty());
    // A live client on the demo host's address is still a live client: the type decides, not the host.
    let mut rest = rest_on(Venue::new(), LIVE_USDM, 2.0);
    rest.base_url = rest_on(Venue::new(), DEMO_USDM, 2.0).base_url;
    assert_eq!(ensure_symbol_leverage(&rest), LeverageHeal::LeftAlone);
}

#[test]
fn a_tradfi_reject_signs_nothing_and_names_the_hand_step_on_either_network() {
    // The owner's ruling of 2026-11-15 (decision 0230 item 5): the holder accepts the agreement in the app.
    let msg = "Please sign TradFi-Perps agreement contract fapi.";
    for book in [DEMO_USDM, LIVE_USDM] {
        // The script names nothing: any request (a signature above all) would panic.
        let rest = rest_on(Venue::new(), book, 2.0);
        let said = reject_reason(&rest, E_TRADFI_AGREEMENT, msg);
        assert!(rest.transport.sent().is_empty(), "{:?}", book.0);
        assert_eq!(said, agreement_hand_step("TSLAUSDT", msg));
        assert!(
            said.contains("TSLAUSDT") && said.contains("-4411") && said.contains(msg),
            "{said}"
        );
        assert!(said.contains("Binance app") && said.contains("send again"), "{said}");
    }
}

#[test]
fn the_position_at_leverage_refusal_is_worded_for_both_networks() {
    let msg = "Exceeded the maximum allowable position at current leverage.";
    let demo = order_reject_reason(E_POSITION_AT_LEVERAGE, msg, UrlType::Demo);
    assert!(demo.contains("-2027") && demo.contains(msg), "{demo}");
    assert!(demo.contains("given a leverage by the bridge before its first order"), "{demo}");
    let live = order_reject_reason(E_POSITION_AT_LEVERAGE, msg, UrlType::Live);
    assert!(live.contains("Binance app") && live.contains("never traded"), "{live}");
    // Every other code keeps the venue's own text.
    assert_eq!(
        order_reject_reason(-2019, "Margin is insufficient.", UrlType::Demo),
        "Margin is insufficient."
    );
}
