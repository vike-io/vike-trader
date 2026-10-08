//! The shared acceptance path (real core + real audit trail), read verbs and housekeeping.

use super::*;

// ---------------------------------------------------------------------------------------------
// The shared acceptance path (real core + real audit trail)
// ---------------------------------------------------------------------------------------------

/// A Telegram-origin command and a TCP-origin one are gated by the SAME code over the SAME
/// `ControlLimits` bucket, and both leave the same shape of audit record. Proven by wiring this
/// channel's `preview`/`accept` hooks to the real [`accept_command`] over one shared bucket:
///
/// - a Telegram write that busts the notional cap is refused at PREVIEW with the SERVER's own
///   message, and never mints a token;
/// - the rate bucket is genuinely shared — one TCP accept plus one Telegram accept exhausts a
///   2-command budget, and the next TCP accept is rate-limited;
/// - both accepted commands appear in the audit trail, each with its own rationale.
#[test]
fn accept_command_shared_by_both_surfaces() {
    test_init();
    let mount = build_paper_maker_core(&MakerMountConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let sink = mount.handle.command_sink();

    // ONE bucket, both surfaces: two commands per second, nothing over $1000 notional.
    let limits = Arc::new(Mutex::new(ControlLimits::new(ControlLimitsConfig {
        max_notional: Some(1_000.0),
        rate_per_sec: 2.0,
    })));
    let preview_limits = Arc::clone(&limits);
    let accept_limits = Arc::clone(&limits);
    let accept_sink = sink.clone();
    let deps = StubDeps::new()
        // `&[], &[]`: this stub surface publishes no engine blocks and no orders (the
        // `accept_command` call below passes the same), so the preview sizes at multiplier 1.0 —
        // the twin of that call.
        .with_preview(Box::new(move |cmd| {
            preview_limits.lock().unwrap().preview_vet(cmd, &[], &[])
        }))
        .with_accept(Box::new(move |cmd, reason| {
            // `peer: None` — this surface has no socket; the origin rides in `reason`. `key_id:
            // None` for the same reason: this channel authenticates a chat id, not a node key.
            // `engines: &[]` and `route_keys: &[]` — this stub surface publishes no engine roster
            // of either shape, which `venue_refusal` and `account_refusal` both read as UNKNOWN and
            // refuse nothing on. Empty TOGETHER is the production shape too: both rosters project
            // one `portfolio.venues`. The routing gates have their own suites
            // (`crates/vike-tradehub/tests/daemon/venue_routing.rs` for the wire path and
            // `crates/vike-tradehub/src/server/tests.rs`'s `account_refusal` for the pure
            // verdict), and THIS surface's own roster plumbing is proven by
            // `the_production_deps_preview_reads_their_own_route_keys` and its `accept` twin
            // below — two tests because the two methods reach the gate by different routes; this
            // one is about the SHARED limiter.
            accept_command(
                cmd,
                Some(reason),
                &mut accept_limits.lock().unwrap(),
                &accept_sink,
                None,
                &[],
                &[],
                &[],
                &[],
                None,
                None,
            )
            .map(vike_tradehub::server::control::Accepted::into_coid)
            .map_err(|e| e.message())
        }));
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let peer: std::net::SocketAddr = "127.0.0.1:65000".parse().unwrap();

    // (a) The notional cap refuses at PREVIEW — the same cap, the same message a TCP peer sees, and
    // no confirmation token is minted for a command that could never pass. Consumes no rate token.
    deps.push_batch(vec![update(1, CHAT, "/submit hyperliquid BTC buy 100 50")]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert!(report.previewed.is_empty(), "an over-cap command mints NO confirmation token");
    assert_eq!(report.refused.len(), 1);
    assert!(
        report.refused[0].contains("max_notional_per_order"),
        "the server's own refusal, naming the POLICY key (settings unification, Phase 5 — the old \
         VIKE_TRADEHUB_* variable no longer exists), not a Telegram-local message: {}",
        report.refused[0]
    );
    assert!(pending.is_empty());

    // (b) A within-cap command previews (still no rate token consumed) …
    deps.push_batch(vec![update(2, CHAT, "/cancel tg-a")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.push_batch(vec![update(3, CHAT, &format!("/confirm {token}"))]);

    // (c) … and now the three accepts run BACK-TO-BACK over the shared 2-token bucket, so the
    // refill (2/s) cannot meaningfully move between them: TCP spends token 1, Telegram spends
    // token 2, and the next TCP command is rate-limited BY THE SAME LIMITER. That is the proof the
    // two surfaces are not merely similar but literally the same gate.
    let coid = accept_command(
        WireCommand::Cancel("tcp-a".into()),
        Some("tcp: pulling the quote"),
        &mut limits.lock().unwrap(),
        &sink,
        None,
        &[],
        &[],
        &[],
        &[],
        Some(peer),
        None,
    )
    .expect("the first command is within budget")
    .into_coid();
    assert_eq!(coid, "tcp-a");

    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["tg-a".to_string()]);

    let err = accept_command(
        WireCommand::Cancel("tcp-b".into()),
        Some("tcp: one too many"),
        &mut limits.lock().unwrap(),
        &sink,
        None,
        &[],
        &[],
        &[],
        &[],
        Some(peer),
        None,
    )
    .expect_err("the shared 2/s budget is exhausted");
    assert!(err.message().contains("rate limited"), "{}", err.message());
    assert!(!err.is_fatal(), "a rate limit never closes the surface");

    // (d) Both surfaces left the same shape of audit record.
    let tcp = audit_entry_for("tcp-a").expect("the TCP command was audited");
    assert_eq!(tcp.kind, "cancel");
    assert_eq!(tcp.reason.as_deref(), Some("tcp: pulling the quote"));
    let tg = audit_entry_for("tg-a").expect("the Telegram command was audited");
    assert_eq!(tg.kind, "cancel");
    assert!(
        tg.reason.as_deref().is_some_and(|r| r.starts_with("telegram chat ")),
        "the Telegram record names its origin (peer is None for a non-socket surface): {:?}",
        tg.reason
    );
}

/// The operator's literal instruction is what the audit trail records — that is the whole point of
/// routing the chat text into the v4 `reason` field. It reaches `audit::record` SANITIZED (control
/// characters stripped) via the shared `accept_command`, exactly as a TCP peer's rationale does.
#[test]
fn chat_text_becomes_audit_reason() {
    test_init();
    let mount = build_paper_maker_core(&MakerMountConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let sink = mount.handle.command_sink();
    let limits = Arc::new(Mutex::new(ControlLimits::new(ControlLimitsConfig::default())));
    let deps = StubDeps::new().with_accept(Box::new(move |cmd, reason| {
        accept_command(
            cmd,
            Some(reason),
            &mut limits.lock().unwrap(),
            &sink,
            None,
            &[],
            &[],
            &[],
            &[],
            None,
            None,
        )
        .map(vike_tradehub::server::control::Accepted::into_coid)
        .map_err(|e| e.message())
    }));
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    // A rationale a human would actually type — including a newline, which the sanitizer must strip
    // before it can reach the structured JSON audit line.
    let instruction = "/cancel AUDIT-COID-1\nliquidity thinning ahead of the print";
    deps.push_batch(vec![update(1, CHAT, instruction)]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["AUDIT-COID-1".to_string()]);

    let entry = audit_entry_for("AUDIT-COID-1").expect("the command was audited");
    let reason = entry.reason.expect("a Telegram command always carries a rationale");
    assert!(
        reason.contains("liquidity thinning ahead of the print"),
        "the operator's literal words ARE the audit rationale: {reason}"
    );
    assert!(
        reason.contains(&format!("telegram chat {CHAT}")),
        "…and it names the origin: {reason}"
    );
    assert!(
        !reason.contains('\n') && !reason.contains('\r'),
        "the shared path sanitized it — no line terminator can reach the audit line: {reason:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Read verbs + housekeeping
// ---------------------------------------------------------------------------------------------

/// Read verbs answer straight from the snapshot: no token, no confirm, nothing lowered. Plain
/// chatter and non-text updates are consumed silently (the offset still advances) so a bot sitting
/// in a busy group is not a noise source.
#[test]
fn read_verbs_answer_without_touching_the_core() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/status"),
        update(2, CHAT, "/positions"),
        update(3, CHAT, "/orders"),
        update(4, CHAT, "/equity"),
        update(5, CHAT, "morning"),
        TgUpdate {
            update_id: 6,
            chat_id: CHAT,
            from_id: USER,
            from_username: None,
            text: String::new(),
        },
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.replies, 4, "only the four read verbs are answered");
    assert!(deps.accepted().is_empty());
    assert!(report.previewed.is_empty());
    let texts: Vec<String> = deps.replies().into_iter().map(|(_, t)| t).collect();
    assert_eq!(texts, ["read:Status", "read:Positions", "read:Orders", "read:Equity"]);
    assert_eq!(led.last(), 6, "every update, answered or not, advances the offset");
}

/// An unrecognized or malformed `/verb` is answered with the usage line — and nothing else happens.
#[test]
fn unknown_and_malformed_instructions_only_get_usage() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/wat"),
        update(2, CHAT, "/submit hyperliquid"),
        update(3, CHAT, "/state sideways"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.replies, 3);
    assert!(report.previewed.is_empty() && report.executed.is_empty());
    assert!(deps.accepted().is_empty());
    for (_, text) in deps.replies() {
        assert!(text.contains("/confirm <token>"), "every answer carries the usage line: {text}");
    }
}

/// A refusal from the core (the `accept_command` error path) is reported back and the command is
/// gone — a failed confirm is never silently retried behind the operator's back.
#[test]
fn a_refused_confirm_reports_and_drops_the_command() {
    let deps =
        StubDeps::new().with_accept(Box::new(|_cmd, _reason| Err("core busy, retry".to_string())));
    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert!(report.executed.is_empty());
    assert_eq!(report.refused, vec!["core busy, retry".to_string()]);
    assert!(pending.is_empty(), "the token was spent even though the core refused");
    let last = deps.replies().last().cloned().expect("a reply").1;
    assert!(last.contains("REFUSED") && last.contains("core busy"), "{last}");
}

/// A submit reaching the shared path with an EMPTY client-order-id is refused (the remote-submit
/// idempotency policy). The Telegram grammar mints a coid at preview time and can never produce
/// one, so this pins the shared path's own guard rather than the channel's.
#[test]
fn an_empty_coid_submit_is_refused_by_the_shared_path() {
    let mount = build_paper_maker_core(&MakerMountConfig::outcome_token(
        "polymarket",
        TOKEN,
        Some(RESOLUTION_TS),
    ));
    let sink = mount.handle.command_sink();
    let mut limits = ControlLimits::new(ControlLimitsConfig::default());
    let err = accept_command(
        WireCommand::Submit(WireOrderRequest {
            client_order_id: String::new(),
            venue: "polymarket".into(),
            symbol: TOKEN.into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(0.4),
            trigger_price: None,
            reduce_only: false,
            account: None,
        }),
        None,
        &mut limits,
        &sink,
        None,
        &[],
        &[],
        &[],
        &[],
        None,
        None,
    )
    .expect_err("an empty coid is refused");
    assert!(err.message().contains("pre-minted client_order_id"), "{}", err.message());
    assert!(!err.is_fatal());
}
