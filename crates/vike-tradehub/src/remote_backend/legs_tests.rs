//! The preflight legs as the fold reads them: the policy a declared risk class is judged by, the
//! reasons a declared gap carries, the gaps that are WARN rows and never green ones, the worst-of
//! rule for a venue with several accounts, and the network reading's numbers.

use vike_gatewayhub_client::proto::{ClockRiskInfo, PreflightLists};
use vike_tradehub_core::preflight::{
    CheckStatus, FnProbes, PreflightConfig, check_clock_skew, check_credentials,
};

use super::*;

const NOW: i64 = 1_800_000_000_000;

fn clock(venue: &str, leg: ClockLeg) -> LegResult {
    LegResult::Clock { venue: venue.to_string(), leg, reason: None, at_stake: None }
}

fn credential(venue: &str, leg: CredentialLeg) -> LegResult {
    LegResult::Credential { venue: venue.to_string(), label: None, leg }
}

/// **The class decides who may FAIL.** A signed-timestamp venue carries the global warn/fail pair; a
/// nonce-window or no-timestamp venue warns at the canary line and has no fail line at all. The remedy is
/// the daemon's sentence, kept verbatim.
#[test]
fn only_a_signed_timestamp_venue_may_fail() {
    let signed = clock_policy_of(ClockRiskClass::SignedTimestamp, "sync it (signed)");
    assert_eq!(signed.warn_ms, DEFAULT_CLOCK_WARN_MS);
    assert_eq!(signed.fail_ms, Some(DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(signed.remedy, "sync it (signed)");
    for class in [ClockRiskClass::NonceWindow, ClockRiskClass::NoTimestamp] {
        let policy = clock_policy_of(class, "sync it (canary)");
        assert_eq!(policy.warn_ms, CANARY_CLOCK_WARN_MS, "{class:?}");
        assert_eq!(policy.fail_ms, None, "{class:?}");
        assert_eq!(policy.remedy, "sync it (canary)");
    }
}

/// The plan's policies are the lists' wired venues, each with its own class and remedy.
#[test]
fn the_plan_yields_a_policy_per_wired_venue() {
    let lists = PreflightLists {
        clock_risks: [
            (
                "binance".to_string(),
                ClockRiskInfo { class: ClockRiskClass::SignedTimestamp, remedy: "a".into() },
            ),
            (
                "hyperliquid".to_string(),
                ClockRiskInfo { class: ClockRiskClass::NonceWindow, remedy: "b".into() },
            ),
        ]
        .into(),
        wired_clock_reads: 2,
        credential_venues: vec!["binance".into()],
    };
    let policies = policies_of(&lists);
    assert_eq!(policies.len(), 2);
    assert_eq!(policies["binance"].fail_ms, Some(DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(policies["hyperliquid"].fail_ms, None);
    assert_eq!(policies["hyperliquid"].remedy, "b");
}

/// **A reading is the venue's stamp as the daemon read it**, and the REAL check judges it under the
/// venue's policy: 3 s out FAILS a signed-timestamp venue and only WARNS a nonce-window one.
#[test]
fn a_reading_is_the_venues_stamp_and_the_policy_judges_it() {
    let legs = [clock(
        "binance",
        ClockLeg::ServerTime { server_ms: NOW + 3_000, local_ms: NOW, rtt_ms: 20 },
    )];
    assert_eq!(server_time_of("binance", &legs), Ok(NOW + 3_000));
    for (class, want) in [
        (ClockRiskClass::SignedTimestamp, CheckStatus::Fail),
        (ClockRiskClass::NonceWindow, CheckStatus::Warn),
    ] {
        let mut cfg = PreflightConfig::default();
        cfg.clock_policies.insert("binance".to_string(), clock_policy_of(class, "r"));
        let stamp = server_time_of("binance", &legs);
        let probes = FnProbes::new()
            .with_now_ms(|| NOW)
            .with_venue_server_time_ms(move |_: &str| stamp.clone());
        assert_eq!(check_clock_skew("binance", &cfg, &probes, None).status, want, "{class:?}");
    }
}

/// **A declared gap carries the row's own words**: the reason of a `NotChecked`, the reason and the
/// cost of an `UnmeasuredRisk`. A gatewayhub that stated none still yields a gap with a sentence.
#[test]
fn a_declared_gap_carries_its_reason() {
    let legs = [
        LegResult::Clock {
            venue: "ig".into(),
            leg: ClockLeg::NotChecked,
            reason: Some("ig publishes no clock".into()),
            at_stake: None,
        },
        LegResult::Clock {
            venue: "polymarket".into(),
            leg: ClockLeg::UnmeasuredRisk,
            reason: Some("polymarket publishes no clock".into()),
            at_stake: Some("its signature binds the timestamp".into()),
        },
        clock("deribit", ClockLeg::NotChecked),
    ];
    assert_eq!(
        server_time_of("ig", &legs),
        Err(ServerTimeGap::NotChecked("ig publishes no clock"))
    );
    assert_eq!(
        server_time_of("polymarket", &legs),
        Err(ServerTimeGap::UnmeasuredRisk {
            reason: "polymarket publishes no clock",
            at_stake: "its signature binds the timestamp",
        })
    );
    assert!(matches!(
        server_time_of("deribit", &legs),
        Err(ServerTimeGap::NotChecked(reason)) if reason.contains("stated no reason")
    ));
}

/// **Every leg the gatewayhub could not give is a WARN or a plain gap, never a green row.**
#[test]
fn the_gaps_are_never_green() {
    let legs = [clock("bybit", ClockLeg::Unreachable("timed out".into()))];
    assert_eq!(
        server_time_of("bybit", &legs),
        Err(ServerTimeGap::Unreachable("timed out".to_string()))
    );
    // A venue the gatewayhub returned no leg for: a WARN-class gap naming the venue.
    let Err(ServerTimeGap::Unreachable(why)) = server_time_of("okx", &legs) else {
        panic!("a missing clock leg must be Unreachable (WARN), never a reading")
    };
    assert!(why.contains("okx"), "{why}");
    // A leg of ANOTHER venue is no answer for this one.
    assert!(matches!(server_time_of("okx", &[]), Err(ServerTimeGap::Unreachable(_))));
}

/// **A query that got no answer makes EVERY read a non-green gap** that names the failure: the clock
/// leg WARNs, the credential leg is `Unanswered` (a WARN that stops nothing).
#[test]
fn an_unanswered_query_is_a_warn_everywhere_and_a_pass_nowhere() {
    let Err(ServerTimeGap::Unreachable(why)) =
        Err::<i64, _>(clock_unanswered("connection refused"))
    else {
        panic!("a failed query must not yield a clock reading")
    };
    assert!(why.contains("connection refused"), "{why}");
    let CredentialGap::Unanswered { waited_ms, detail } =
        credential_unanswered("connection refused")
    else {
        panic!("a failed query must be Unanswered, which never stops a venue and is never green")
    };
    assert_eq!(waited_ms, 0);
    assert!(detail.contains("connection refused"), "{detail}");
}

/// **The worst of a venue's accounts decides**: a refusal beats a silence beats an acceptance, and a
/// venue with no credential leg at all is a plain `Rejected` (the wiring's own word for an unplanned
/// probe, as the in-process backend answers).
#[test]
fn a_venue_with_several_accounts_is_judged_by_the_worst() {
    let accepted = || credential("binance", CredentialLeg::Accepted);
    let silent = || {
        credential(
            "binance",
            CredentialLeg::Unanswered { waited_ms: 5_000, detail: "no answer".into() },
        )
    };
    let rejected = || credential("binance", CredentialLeg::Rejected("-2015 invalid key".into()));

    assert_eq!(authed_read_of("binance", &[accepted(), accepted()]), Ok(()));
    assert_eq!(
        authed_read_of("binance", &[accepted(), silent()]),
        Err(CredentialGap::Unanswered { waited_ms: 5_000, detail: "no answer".into() })
    );
    for order in [vec![accepted(), silent(), rejected()], vec![rejected(), silent(), accepted()]] {
        assert_eq!(
            authed_read_of("binance", &order),
            Err(CredentialGap::Rejected("-2015 invalid key".into())),
            "a refusal wins whatever its position"
        );
    }
    assert!(matches!(authed_read_of("binance", &[]), Err(CredentialGap::Rejected(_))));
    assert!(
        matches!(
            authed_read_of("binance", &[credential("okx", CredentialLeg::Accepted)]),
            Err(CredentialGap::Rejected(_))
        ),
        "another venue's acceptance is no answer for this one"
    );

    // …and through the REAL check: a refusal is a FAIL, a silence a WARN, an acceptance a PASS.
    for (legs, want) in [
        (vec![accepted()], CheckStatus::Pass),
        (vec![silent()], CheckStatus::Warn),
        (vec![rejected()], CheckStatus::Fail),
    ] {
        let answer = authed_read_of("binance", &legs);
        let probes = FnProbes::new().with_venue_authed_read(move |_: &str| answer.clone());
        assert_eq!(check_credentials("binance", &probes, None).status, want);
    }
}

/// The network leg: alive carries its rounds, dead its down-since stamp, and absence is UNKNOWN (a
/// WARN). A daemon that gave no number reads one round / down since now.
#[test]
fn the_network_reading_carries_its_numbers() {
    use vike_tradehub_core::NetReading as Fold;
    let net = |reading, rounds, since_ms| vec![LegResult::Network { reading, rounds, since_ms }];
    assert_eq!(
        net_reading_of(&net(WireNetReading::Alive, Some(7), None), NOW),
        Fold::Alive { rounds: 7 }
    );
    assert_eq!(
        net_reading_of(&net(WireNetReading::Dead("dns".into()), None, Some(NOW - 5)), NOW),
        Fold::Dead { since_ms: NOW - 5 }
    );
    assert_eq!(
        net_reading_of(&net(WireNetReading::Alive, None, None), NOW),
        Fold::Alive { rounds: 1 }
    );
    assert_eq!(
        net_reading_of(&net(WireNetReading::Dead("dns".into()), None, None), NOW),
        Fold::Dead { since_ms: NOW }
    );
    assert_eq!(net_reading_of(&net(WireNetReading::Unknown, None, None), NOW), Fold::Unknown);
    assert_eq!(net_reading_of(&[], NOW), Fold::Unknown);
}

/// Interning keeps one copy per text, bounds a hostile one and never grows past its cap (over a table of
/// its own, so the process-wide one is not spent).
#[test]
fn a_reason_is_interned_once_and_bounded() {
    let table = Mutex::new(Vec::new());
    let a = intern_in(&table, 3, "the same sentence");
    let b = intern_in(&table, 3, "the same sentence");
    assert!(std::ptr::eq(a, b), "one copy per distinct text");
    let long = "é".repeat(MAX_REASON_BYTES);
    let cut = intern_in(&table, 3, &long);
    assert!(cut.len() <= MAX_REASON_BYTES && long.starts_with(cut), "cut at a character boundary");
    let _ = intern_in(&table, 3, "a third");
    assert_eq!(intern_in(&table, 3, "past the cap"), OVERFLOW);
    assert_eq!(
        intern_in(&table, 3, "the same sentence"),
        "the same sentence",
        "a kept text is still found past the cap"
    );
}
