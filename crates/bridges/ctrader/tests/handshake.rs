//! Integration test for the cTrader connection actor's two-stage auth handshake, exercised
//! end-to-end against an in-process fake cTrader server (plaintext, no TLS). Mirrors the
//! PROVEN-LIVE sequence from `scratchpad/ctrader_catcher.py::prove`:
//! ApplicationAuth -> GetAccountListByAccessToken (pick first demo ctid) -> AccountAuth ->
//! Trader (money_digits) -> SymbolCategoryList -> AssetClassList -> AssetList (the broker
//! reference lists the catalog reads a class and base/quote from) -> SymbolsList -> SymbolById
//! (for per-symbol digits/scale).

mod common;

use std::sync::Arc;

use vike_ctrader::conn::{ConnConfig, ConnError, connect_and_auth};
use vike_ctrader::symbols::SymbolCategory;
use vike_model::AssetClass;

use common::{AccountsScript, FakeCtrader, NoopSink};

#[test]
fn connects_and_completes_two_stage_auth() {
    // Fake server scripts: AppAuthRes, GetAccountsRes(one demo ctid=99), AccountAuthRes,
    // TraderRes(money_digits=2), the three reference lists (category 1 -> asset class `Forex`;
    // assets EUR/USD), SymbolsListRes(EURUSD id=1), SymbolByIdRes(id=1 digits=5).
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    assert_eq!(handle.ctid, 99);
    assert_eq!(handle.symbols.id_of("EURUSD"), Some(1));
    assert_eq!(handle.symbols.name_of(1), Some("EURUSD"));
    assert_eq!(handle.symbols.scale(1), 100000.0);
    assert_eq!(
        handle.symbols.category(1),
        Some(SymbolCategory { id: 1, name: "Default Category", asset_class: Some("Forex") })
    );
    assert_eq!(
        (handle.symbols.base_asset(1), handle.symbols.quote_asset(1)),
        (Some("EUR"), Some("USD"))
    );
    server.assert_saw(&[
        "APPLICATION_AUTH_REQ",
        "GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ",
        "ACCOUNT_AUTH_REQ",
        "TRADER_REQ",
        "SYMBOL_CATEGORY_REQ",
        "ASSET_CLASS_LIST_REQ",
        "ASSET_LIST_REQ",
        "SYMBOLS_LIST_REQ",
        "SYMBOL_BY_ID_REQ",
    ]);
}

/// A broker that REFUSES the three reference lists still completes the handshake: those lists only
/// name what a symbol is, and must not keep a live mount from trading. Its symbols read as
/// uncategorised — no category, no asset names, no class on the risk grid.
#[test]
fn refused_reference_lists_leave_symbols_uncategorised_not_the_handshake_failed() {
    let server = FakeCtrader::start_reject_reference_lists();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok despite refusals");
    assert_eq!(handle.symbols.id_of("EURUSD"), Some(1));
    assert_eq!(handle.symbols.category_id(1), Some(1), "the light symbol's id is still read");
    assert_eq!(handle.symbols.category(1), None);
    assert_eq!((handle.symbols.base_asset(1), handle.symbols.quote_asset(1)), (None, None));
    let grid = handle.symbols.risk_properties("EURUSD").expect("known symbol → a grid");
    assert_eq!(grid.asset_class, None::<AssetClass>);
    server.assert_saw(&[
        "SYMBOL_CATEGORY_REQ",
        "ASSET_CLASS_LIST_REQ",
        "ASSET_LIST_REQ",
        "SYMBOLS_LIST_REQ",
        "SYMBOL_BY_ID_REQ",
    ]);
}

#[test]
fn no_demo_account_is_rejected_not_fallen_back_to_live() {
    // Fake server scripts GetAccountsRes with ONLY a live account (ctid=77, is_live=true) — the
    // handshake must surface `ConnError::NoAccounts` rather than silently authorizing it.
    let server = FakeCtrader::start(AccountsScript::OnlyLive);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    match connect_and_auth(cfg, Arc::new(NoopSink)) {
        Err(ConnError::NoAccounts) => {}
        Err(other) => panic!("expected NoAccounts, got {other:?}"),
        Ok(_) => panic!("expected NoAccounts, but connect_and_auth succeeded"),
    }
}
