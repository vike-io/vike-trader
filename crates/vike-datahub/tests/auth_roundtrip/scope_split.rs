//! Section 3: the Observe/Control scope split, its pinned classification table and the sweeps.

use super::*;
use std::assert_matches;

// ---- 3. the scope split -------------------------------------------------------------------------

/// The CLASSIFICATION, pinned as a table — the authority is [`required_scope`] and this is what it
/// says, verb by verb. A change to any row is a deliberate change to what an Observe credential can
/// do, and must show up as a diff here.
///
/// ⚠ The rows that matter most are the `Run*` ones. Six of them RETURN answers and read like reads,
/// but compile CLIENT-SUPPLIED RHAI server-side, so they are Control — and `RunNamed` is the one
/// that is Observe, because it carries no field a script could occupy
/// (`docs/decisions/0064-a-named-run-carries-no-source.md`). Read that pair of rows together: what
/// separates them is what the request can CARRY, never that one of them runs something.
#[test]
fn the_verb_scope_classification_is_pinned() {
    let expect = |request: Request, want: VerbScope| {
        assert_eq!(required_scope(&request), want, "{request:?}");
    };
    expect(Request::Hello { proto_version: PROTO_VERSION }, VerbScope::Handshake);
    expect(Request::Auth { scope: Scope::Read, mac: vec![] }, VerbScope::Handshake);

    expect(Request::Ping, VerbScope::Read);
    expect(Request::ListSeries, VerbScope::Read);
    expect(Request::Inventory, VerbScope::Read);
    expect(Request::ListStrategies, VerbScope::Read);
    expect(
        Request::SeriesGaps {
            id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
        },
        VerbScope::Read,
    );
    for r in every_request() {
        match &r {
            Request::LoadBars { .. }
            | Request::ScanQuotes { .. }
            | Request::ScanTrades { .. }
            | Request::ScanBookUpdates { .. }
            | Request::ScanDepth { .. }
            | Request::ScanCohort { .. }
            | Request::ScanPerpMetrics { .. }
            | Request::ScanEquity { .. }
            | Request::ScanExecFills { .. }
            | Request::PropertiesAsOf { .. } => {
                assert_eq!(required_scope(&r), VerbScope::Read, "reads are Observe: {r:?}")
            }
            _ => {}
        }
    }

    // The WRITE verb.
    expect(
        Request::Backfill {
            venue: "binance".into(),
            symbol: "B".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        VerbScope::Write,
    );
    // ...and the RHAI-COMPILING verbs. `RunSlice`/`RunSweep`/`RunWalkforward` need studio DTOs to
    // construct, so the profile-shaped three stand for the family here; the family's membership is
    // exhaustive in `required_scope`, whose match has no `_` arm.
    expect(Request::RunBacktest(String::new()), VerbScope::Write);
    expect(
        Request::RunParamscanProfile { profile_toml: String::new(), rank_by: None, search: None },
        VerbScope::Write,
    );
    expect(Request::RunWalkforwardProfile { profile_toml: String::new() }, VerbScope::Write);
    // ...and the DESTRUCTIVE verb. Control here is NECESSARY and not sufficient: the server refuses
    // it outright when it holds no keys, which `a_keyless_server_serves_no_delete_verb` proves — a
    // scope classification is meaningless on a server that authenticates nothing.
    expect(
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        VerbScope::Write,
    );
    // ...and the ARCHIVE IMPORT, the second store WRITE.
    // `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.2 is the argument, and
    // `required_scope`'s arm its short form: a write (so 0062's route is closed), the CLIENT names
    // the window (so 0058's
    // fourth leg fails, `Backfill`'s exact reason), and two properties no Observe verb has — it
    // steers server-side filesystem reads, and a wrong day spends its key for good. Even a
    // PLAN-ONLY request is Control: the scope is the verb's, never the flag's, so an Observe key
    // cannot even probe which files sit on the box. Moving this line is re-deciding that design.
    let import = Request::ImportArchive(vike_datahub_client::archive::ImportSpec {
        format: "dukascopy-bi5".into(),
        dataset: "EURUSD".into(),
        from_day: None,
        to_day: None,
        bars: Vec::new(),
        dry_run: true,
        verify: false,
    });
    expect(import.clone(), VerbScope::Write);
    assert_eq!(
        vike_datahub_client::proto::plane_of(&import),
        vike_datahub_client::proto::Plane::Data,
        "ImportArchive is served by the DATA daemon, which holds the store and the imports root"
    );
    assert_eq!(vike_datahub_client::proto::request_kind(&import), "ImportArchive");
    // ...and the two verbs that LIST and STOP running backfills —
    // `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
    // The CANCEL is Control although it writes no row: it ends work ANOTHER connection started, so
    // an Observe key that could send it — the desktop's — would stop the operator's backfills while
    // it cannot start one (0052's test admits an Observe verb by a cost that dies with its OWN
    // connection, and this one's whole effect falls on others). 0081 ruled `record rm`, stopping a
    // server-side activity, Control; this is its twin. It destroys nothing, so it is NOT keys-only
    // the way `DeleteSeries` is: `a_keyless_loopback_server_serves_both_registry_verbs` in
    // `crates/vike-datahub/tests/backfill_cancel.rs` holds that half. The LIST is Observe, on 0081's
    // decision 1: it answers from server state and changes nothing. Moving either line is
    // re-deciding 0101.
    let cancel = Request::CancelBackfill {
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        interval: "5s".into(),
    };
    expect(cancel.clone(), VerbScope::Write);
    expect(Request::ListBackfills, VerbScope::Read);
    // ...and the HISTORY-CHANNELS read — `docs/decisions/0102-the-history-channels-read-is-an-
    // observe-verb.md`. Observe: not a write (0062's prior question), the server bounds its whole
    // cost because the request names nothing (0052), it authenticates nowhere (0062 decision 3), and
    // it never calls the credentialed lane nor uses its token — it reports a presence WORD, which is
    // the one thing an Observe key learns (the owner's Q1). Moving this line is re-deciding 0102.
    expect(Request::HistoryChannels, VerbScope::Read);
    assert_eq!(
        vike_datahub_client::proto::plane_of(&Request::HistoryChannels),
        vike_datahub_client::proto::Plane::Data,
        "HistoryChannels is served by the DATA daemon, whose table, store and credential store it reads"
    );
    for r in [Request::ListBackfills, cancel] {
        assert_eq!(
            vike_datahub_client::proto::plane_of(&r),
            vike_datahub_client::proto::Plane::Data,
            "{r:?} is served by the DATA daemon, whose registry it reads"
        );
    }
    // ...and the MARKET-DATA verbs, which are the rows a reader is most likely to want re-argued:
    // `Backfill` sits three lines up as Control and also "just returns data", so "it reads like a
    // read" cannot be the test. The rule that separates them is BOUNDED-BY-AN-OPERATOR-SET-CEILING
    // versus not — a backfill's venue cost is unbounded per request and the CLIENT names the range,
    // while a subscription's is bounded by MD_MAX_KEYS_PER_VENUE / MD_LINGER, which no request can
    // move. `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` is the record,
    // and it also states the posture this inherits (on a KEY-LESS server every verb but
    // `DeleteSeries` is served to whoever reaches the loopback socket, per 0050).
    // ...and the CHART-GAP SEED, which is the row a reader is SECOND-most likely to want
    // re-argued, and the only WRITE on this side of the table. `docs/decisions/0052` predicted in
    // its own *What would reopen this* that "a market-data verb that WRITES … would be a
    // `Backfill`-class write and would take its scope";
    // `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` argues the predicate is the
    // CLASS and that this verb is not in it, on 0052's own cost rule (the client names a SERIES and
    // nothing else — no range, so no cost term is the request's) plus a REACH axis 0052 never
    // needed: the write is additive and idempotent by commit key, contained to the server's own
    // venue and interval sets, and armed by an operator whose UNARMED server still answers the verb
    // and writes nothing. Moving this line is re-deciding that record, not a reclassification.
    expect(
        Request::SeedSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            // 0061 Phase 3 added this field. `None` is the pre-field frame BYTE-IDENTICALLY
            // (`skip_serializing_if`), which is what keeps this sweep a statement about the
            // verb rather than about the field.
            class: None,
        },
        VerbScope::Read,
    );
    // ...and the VENUE CATALOG, the row a reader is THIRD-most likely to want re-argued — because
    // it sits beside a write on the Observe side and is not one.
    // `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` argues that
    // 0058's four-part rule opens with the words "a write verb" and so does not BIND here: nothing
    // reaches the served store, which is why `venue_catalog_verb` is handed no store handle at all
    // while both its neighbours are. On 0052's cost rule it passes further than either: the client
    // names a VENUE, and that dimension is drawn from the gated `vike_model::VENUES` roster, which
    // CLOSES the free-symbol residual 0058 had to declare. Moving this line is re-deciding that
    // record, not a reclassification.
    expect(Request::VenueCatalog { venue: "binance".into() }, VerbScope::Read);
    // ...and its PLANE, for the same reason the market-data rows below pin theirs: a
    // `Plane::Compute` answer would have the DATA daemon — the only one linking venue bridges —
    // refuse its own verb with a wrong-plane message, compiling perfectly and working not at all.
    assert_eq!(
        vike_datahub_client::proto::plane_of(&Request::VenueCatalog { venue: "binance".into() }),
        vike_datahub_client::proto::Plane::Data,
        "VenueCatalog is served by the DATA daemon"
    );
    // ...and **THE NAMED RUN, the row a reader is most likely to think is simply WRONG** — a
    // `Run*` verb on the Observe side, three lines below three `Run*` verbs pinned Control.
    // `docs/decisions/0064-a-named-run-carries-no-source.md` is the record. Read the predicate the
    // Control rows above actually rest on: it names FIELDS — a profile's `[strategy.params].src`,
    // a `WireSpec` — not the act of running. This request carries
    // `vike_datahub_client::named_run::NamedParam` (`i64`/`f64`/`bool`), which has no variant a
    // script could occupy, and it resolves through `vike_user_strategies::named_run::resolve`,
    // whose crate cannot name `vike-script` — a closure
    // `crates/vike-ops/tests/architecture/named_run_closure_gate.rs` holds — so a `src` is UNREAD rather than
    // refused.
    //
    // ⚠ Its COST is the CONDITION of this row rather than a consequence of it. 0064's decision 3
    // found that on the compute daemon NOT ONE dimension a request names is bounded, so the denial
    // of service survives removing the compiler entirely; this verb is Observe because it is the
    // single-point shape (no grid, no trials, no splits — the type has no field a search can
    // occupy) under a window ceiling, an interval set, a symbol validator and a run-slot count.
    // **Adding a field to `NamedRunSpec` is 0064's first reopener**, and moving this line is
    // re-deciding that record.
    expect(
        Request::RunNamed(Box::new(vike_datahub_client::named_run::NamedRunSpec {
            strategy: "buy_hold".into(),
            params: Vec::new(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 3_600_000,
        })),
        VerbScope::Read,
    );
    // ...and the roster it enumerates, which must be answerable to the SAME credential that runs it
    // (0064's decision 7) — otherwise a client names into the dark, which is the
    // empty-answer-versus-refusal lie 0062's decision 5 fences against, one layer out.
    expect(Request::NamedStrategies, VerbScope::Read);
    // ...and their PLANE, which for this pair is the classification most likely to be got wrong in
    // the OTHER direction: the scope is Observe like the data verbs around it, but the plane is
    // COMPUTE. A `Plane::Data` answer would have the compute daemon refuse its own new verbs with a
    // wrong-plane message while the data daemon — which holds no strategy roster and no engine —
    // tried to answer them.
    for r in [
        Request::NamedStrategies,
        Request::RunNamed(Box::new(vike_datahub_client::named_run::NamedRunSpec {
            strategy: "buy_hold".into(),
            params: Vec::new(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 3_600_000,
        })),
    ] {
        assert_eq!(
            vike_datahub_client::proto::plane_of(&r),
            vike_datahub_client::proto::Plane::Compute,
            "{r:?} is served by the COMPUTE daemon (`vike-backend backtest --addr`)"
        );
    }
    expect(Request::MdSubscribe { specs: Vec::new() }, VerbScope::Read);
    expect(
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
        VerbScope::Read,
    );
    // ...and their PLANE, which is the single highest-consequence classification in this change: a
    // `Plane::Compute` answer would have the DATA daemon refuse its own new verbs with a
    // wrong-plane message, compiling perfectly and working not at all.
    for r in [
        Request::MdSubscribe { specs: Vec::new() },
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
    ] {
        assert_eq!(
            vike_datahub_client::proto::plane_of(&r),
            vike_datahub_client::proto::Plane::Data,
            "{r:?} is served by the DATA daemon"
        );
        assert_matches!(
            vike_datahub_client::proto::request_kind(&r),
            "MdSubscribe" | "MdUpdate",
            "{r:?}"
        );
    }
}

/// The sample set used by the pre-auth table actually spans the classification THIS DAEMON can
/// exercise — all three [`VerbScope`]s, with a store WRITE and a store REMOVAL on the Control side.
/// A new verb that shifted the shape of the split would otherwise leave the exhaustive test above
/// exhaustive-looking but blind.
///
/// ⚠ It used to demand a RHAI-COMPILING Control verb in the set too, and that requirement moved
/// with the verbs (ruling 7): the `Run*` family is the COMPUTE daemon's, and
/// `crates/vike-backtest/tests/compute_plane.rs` is where a Control-scoped Rhai compiler is now
/// driven over a socket. What this file keeps is the half that is still true here — Control on this
/// daemon means "changes the store".
#[test]
fn the_sample_set_covers_every_verb_scope_classification() {
    let scopes: Vec<VerbScope> = every_request().iter().map(required_scope).collect();
    for want in [VerbScope::Handshake, VerbScope::Read, VerbScope::Write] {
        assert!(scopes.contains(&want), "the sample set never exercises {want:?}");
    }
    let has_write = every_request()
        .iter()
        .any(|r| matches!(r, Request::Backfill { .. }) && required_scope(r) == VerbScope::Write);
    let has_delete = every_request().iter().any(|r| {
        matches!(r, Request::DeleteSeries { .. }) && required_scope(r) == VerbScope::Write
    });
    assert!(has_write, "the sample set must include the store-WRITE Control verb");
    assert!(has_delete, "the sample set must include the store-REMOVAL Control verb");
    let has_import = every_request()
        .iter()
        .any(|r| matches!(r, Request::ImportArchive(_)) && required_scope(r) == VerbScope::Write);
    assert!(has_import, "the sample set must include the archive-import Control verb");
    // ...and the registry pair, one on each side of the split — the cancel is the one Control verb
    // on this daemon that changes NO stored row, so neither "has_write" above stands in for it.
    assert!(
        every_request().iter().any(|r| matches!(r, Request::CancelBackfill { .. })
            && required_scope(r) == VerbScope::Write),
        "the sample set must include the Control verb that stops a running backfill"
    );
    assert!(
        every_request()
            .iter()
            .any(|r| matches!(r, Request::ListBackfills) && required_scope(r) == VerbScope::Read),
        "the sample set must include the Observe verb that lists running backfills"
    );
    assert!(
        every_request()
            .iter()
            .any(|r| matches!(r, Request::HistoryChannels) && required_scope(r) == VerbScope::Read),
        "the sample set must include the Observe history-channels read"
    );
    // ⚠ **AND THE MODE-SWITCH FAMILY, WHOSE MEMBERSHIP IS SPLIT ON PURPOSE.** `MdUpdate` is an
    // ordinary positional verb and belongs in the sweep; `MdSubscribe` converts the connection into
    // a push stream and would make every later `exchange` in `observe_reads` assert against a
    // heartbeat. Asserting BOTH halves here is what keeps the carve-out distinguishable from an
    // omission — without it, adding two Observe verbs and forgetting to list them reddens nothing,
    // and what goes untested is the pre-auth refusal of the one verb that becomes an unbounded
    // writer.
    assert!(
        every_request().iter().any(|r| matches!(r, Request::MdUpdate { .. })),
        "the sample set must include the market-data SET MUTATION — it is positional and safe here"
    );
    assert!(
        every_request().iter().all(|r| !matches!(r, Request::MdSubscribe { .. })),
        "⚠ `MdSubscribe` must STAY OUT of the sweep — see the comment beside its omission in \
         `every_request`. Its pre-auth refusal, its mode switch and its hub-less refusal each have \
         a dedicated test on a FRESH connection"
    );
    // ...and it must carry NO compute verb: this daemon refuses those before the scope check, so
    // one slipping back into the set would make the auth tests below silently prove a refusal.
    assert!(
        every_request().iter().all(|r| vike_datahub_client::proto::plane_of(r)
            != vike_datahub_client::proto::Plane::Compute),
        "the sample set must stay DATA-plane: the compute verbs are served by `vike-backend \
         backtest --addr` and are refused here before authentication is consulted"
    );
}

/// An OBSERVE connection reads — every read verb answers on ONE long-lived connection, proving the
/// scope check does not close it.
#[test]
fn observe_reads() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Read);
    for request in every_request().into_iter().filter(|r| required_scope(r) == VerbScope::Read) {
        match exchange(&mut s, &request) {
            Response::AuthDenied { reason } => {
                panic!("Observe refused a read {request:?}: {reason}")
            }
            Response::Error(msg) => {
                assert!(!msg.contains("scope"), "Observe refused {request:?} on scope: {msg}")
            }
            _ => {}
        }
    }
}

