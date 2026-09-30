use super::*;
use crate::preflight::REMEDY_CLOCK;
use crate::transition::LEGACY_REGISTRY;

/// A REAL captured body for each wired venue, read from
/// `crates/vike-mount/tests/fixtures/server_time/`. Captured from the CI box on 2026-08-09 (ig's
/// twin lives in that crate — see `crates/bridges/ig/src/rest.rs`'s `parse_server_time_ms`).
fn fixture(name: &str) -> serde_json::Value {
    let raw = match name {
        "binance" => include_str!("../tests/fixtures/server_time/binance.json"),
        "bybit" => include_str!("../tests/fixtures/server_time/bybit.json"),
        "okx" => include_str!("../tests/fixtures/server_time/okx.json"),
        "aster" => include_str!("../tests/fixtures/server_time/aster.json"),
        "deribit" => include_str!("../tests/fixtures/server_time/deribit.json"),
        "hyperliquid" => include_str!("../tests/fixtures/server_time/hyperliquid.json"),
        other => panic!("no captured fixture for {other}"),
    };
    serde_json::from_str(raw).expect("the captured fixture is valid JSON")
}

/// THE completeness gate, and the reason no count of wired venues is written down anywhere:
/// every canonical roster venue is either wired or declared, and adding a bridge crate turns
/// this red until the new venue is classified.
#[test]
fn clock_sources_cover_the_roster() {
    for venue in vike_model::VENUES {
        assert!(
            clock_decl(LEGACY_REGISTRY, venue).is_some(),
            "{venue} is on vike_model::VENUES but has no clock row — wire its clock or \
                 declare why it has none"
        );
    }
}

/// …and the other direction: no row names a venue that is not on the roster (a typo would
/// otherwise silently classify nothing).
#[test]
fn no_clock_source_row_is_off_roster() {
    for (venue, _) in CLOCK_SOURCES {
        assert!(
            vike_model::VENUES.contains(venue),
            "CLOCK_SOURCES names {venue}, which is not in vike_model::VENUES"
        );
    }
    // Rows leave this table as each venue's clock row moves into its bridge's declaration.
    assert!(CLOCK_SOURCES.len() <= vike_model::VENUES.len());
}

#[test]
fn no_venue_is_declared_twice() {
    for (i, (venue, _)) in CLOCK_SOURCES.iter().enumerate() {
        let dupe = CLOCK_SOURCES.iter().skip(i + 1).any(|(other, _)| other == venue);
        assert!(!dupe, "{venue} has two CLOCK_SOURCES rows");
    }
}

/// A wired row must name the endpoint an operator can curl; a declared row must give a REASON,
/// not a shrug. The length floor is deliberate — "n/a" would pass a non-empty check and teach
/// nobody anything.
#[test]
fn every_row_says_something_useful() {
    for venue in vike_model::VENUES {
        let Some(decl) = clock_decl(LEGACY_REGISTRY, venue) else { continue };
        match decl {
            ClockDecl::Wired { endpoint, auth, .. } => {
                assert!(endpoint.contains('/'), "{venue}'s endpoint names no path: {endpoint}");
                // The label and the declared `auth` must not contradict each other: the label
                // is what an operator reads, the field is what the live smoke branches on, and
                // a row whose two halves disagree is a capability table with a false row.
                let labelled_public = endpoint.contains("(public");
                assert_eq!(
                    labelled_public,
                    auth == ClockAuth::Public,
                    "{venue}'s endpoint label and its declared auth disagree: {endpoint}"
                );
            }
            ClockDecl::NotWired { reason, unmeasured_risk } => {
                assert!(
                    reason.len() >= 60,
                    "{venue}'s NotWired reason is too short to be an explanation: {reason}"
                );
                assert!(!reason.contains("TODO"), "{venue}'s reason is a TODO, not a reason");
                if let Some(at_stake) = unmeasured_risk {
                    assert!(
                        at_stake.len() >= 60,
                        "{venue} declares an unmeasured risk without saying what is at stake"
                    );
                }
            }
        }
    }
}

