//! Integration test for the cTrader connection actor's two-stage auth handshake, exercised
//! end-to-end against an in-process fake cTrader server (plaintext, no TLS). Mirrors the
//! PROVEN-LIVE sequence from `scratchpad/ctrader_catcher.py::prove`:
//! ApplicationAuth -> GetAccountListByAccessToken (pick first demo ctid) -> AccountAuth ->
//! Trader (money_digits) -> SymbolsList -> SymbolById (for per-symbol digits/scale).

mod common;

use std::sync::Arc;

use vike_ctrader::conn::{ConnConfig, ConnError, connect_and_auth};

use common::{AccountsScript, FakeCtrader, NoopSink};

#[test]
fn connects_and_completes_two_stage_auth() {
    // Fake server scripts: AppAuthRes, GetAccountsRes(one demo ctid=99), AccountAuthRes,
    // TraderRes(money_digits=2), SymbolsListRes(EURUSD id=1), SymbolByIdRes(id=1 digits=5).
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    assert_eq!(handle.ctid, 99);
    assert_eq!(handle.symbols.id_of("EURUSD"), Some(1));
    assert_eq!(handle.symbols.name_of(1), Some("EURUSD"));
    assert_eq!(handle.symbols.scale(1), 100000.0);
    server.assert_saw(&[
        "APPLICATION_AUTH_REQ",
        "GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ",
        "ACCOUNT_AUTH_REQ",
        "TRADER_REQ",
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