/// ...and an OBSERVE connection is REFUSED every Control verb this daemon serves — the store WRITE
/// and the store REMOVAL. This is the property the whole scope split exists for: history without a
/// write, which was not expressible before.
///
/// ⚠ The name still says "the rhai compiling verbs" and that half is now the COMPUTE daemon's
/// (ruling 7). It is KEPT rather than renamed because a test name is what a failure report prints
/// and this one has been quoted in review; the assertion below is what moved, and
/// `crates/vike-backtest/tests/compute_plane.rs` is where an Observe connection meets a Rhai
/// compiler now.
#[test]
fn observe_is_refused_the_write_verb_and_the_rhai_compiling_verbs() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Read);
    let controls: Vec<Request> =
        every_request().into_iter().filter(|r| required_scope(r) == VerbScope::Write).collect();
    assert!(!controls.is_empty(), "guard: the filter must not be vacuous");
    for request in controls {
        match exchange(&mut s, &request) {
            Response::Error(msg) => {
                assert!(msg.contains("Control scope"), "the refusal names the scope: {msg}");
                assert!(
                    msg.contains("Backfill store WRITE")
                        && msg.contains("ImportArchive store WRITE")
                        && msg.contains("DeleteSeries"),
                    "...and WHICH Control verbs this daemon has: {msg}"
                );
            }
            other => panic!("Observe was ALLOWED the Control verb {request:?}: {other:?}"),
        }
    }
    // The connection SURVIVED every refusal — an over-scoped verb is a bad request, not a bad
    // connection, so a mixed-verb client is not disconnected for asking.
    match exchange(&mut s, &Request::Ping) {
        Response::Pong => {}
        other => panic!("the connection did not survive the scope refusals: {other:?}"),
    }
}