/// The remedy is per-venue BECAUSE it would otherwise be false: only the recv-window venues may
/// mention a recv window, and only they may claim orders are rejected.
#[test]
fn only_recv_window_venues_claim_orders_are_rejected() {
    for venue in vike_model::VENUES {
        let Some(ClockDecl::Wired { risk, .. }) = clock_decl(LEGACY_REGISTRY, venue) else {
            continue;
        };
        let remedy = risk.remedy();
        assert!(remedy.starts_with("sync the host clock"), "{venue}: {remedy}");
        let claims_recv_window = remedy.contains("recv window");
        assert_eq!(
            claims_recv_window,
            risk == ClockRisk::SignedTimestamp,
            "{venue}'s remedy must mention a recv window iff it signs a timestamp: {remedy}"
        );
        if risk == ClockRisk::NoTimestamp {
            assert!(
                remedy.contains("NOT at risk"),
                "{venue} cannot reject an order over clock drift; say so: {remedy}"
            );
        }
    }
}

/// THE policy rule, over the table: a venue may carry a FAIL threshold — the one that degrades
/// it to paper — IFF a drifted clock can actually get its orders rejected. Everything else is a
/// host-health canary that may warn and nothing more.
#[test]
fn only_order_rejecting_venues_can_fail_their_clock_check() {
    for venue in vike_model::VENUES {
        let Some(ClockDecl::Wired { risk, .. }) = clock_decl(LEGACY_REGISTRY, venue) else {
            assert_eq!(
                clock_policy(LEGACY_REGISTRY, venue),
                None,
                "{venue} is unwired and judges nothing"
            );
            continue;
        };
        let policy = clock_policy_of(risk);
        assert_eq!(clock_policy(LEGACY_REGISTRY, venue), Some(policy), "{venue}");
        assert_eq!(
            policy.fail_ms.is_some(),
            risk == ClockRisk::SignedTimestamp,
            "{venue}: only a venue that REJECTS orders over drift may be degraded to paper by \
                 this leg"
        );
        assert!(policy.warn_ms > 0, "{venue} must warn somewhere");
        assert_ne!(policy.remedy, REMEDY_CLOCK, "{venue} must carry its OWN remedy");
    }
}

/// The canary venues warn LATER than the recv-window ones, because what a reading at them
/// measures is dominated by the venue's own clock (hyperliquid: -220..-424 ms across 40
/// samples from an NTP-disciplined box).
#[test]
fn the_canary_threshold_is_looser_than_the_recv_window_one() {
    let signed = clock_policy_of(ClockRisk::SignedTimestamp);
    let nonce = clock_policy_of(ClockRisk::NonceWindow);
    let none = clock_policy_of(ClockRisk::NoTimestamp);
    assert!(nonce.warn_ms > signed.warn_ms);
    // The two canary risks share THRESHOLDS and differ only in words — a nonce window and an
    // absent timestamp cost the same nothing, but an operator is told which one they have.
    assert_eq!((nonce.warn_ms, nonce.fail_ms), (none.warn_ms, none.fail_ms));
    assert_ne!(nonce.remedy, none.remedy, "…and they must not say the same thing");
    assert_eq!(signed.fail_ms, Some(DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(nonce.fail_ms, None);
}

/// ③ IS PURE: a declared venue with nothing at stake answers `NotChecked` with its reason and
/// touches NO network — which is what lets the preflight render it without a probe, a timeout
/// or a warning.
#[test]
fn a_declared_venue_reports_not_checked_without_touching_the_network() {
    let vars = HashMap::new();
    for venue in vike_model::VENUES {
        let Some(ClockDecl::NotWired { reason, unmeasured_risk: None }) =
            clock_decl(LEGACY_REGISTRY, venue)
        else {
            continue;
        };
        let gap = venue_server_time_ms(LEGACY_REGISTRY, venue, &vars, false)
            .expect_err("declared venues never measure");
        assert_eq!(gap, ServerTimeGap::NotChecked(reason), "{venue}");
    }
}

/// ④ AND ITS ONE ROW: a declared venue whose clock IS on the order path answers the OTHER
/// declaration, so the preflight can warn about it instead of printing "not applicable" over
/// the roster's only order-affecting gap. Also pure — no network.
#[test]
fn a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug() {
    let vars = HashMap::new();
    let mut at_risk = 0usize;
    for venue in vike_model::VENUES {
        let Some(ClockDecl::NotWired { reason, unmeasured_risk: Some(at_stake) }) =
            clock_decl(LEGACY_REGISTRY, venue)
        else {
            continue;
        };
        at_risk += 1;
        let gap = venue_server_time_ms(LEGACY_REGISTRY, venue, &vars, false)
            .expect_err("declared venues never measure");
        assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake }, "{venue}");
    }
    assert!(at_risk > 0, "the table has an at-risk row, or this test proves nothing");
}

