use super::*;

/// One OPEN wire position in symbol 1, for the position-book provenance tests below.
fn wire_open(position_id: i64, volume: i64) -> crate::proto::ProtoOaPosition {
    crate::proto::ProtoOaPosition {
        position_id,
        position_status: crate::proto::ProtoOaPositionStatus::PositionStatusOpen as i32,
        trade_data: crate::proto::ProtoOaTradeData {
            symbol_id: 1,
            volume,
            trade_side: crate::proto::ProtoOaTradeSide::Buy as i32,
            open_timestamp: Some(10),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// ⚠ **A `RECONCILE_RES` for ANOTHER account is not this account's answer.** It used to be
/// folded in wholesale — a stranger's positions replacing ours and the result marked
/// AUTHORITATIVE, so a stranger's flat book manufactured this account's `Flat` and refused its
/// exit under `halt_admit = "verify"`. Delete the `res.ctid_trader_account_id != ctid` guard in
/// [`rebuild_position_book`] and this goes red, as does
/// `crates/bridges/ctrader/tests/exec_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` end to end.
#[test]
fn a_reconcile_answer_for_another_ctid_is_discarded_and_clears_the_evidence() {
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));

    // Our own answer lands and is authoritative.
    let mine = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&mine, 99, &positions);
    {
        let book = positions.lock().expect("lock");
        assert!(book.is_fetched());
        assert_eq!(book.len(), 1);
    }

    // A stranger's (flat) answer must change NOTHING except to withdraw the evidence claim.
    let theirs = ProtoOaReconcileRes {
        ctid_trader_account_id: 12_345,
        position: vec![],
        ..Default::default()
    };
    rebuild_position_book(&theirs, 99, &positions);
    let book = positions.lock().expect("lock");
    assert_eq!(book.len(), 1, "our routing entries must survive a foreign answer, not be wiped");
    assert!(
        !book.is_fetched(),
        "we asked and what came back was not about us — whatever we believed is now of \
             unknown standing, so it may not refuse anything"
    );
}

/// ⚠ **The completeness discipline does not stop at the reconcile.** The evidence flag is set
/// ONCE and the book then drifts forwards on execution events for the life of the socket, so an
/// event carrying a position ref this build cannot CLASSIFY would otherwise remove a live row
/// while the book went on calling itself authoritative — and that manufactured absence refuses
/// the position's own exit. Delete the `invalidate()` in [`update_position_map`]'s hole arm and
/// this goes red.
#[test]
fn an_unclassifiable_position_ref_on_an_event_withdraws_the_evidence_claim() {
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 40_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    assert!(positions.lock().expect("lock").is_fetched());

    // A CLOSED position is KNOWLEDGE: drop the entry, keep the evidence claim.
    let mut closed = wire_open(8, 0);
    closed.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusClosed as i32;
    update_position_map(
        &ProtoOaExecutionEvent { position: Some(closed), ..Default::default() },
        &positions,
    );
    {
        let book = positions.lock().expect("lock");
        assert_eq!(book.len(), 1, "the closed position is gone");
        assert!(book.is_fetched(), "the venue TOLD us it closed — that is knowledge, not a hole");
    }

    // An ERROR position is a HOLE: drop the entry AND stop being evidence.
    let mut broken = wire_open(7, 100_000);
    broken.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusError as i32;
    update_position_map(
        &ProtoOaExecutionEvent { position: Some(broken), ..Default::default() },
        &positions,
    );
    let book = positions.lock().expect("lock");
    assert!(
        !book.is_fetched(),
        "a position ref this build cannot classify is a hole — the book must stop refusing \
             exits on the absence it just created"
    );
}

#[test]
fn conn_config_debug_redacts_secrets() {
    let cfg = ConnConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
    assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
    assert!(debug.contains("my-client-id"), "client_id should stay visible: {debug}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("refresh_token: \"None\""), "unset refresh_token stays None: {debug}");
}

#[test]
fn conn_config_debug_redacts_refresh_token_when_set() {
    let mut cfg = ConnConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    cfg.refresh_token = Some("super-secret-refresh-token".to_string());
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
}

/// Build an `ERROR_RES`-typed `ProtoMessage` carrying `error_code`, for [`is_auth_error`]
/// unit tests below. The full network round-trip (`oauth::refresh` against a real cTrader
/// OAuth endpoint) is NOT exercised here — it can't run offline — and stays covered only by
/// the live smoke test (`tests/ctrader_demo_smoke.rs`); this covers just the pure classifier.
fn error_res_msg(error_code: &str) -> ProtoMessage {
    let err = ProtoOaErrorRes { error_code: error_code.to_string(), ..Default::default() };
    ProtoMessage {
        payload_type: pt::ERROR_RES,
        payload: Some(err.encode_to_vec()),
        client_msg_id: None,
    }
}

#[test]
fn is_auth_error_true_for_auth_and_token_error_codes() {
    assert!(is_auth_error(&error_res_msg("OA_AUTH_TOKEN_EXPIRED")));
    assert!(is_auth_error(&error_res_msg("ACCESS_TOKEN_INVALID")));
    // Case-insensitive: the classifier upper-cases before matching.
    assert!(is_auth_error(&error_res_msg("access_token_invalid")));
}

#[test]
fn is_auth_error_false_for_unrelated_error_code() {
    assert!(!is_auth_error(&error_res_msg("SYMBOL_NOT_FOUND")));
    assert!(!is_auth_error(&error_res_msg("ORDER_REJECTED")));
}

#[test]
fn is_auth_error_false_for_non_error_res_payload_type() {
    // Only `ERROR_RES` frames are ever classified — any other payload type is never an auth
    // error regardless of what bytes happen to be in its payload.
    let mut msg = error_res_msg("OA_AUTH_TOKEN_EXPIRED");
    msg.payload_type = pt::HEARTBEAT_EVENT;
    assert!(!is_auth_error(&msg));
}

#[test]
fn is_transient_retries_network_faults() {
    // A failed connect / reset / EOF-mid-handshake and a no-response timeout are the blips the
    // bounded connect-retry rides out.
    assert!(ConnError::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset")).is_transient());
    assert!(
        ConnError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "closed mid-handshake"))
            .is_transient()
    );
    assert!(ConnError::Timeout(pt::APPLICATION_AUTH_RES).is_transient());
}

#[test]
fn is_transient_fails_fast_on_permanent_faults() {
    // Auth rejection is the crux permanent case — a bad credential never becomes good by
    // waiting, so the retry must NOT burn attempts on it.
    assert!(
        !ConnError::Venue {
            error_code: "CH_CLIENT_AUTH_FAILURE".into(),
            description: "invalid client credentials".into(),
        }
        .is_transient()
    );
    assert!(!ConnError::NoAccounts.is_transient());
    assert!(!ConnError::Decode("schema mismatch".into()).is_transient());
    assert!(!ConnError::Tls("rustls config".into()).is_transient());
}

#[test]
fn connect_retry_default_is_bounded_and_short() {
    // The default policy must stay bounded (so a dead venue yields to paper) and cheap (a few
    // seconds total), or the "eventually fall back to paper" contract regresses.
    let r = ConnectRetry::default();
    assert!(r.max_attempts >= 1);
    assert!(r.max_attempts <= 6, "keep the initial connect bounded");
    assert!(r.initial_backoff <= r.max_backoff);
    assert!(r.max_backoff <= Duration::from_secs(5), "cap the per-retry wait");
}