/// A CONTROL connection does both: the reads AND the Control verbs. (`Backfill` answers a clean
/// refusal here because no collector table is mounted — the point is that it REACHED the verb
/// rather than being stopped at the scope check, which the message distinguishes.)
#[test]
fn control_does_both() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Write);
    for request in every_request()
        .into_iter()
        .filter(|r| matches!(required_scope(r), VerbScope::Read | VerbScope::Write))
    {
        match exchange(&mut s, &request) {
            Response::AuthDenied { reason } => panic!("Control refused {request:?}: {reason}"),
            Response::Error(msg) => assert!(
                !msg.contains("Control scope"),
                "Control was refused {request:?} on scope grounds: {msg}"
            ),
            _ => {}
        }
    }
}

/// An observe-ONLY server (a `NodeKeys` with no control key) refuses a `Control` handshake outright
/// — the closed-gate shape: an absent key is never consulted, so it cannot be an open door.
#[test]
fn a_server_with_no_control_key_refuses_control_auth() {
    let addr = spawn(Some(NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new())));
    let err = DatahubClient::connect_authed(addr, &keys(), Scope::Write)
        .expect_err("control must be refused on an observe-only server");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    // ...and Observe on the same server still works, so the refusal is the missing key and not the
    // server being broken.
    let mut ok = DatahubClient::connect_authed(addr, &keys(), Scope::Read).expect("observe");
    assert_eq!(ok.authenticated_scope(), Some(Scope::Read));
    ok.ping().expect("ping");
}