/// The polymarket row's PREMISE, asserted rather than asserted-about: that venue really does
/// sign a timestamp into every authenticated request. Under the feature this reads the venue
/// crate's own header builder, so the row cannot outlive the fact it claims.
#[cfg(feature = "polymarket")]
#[test]
fn the_polymarket_row_is_at_risk_because_that_venue_signs_a_timestamp() {
    let creds = vike_polymarket::PolymarketCreds {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string(),
        address: "0xabc".to_string(),
        api_key: "key-1".to_string(),
        passphrase: "pass-1".to_string(),
        ..Default::default()
    };
    let headers = vike_polymarket::l2_auth_headers(&creds, 1_700_000_000, "GET", "/x", "")
        .expect("the fixture secret is valid base64url");
    assert!(
        headers.iter().any(|(k, _)| k == "POLY_TIMESTAMP"),
        "polymarket's row claims its clock is on the order path — prove it"
    );
    let Some(ClockDecl::NotWired { unmeasured_risk, .. }) =
        clock_decl(LEGACY_REGISTRY, "polymarket")
    else {
        panic!("polymarket is declared, not wired");
    };
    assert!(unmeasured_risk.is_some(), "…so its row must declare the risk, not shrug");
}

/// An unknown venue string is NOT silently "declared" — it is an error that names itself.
#[test]
fn an_unknown_venue_is_unreachable_not_declared() {
    let gap = venue_server_time_ms(LEGACY_REGISTRY, "not-a-venue", &HashMap::new(), false)
        .expect_err("unknown venue");
    match gap {
        ServerTimeGap::Unreachable(e) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

/// The wired venues that need a credential to READ the clock are exactly the ones whose live
/// gate already resolved one — so an uncredentialed box can never turn a wired row into a
/// spurious ② "the venue did not answer". (ig is the only such row today; the assert is over
/// the table, not over that fact.)
#[test]
fn a_credentialed_clock_read_reports_its_own_missing_key() {
    let vars = HashMap::new();
    let e = ig_time(&vars, false).expect_err("no IG credentials in an empty map");
    let (api_key_var, _, _) = vike_ig::ig_env_var_names(Environment::Demo);
    assert!(e.contains(&api_key_var), "the message must name what is missing, not the URL: {e}");
    assert!(!e.contains("http"), "no URL in a report line: {e}");
}

// ---- the PARSE, against real captured bodies (CI, no network) ------------------------------

/// Every wired parser against the REAL body its venue answered, with the exact stamp pinned.
/// This is the test that was missing: the parses were inline in the fetchers, so a renamed
/// field or a wrong unit could only be found by the `#[ignore]`d live smoke.
#[test]
fn every_parser_reads_its_venues_real_captured_body() {
    assert_eq!(parse_binance_shaped_time(&fixture("binance")), Ok(1_786_242_369_664));
    assert_eq!(parse_binance_shaped_time(&fixture("aster")), Ok(1_786_242_370_439));
    assert_eq!(parse_bybit_time(&fixture("bybit")), Ok(1_786_242_369_911));
    assert_eq!(parse_okx_time(&fixture("okx")), Ok(1_786_242_370_157));
    assert_eq!(parse_deribit_time(&fixture("deribit")), Ok(1_786_242_370_599));
    assert_eq!(parse_hyperliquid_time(&fixture("hyperliquid")), Ok(1_786_242_384_206));
}

/// THE UNIT TRAP, pinned at the two venues that ship a wrong-unit field in the SAME body: a
/// mix-up here reports a thousand- or million-fold skew and looks authoritative. Each assert
/// names the neighbouring field the parse must NOT read.
#[test]
fn no_parser_reads_a_neighbouring_field_in_the_wrong_unit() {
    // bybit: `result.timeNano` is NANOSECONDS as a string, `result.timeSecond` SECONDS.
    let bybit = fixture("bybit");
    let ns = bybit["result"]["timeNano"].as_str().expect("the capture carries timeNano");
    let secs = bybit["result"]["timeSecond"].as_str().expect("…and timeSecond");
    let read = parse_bybit_time(&bybit).expect("the ms number parses");
    assert_ne!(read.to_string(), ns, "timeNano is nanoseconds — a millionfold error");
    assert_ne!(read.to_string(), secs, "timeSecond is seconds — a thousandfold error");
    assert_eq!(read / 1_000, secs.parse::<i64>().unwrap(), "…and it agrees with them");
    assert_eq!(read, ns.parse::<i64>().unwrap() / 1_000_000);

    // deribit: `usIn`/`usOut` are MICROSECONDS beside the ms `result`.
    let deribit = fixture("deribit");
    let us_in = deribit["usIn"].as_i64().expect("the capture carries usIn");
    let read = parse_deribit_time(&deribit).expect("the ms result parses");
    assert_ne!(read, us_in, "usIn is microseconds — a thousandfold error");
    assert_eq!(read, us_in / 1_000);

    // okx: the value is a STRING inside an ARRAY, so the naive `as_i64()` reads nothing.
    let okx = fixture("okx");
    assert!(okx["data"][0]["ts"].as_i64().is_none(), "precondition: it is a string");
    assert_eq!(parse_okx_time(&okx), Ok(1_786_242_370_157));
}

/// A body that answered but carries no stamp is an ERROR naming the field, never a zero or a
/// panic — and every parser is total over garbage.
#[test]
fn a_body_without_the_stamp_names_the_missing_field() {
    let empty = serde_json::json!({});
    assert_eq!(parse_binance_shaped_time(&empty), Err(missing("serverTime")));
    assert_eq!(parse_bybit_time(&empty), Err(missing("time")));
    assert_eq!(parse_hyperliquid_time(&empty), Err(missing("time")));
    assert_eq!(parse_okx_time(&empty), Err(missing("data[0].ts")));
    // okx with an EMPTY data array — the shape that would index out of bounds.
    assert_eq!(
        parse_okx_time(&serde_json::json!({"code":"0","data":[],"msg":""})),
        Err(missing("data[0].ts"))
    );
    // okx with the ts as a NUMBER (the shape a future API change might send): still refused
    // rather than silently mis-read.
    assert_eq!(
        parse_okx_time(&serde_json::json!({"data":[{"ts":1_786_242_370_157i64}]})),
        Err(missing("data[0].ts"))
    );
}

/// deribit's row measures the TESTNET host every exec spawn site binds, and its body says so.
/// A mainnet body — `testnet: false` — is refused rather than reported as a reading against a
/// host this mount never talks to.
#[test]
fn a_deribit_body_from_the_wrong_host_is_refused() {
    let mut mainnet = fixture("deribit");
    mainnet["testnet"] = serde_json::Value::Bool(false);
    let e = parse_deribit_time(&mainnet).expect_err("wrong host");
    assert!(e.contains("wrong host"), "{e}");
    // …and an absent flag is treated the same way, not as an implicit pass.
    let mut absent = fixture("deribit");
    absent.as_object_mut().expect("object").remove("testnet");
    assert!(parse_deribit_time(&absent).is_err());
}
