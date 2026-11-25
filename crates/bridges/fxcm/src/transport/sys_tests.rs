use super::*;

/// With no shim installed — which is every CI box and every fresh clone — a login is
/// `Unavailable` rather than a panic, a hang, or a link error.
///
/// ⚠ Conditional on the shim's absence rather than unconditional, and this is the difference
/// the whole change is about: before 2026-09-09 this test was `#[cfg(all(test, not(fcsdk)))]`,
/// so on the ONE box that had the SDK it was not compiled at all. Now it compiles everywhere
/// and asserts the right thing for the box it is on.
#[test]
fn a_box_without_the_shim_reports_unavailable() {
    if crate::sdk_available() {
        return; // this box HAS the shim — `crates/bridges/fxcm/tests/fxcm_smokes/fxcm_live_smoke.rs` is its test
    }
    assert_eq!(
        FxcmSession::login("u", "p", "http://x", "Demo").err(),
        Some(FxcmError::Unavailable)
    );
}

#[test]
fn side_codes() {
    assert_eq!(OrderSideWire::Buy.code(), "B");
    assert_eq!(OrderSideWire::Sell.code(), "S");
}

/// The two messages measured against the SDK's own sample clients on 2026-09-22, and the third
/// class the same ORA-499 family produces. All three are [`LoginFailureClass::Reported`] and are
/// told apart by the TEXT — so what this pins is that the text SURVIVES to the operator, which
/// is the whole deliverable.
#[test]
fn a_reported_failure_shows_the_venues_own_words() {
    let dead_account = FxcmError::LoginFailed {
        class: LoginFailureClass::Reported,
        message: "User or connection doesn't exist.".into(),
    };
    let shown = dead_account.to_string();
    assert!(
        shown.contains("User or connection doesn't exist."),
        "the SDK's own text must reach the operator verbatim: {shown}"
    );
    assert!(shown.contains("the venue said"), "…and be attributed to the venue: {shown}");

    // The bad-connection-name / unreachable-host family: a different string, so an operator
    // reading the message can tell it from the one above without this crate parsing anything.
    let bad_connection = FxcmError::LoginFailed {
        class: LoginFailureClass::Reported,
        message: redact_login_text(
            "ORA-499: Unable to obtain station descriptor. HTTP request failed \
                 object='/Hosts.jsp?ID=abc&PN=NotAConnection&SN=ForexConnect&MV=5&LN=D999&AT=PLAIN' \
                 errorCode=503",
        ),
    };
    let shown = bad_connection.to_string();
    assert!(shown.contains("ORA-499"), "the SDK's code must survive: {shown}");
    assert!(shown.contains("errorCode=503"), "…and so must the HTTP code: {shown}");
    assert_ne!(shown, dead_account.to_string(), "the three classes must read differently");
}

/// Every class, so a loop over them cannot silently cover five of six.
///
/// Held against the enum by [`pinned_label`], whose `match` is exhaustive and carries no `_`
/// arm: a new [`LoginFailureClass`] is a COMPILE error there, and the author fixing it is
/// looking straight at this array.
const ALL_CLASSES: [LoginFailureClass; 6] = [
    LoginFailureClass::Reported,
    LoginFailureClass::Disconnected,
    LoginFailureClass::NoResponse,
    LoginFailureClass::SdkInitFailed,
    LoginFailureClass::TradingSessionRequired,
    LoginFailureClass::ShimPredatesErrorReporting,
];

/// The six labels, spelled AGAIN, by hand, independently of [`LoginFailureClass::label`].
///
/// ⚠ **This exists because the assertion it feeds could not fail, and a mutation proved it.**
/// The old check was `assert!(shown.contains(class.label()))` — but [`FxcmError`]'s `Display`
/// BUILDS `shown` by calling `class.label()`, so the test compared a renderer's output to its
/// own input, and `contains("")` is true of every string. MEASURED 2026-09-22: with all six
/// arms of `label()` emptied the test PASSED, while three sibling mutations in this file were
/// killed. A renderer can only be checked against a SECOND, independent statement of the same
/// fact, which is what this is.
///
/// It is the one hand copy in this crate that is the point rather than the hazard — and it is
/// not unguarded either: `crates/bridges/fxcm/tests/fxcm_login_triage_gate.rs` holds
/// `docs/ops/fxcm-forexconnect.md`'s operator triage table against the SAME arms, so the code,
/// this pin and the page are three spellings that cannot drift apart in silence.
///
/// The `match` is exhaustive and has NO catch-all: that is the only mechanism by which a pin
/// keeps up with the enum it pins.
fn pinned_label(class: LoginFailureClass) -> &'static str {
    match class {
        LoginFailureClass::Reported => "the venue named a reason",
        LoginFailureClass::Disconnected => "disconnected with no reason given",
        LoginFailureClass::NoResponse => "no answer from ForexConnect",
        LoginFailureClass::SdkInitFailed => "the ForexConnect SDK did not initialise",
        LoginFailureClass::TradingSessionRequired => "a trading session must be selected",
        LoginFailureClass::ShimPredatesErrorReporting => {
            "this box's shim predates login error reporting"
        }
    }
}

/// Every class NAMES itself, in the words [`pinned_label`] pins — including the two the shim
/// used to answer with a 30-second silence, and the one an OLD installed shim gives.
#[test]
fn every_login_failure_class_says_what_it_is() {
    for class in ALL_CLASSES {
        let expected = pinned_label(class);
        assert!(!expected.is_empty(), "{class:?} must have a non-empty label to render");
        assert_eq!(
            class.label(),
            expected,
            "{class:?}'s label moved. It is read by an operator mid-incident and copied into \
                 `docs/ops/fxcm-forexconnect.md`'s triage table, so changing it is a three-place \
                 edit: here, `label()`, and that page."
        );

        // ⚠ `Reported` is the ONE class whose `Display` deliberately omits the label: the
        // venue's own words lead instead, which is the whole deliverable. Its rendering is
        // asserted by `a_reported_failure_shows_the_venues_own_words`.
        if class == LoginFailureClass::Reported {
            continue;
        }
        let shown = FxcmError::LoginFailed { class, message: "detail".into() }.to_string();
        assert!(shown.contains(expected), "{class:?} must render its label: {shown}");
        assert!(shown.contains("detail"), "{class:?} must carry its message: {shown}");
    }
}

/// No two classes read the same, so an operator who has the label has the class.
///
/// The equality above catches any divergence between `label()` and the pin, and it is blind to
/// exactly one thing: a COLLISION introduced on both sides at once. Two classes given the same
/// sentence satisfy six equality checks and leave a log line that names an answer without
/// identifying it, which is the whole failure this diagnostic exists to end.
///
/// The loop's own coverage is held by [`ALL_CLASSES`]'s declared length, not by a count here —
/// a `seen.len()` assertion after an unconditional `push` is true by construction, which is
/// the shape of assertion this file has already been caught writing once.
#[test]
fn no_two_login_failure_classes_read_the_same() {
    let mut seen: Vec<&str> = Vec::new();
    for class in ALL_CLASSES {
        let label = class.label();
        assert!(
            !seen.contains(&label),
            "{class:?} renders `{label}`, which another class already renders — an operator \
                 reading a log line could not tell them apart"
        );
        seen.push(label);
    }
}

/// **A failed login's kind is read from the shim's class and the SDK's own words, and only a failure that
/// provably never reached FXCM is transient**: a host discovery with no HTTP answer (`errorCode=56`, measured in the
/// FX weekend close) and no answer inside the shim's wait. The measured credential refusal is `Auth`; a host
/// discovery the host ANSWERED (`errorCode=503`, measured: a connection name that names no station) and every text
/// that cannot be told apart park as `Unknown`.
#[test]
fn a_login_is_transient_only_when_it_never_reached_fxcm() {
    let reported = |message: &str| FxcmError::LoginFailed {
        class: LoginFailureClass::Reported,
        message: message.to_string(),
    };
    let station = |code: &str| {
        reported(&format!(
            "ORA-499: Unable to obtain station descriptor. HTTP request failed \
             object='/Hosts.jsp?<query redacted: it carries the FXCM login name>' errorCode={code}"
        ))
    };
    for (failure, kind) in [
        (station("56"), ErrorKind::Network),
        (station("503"), ErrorKind::Unknown),
        (reported("Login failed. Incorrect user name or password"), ErrorKind::Auth),
        (reported("User or connection doesn't exist."), ErrorKind::Unknown),
        (
            FxcmError::LoginFailed { class: LoginFailureClass::NoResponse, message: "x".into() },
            ErrorKind::Timeout,
        ),
        (
            FxcmError::LoginFailed { class: LoginFailureClass::Disconnected, message: "x".into() },
            ErrorKind::Unknown,
        ),
        (FxcmError::Unavailable, ErrorKind::Unknown),
    ] {
        assert_eq!(failure.kind(), kind, "{failure}");
    }
}

/// **A table read FXCM did not answer inside the shim's wait is transient; one the venue failed parks**: `-8` is the
/// shim's "no answer" (`fcshim.cpp`'s `refreshTable`), `-6` the venue's own failure in words that do not tell a
/// transient failure from a refusal.
#[test]
fn a_table_read_is_transient_only_when_fxcm_did_not_answer() {
    let native =
        |code: i32, message: &str| FxcmError::Native { code, message: message.to_string() };
    assert_eq!(
        native(-8, "timeout waiting for the refresh-table response").kind(),
        ErrorKind::Timeout
    );
    assert_eq!(native(-6, "request failed").kind(), ErrorKind::Unknown);
    assert_eq!(native(-7, "the response is not an Offers table").kind(), ErrorKind::Unknown);
}

/// The shim's integer classes map one-for-one, and an UNKNOWN code — a NEWER shim under an
/// older binary — degrades to "no answer" rather than being folded into a class it is not.
#[test]
fn an_unknown_class_code_is_not_silently_adopted() {
    assert_eq!(LoginFailureClass::from_code(1), LoginFailureClass::Reported);
    assert_eq!(LoginFailureClass::from_code(2), LoginFailureClass::Disconnected);
    assert_eq!(LoginFailureClass::from_code(3), LoginFailureClass::NoResponse);
    assert_eq!(LoginFailureClass::from_code(4), LoginFailureClass::SdkInitFailed);
    assert_eq!(LoginFailureClass::from_code(5), LoginFailureClass::TradingSessionRequired);
    for unknown in [0, 6, 99, -1] {
        assert_eq!(
            LoginFailureClass::from_code(unknown),
            LoginFailureClass::NoResponse,
            "class {unknown} is not a class this build knows"
        );
    }
}

/// The login NAME never reaches a log line or a journalled reject reason, and the parts that
/// make the message diagnosable always do.
#[test]
fn the_login_name_is_redacted_out_of_a_quoted_request_url() {
    let raw = "ORA-499: Unable to obtain station descriptor. HTTP request failed \
                   object='/Hosts.jsp?ID=nonce&PN=Demo&SN=ForexConnect&MV=5&LN=D000000000&AT=PLAIN' \
                   errorCode=503";
    let red = redact_login_text(raw);
    assert!(!red.contains("D000000000"), "the login name must not survive: {red}");
    assert!(!red.contains("LN="), "nor the parameter that carries it: {red}");
    assert!(red.contains("object='/Hosts.jsp"), "the request PATH must survive: {red}");
    assert!(red.contains("errorCode=503"), "and so must the HTTP code: {red}");
    assert!(red.contains("ORA-499"), "and the SDK's own code: {red}");
    assert!(red.contains(REDACTED_QUERY), "the cut must be visible, not silent: {red}");

    // A TRUNCATED buffer has no closing quote, and the query is exactly what a truncation
    // leaves behind — so the redaction has to fire there too rather than fall through.
    let cut = "HTTP request failed object='/Hosts.jsp?ID=nonce&LN=D2511";
    let red = redact_login_text(cut);
    assert!(!red.contains("D2511"), "a truncated URL must still be redacted: {red}");
    assert!(red.contains("object='/Hosts.jsp"), "…keeping the path: {red}");

    // A message with no URL in it — every OTHER login failure — is passed through untouched.
    let plain = "User or connection doesn't exist.";
    assert_eq!(redact_login_text(plain), plain);
}

/// A failed table read says WHY, and an old shim that says nothing still produces a message.
///
/// The shim (`fcshim.cpp`'s `refreshTable`) writes the reason of a failed refresh-table request into
/// the buffer it was handed and returns a negative code, so a timeout or a refused request reaches
/// the operator in the SDK's own words instead of as "table snapshot failed". The shim the CI box has
/// installed predates that and leaves the buffer untouched, so the fallback is the case that runs
/// until the new shim is deployed.
#[test]
fn a_failed_table_read_carries_the_shims_reason_and_falls_back_for_an_old_shim() {
    assert_eq!(
        failure_text("timeout waiting for the refresh-table response", "table snapshot failed"),
        "timeout waiting for the refresh-table response"
    );
    // An empty or whitespace-only buffer is not a reason.
    assert_eq!(failure_text("", "table snapshot failed"), "table snapshot failed");
    assert_eq!(
        failure_text("  \n", "forex_connect_account failed"),
        "forex_connect_account failed"
    );
    // An SDK string is scrubbed like every other one: the login name never reaches a log line.
    let red = failure_text(
        "HTTP request failed object='/Hosts.jsp?ID=n&LN=D000000000&AT=PLAIN' errorCode=503",
        "x",
    );
    assert!(!red.contains("D000000000"), "the login name must not survive: {red}");
    assert!(red.contains("errorCode=503"), "the diagnosable part must: {red}");
}

/// The table-read failure renders as `FXCM native error <code>: <reason>`, so the code (the
/// `fcshim.cpp` return-code table) and the words travel together.
#[test]
fn a_native_table_failure_renders_its_code_and_its_reason() {
    let e = FxcmError::Native {
        code: -8,
        message: failure_text(
            "timeout waiting for the refresh-table response",
            "table snapshot failed",
        ),
    };
    assert_eq!(
        e.to_string(),
        "FXCM native error -8: timeout waiting for the refresh-table response"
    );
}

/// A shim-filled C string buffer holding `text`, NUL-terminated.
fn filled(text: &str) -> Vec<c_char> {
    text.bytes().map(|b| b as c_char).chain([0]).collect()
}

/// **FX-1: a write FXCM failed with an internal error (`ORA-20001`) is OUTCOME UNKNOWN, never a refusal.** A placement
/// keeps the RequestID the shim hands back on `-6`, so `confirm` can look the order up (`None` from a shim older than
/// that, which writes none); a close and an amend have nothing to look up by. Every other code stays the venue's refusal,
/// the rollover's `ORA-20131` included (nothing happened, so it is refused NOW).
#[test]
fn a_write_fxcm_failed_with_an_internal_error_is_unknown_and_a_placement_keeps_its_request_id() {
    let (request, offer, account, none) =
        (filled("REQ-7"), filled("OFFER-1"), filled("ACC-9"), filled(""));
    let sent = Sent { side: OrderSideWire::Buy, amount: 1000, limit: true, rate: 1.1 };
    let internal = "ORA-20001: Internal system error";
    let failed = placement_failure(-6, internal.into(), [&request, &offer, &account], &sent);
    assert!(failed.outcome_unknown());
    assert_eq!(
        failed.into_lookup(),
        Some(PendingLookup {
            request_id: "REQ-7".into(),
            account_id: "ACC-9".into(),
            offer_id: "OFFER-1".into(),
            side: OrderSideWire::Buy,
            amount: 1000,
            limit: true,
            rate: 1.1,
        })
    );
    let old_shim = placement_failure(-6, internal.into(), [&none, &offer, &account], &sent);
    assert!(old_shim.outcome_unknown());
    assert_eq!(old_shim.into_lookup(), None);
    for refused in ["ORA-20102: Access Violation", "ORA-20131: account locked", "request failed"] {
        assert_eq!(
            placement_failure(-6, refused.into(), [&request, &offer, &account], &sent),
            FxcmError::Native { code: -6, message: refused.into() },
            "{refused}"
        );
    }
    assert_eq!(
        closing_reply(-6, &none, &none, &none, &filled(internal), "close failed").unwrap_err(),
        FxcmError::SentUnknown { code: -6, message: internal.into(), lookup: None }
    );
    let gone = "ORA-20185: cannot close an already closed position";
    assert_eq!(
        closing_reply(-6, &none, &none, &none, &filled(gone), "close failed").unwrap_err(),
        FxcmError::Native { code: -6, message: gone.into() }
    );
    assert!(write_failure(-6, internal.into()).outcome_unknown(), "an amend reads the order back");
    assert!(write_failure(-11, "no answer".into()).outcome_unknown());
    assert!(
        !write_failure(-6, "ORA-20105: Order price did not pass validation.".into())
            .outcome_unknown()
    );
}

/// **FX-2: a cancel that was SENT and got no answer is OUTCOME UNKNOWN, never a refusal**: the shim's `FC_SENT_NO_ANSWER`, the
/// `-8` an older shim answered that same timeout with (the export's only `-8`), and FXCM's internal error. What never left
/// (`-1`/`-4`/`-5`) and the venue's own refusal stay definite.
#[test]
fn a_cancel_sent_without_an_answer_is_unknown_and_a_refusal_stays_one() {
    for (rc, words) in [
        (-11, "outcome UNKNOWN, the cancel was sent"),
        (-8, "timeout"),
        (-6, "ORA-20001: Internal system error"),
    ] {
        assert_eq!(
            cancel_failure(rc, words.into()),
            FxcmError::SentUnknown { code: rc, message: words.into(), lookup: None },
            "{rc}"
        );
    }
    for (rc, words) in
        [(-1, ""), (-4, ""), (-5, "no request"), (-6, "ORA-20136: order being executed")]
    {
        assert_eq!(
            cancel_failure(rc, words.into()),
            FxcmError::Native { code: rc, message: words.into() },
            "{rc}"
        );
    }
}

/// **A cancel of an order FXCM no longer holds is no refusal**: `ORA-20186` (no order with that id) and `ORA-20169` (no
/// conditional order to change) are `AlreadyGone`, decided on the code; it filled or was cancelled, and its own event says
/// which. The same code outside FXCM's failure, and any other failure, stay what `cancel_failure` makes of them.
#[test]
fn a_cancel_of_an_order_fxcm_no_longer_holds_is_already_gone() {
    assert_eq!(cancel_answer(0, String::new()), Ok(OrderCancelAck::Canceled));
    for gone in [
        "ORA-20186: order with the specified id does not exist",
        "DAS 19915: ZDas Exception ORA-20169: conditional order to be changed does not exist",
    ] {
        assert_eq!(cancel_answer(-6, gone.into()), Ok(OrderCancelAck::AlreadyGone), "{gone}");
    }
    for (rc, words) in [
        (-6, "ORA-20136: order being executed"),
        (-6, "order does not exist"),
        (-11, "ORA-20186: order does not exist"),
    ] {
        assert_eq!(
            cancel_answer(rc, words.into()),
            Err(cancel_failure(rc, words.into())),
            "{rc} {words}"
        );
    }
}

// ---- the table reads over a scripted shim (finding 3 of the second review) -------------------------------------

/// A scripted table entry point: writes `json` into the buffer it is handed (truncated and NUL-terminated, as
/// `emit_json` does), returns the FULL length, and counts its calls.
fn scripted<'a>(
    json: &'a str,
    calls: &'a std::cell::Cell<u32>,
) -> impl FnMut(&mut [c_char]) -> i32 + 'a {
    move |buffer: &mut [c_char]| {
        calls.set(calls.get() + 1);
        let bytes = json.as_bytes();
        let n = bytes.len().min(buffer.len() - 1);
        for (slot, byte) in buffer.iter_mut().zip(&bytes[..n]) {
            *slot = *byte as c_char;
        }
        buffer[n] = 0;
        bytes.len() as i32
    }
}

/// A full-size Offers table: the shim's row is ~240 bytes and FXCM lists a few hundred instruments.
fn offers_table(rows: usize) -> String {
    let row = r#"{"instrument":"EUR/USD","bid":1.09980000,"ask":1.10020000,"digits":5,"instrument_type":1,"contract_currency":"EUR","contract_multiplier":1.00000000,"subscription":"T","trading_status":"O","bid_tradable":"T","ask_tradable":"T","base_unit":1000,"quote_age_ms":120}"#;
    format!("[{}]", vec![row; rows].join(","))
}

/// **An Offers read is ONE shim call, however long the table.** Every `forex_connect_offers` call refreshes the table from
/// the venue, so the old first buffer (8 KiB: about 34 rows) made every real read two refreshes, and the table
/// returned was the second one. 400 rows is ~100 KiB, well past the old buffer.
#[test]
fn an_offers_read_of_a_few_hundred_rows_is_one_shim_call() {
    let table = offers_table(400);
    assert!(table.len() > TABLE_FIRST_BUFFER * 10, "the fixture must dwarf the old buffer");
    let calls = std::cell::Cell::new(0);
    let got = read_table(scripted(&table, &calls), OFFERS_BUFFER, Overflow::Fail).expect("a table");
    assert_eq!(got, table);
    assert_eq!(calls.get(), 1, "one call is one venue refresh");
}

/// A table that does not fit even the Offers buffer is an error naming the sizes, and the shim is NOT called
/// again (a second call would be a second refresh): fail closed, never a silently truncated table.
#[test]
fn an_offers_table_past_the_buffer_is_an_error_and_not_a_second_refresh() {
    let table = offers_table(400);
    let calls = std::cell::Cell::new(0);
    let err = read_table(scripted(&table, &calls), 4096, Overflow::Fail).unwrap_err();
    assert_eq!(calls.get(), 1);
    let text = err.to_string();
    assert!(text.contains(&table.len().to_string()) && text.contains("4096"), "{text}");
}

/// The reconcile reads keep their one regrow: the same table twice, at the exact size the shim asked for.
#[test]
fn a_reconcile_table_that_overflows_is_read_again_at_the_exact_size() {
    let table = offers_table(5);
    let calls = std::cell::Cell::new(0);
    let got = read_table(scripted(&table, &calls), 256, Overflow::Regrow).expect("a table");
    assert_eq!(got, table);
    assert_eq!(calls.get(), 2);
    let fits = std::cell::Cell::new(0);
    read_table(scripted(&table, &fits), TABLE_FIRST_BUFFER, Overflow::Regrow).expect("a table");
    assert_eq!(fits.get(), 1, "a table that fits is one call");
}

/// A negative return is a failed read carrying the shim's own words, never an empty table.
#[test]
fn a_failed_table_read_is_an_error_with_the_shims_words() {
    let err = read_table(
        |buffer| {
            let words = b"timeout waiting for the refresh-table response\0";
            for (slot, byte) in buffer.iter_mut().zip(words) {
                *slot = *byte as c_char;
            }
            -8
        },
        OFFERS_BUFFER,
        Overflow::Fail,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "FXCM native error -8: timeout waiting for the refresh-table response"
    );
}

/// A login waits out what is left of `LOGOUT_SETTLE` after the last logout, and not at all when no
/// session was logged out recently: the replacement start of 2026-10-24 09:11 UTC logged in right after
/// the old adapter's logout and FXCM answered `User or connection doesn't exist.` to a valid account.
#[test]
fn a_login_after_a_logout_waits_out_the_rest_of_the_settle_time() {
    let now = Instant::now();
    assert_eq!(settle_wait(None, now), Duration::ZERO, "a fresh process has no logout to wait for");
    assert_eq!(settle_wait(Some(now), now), LOGOUT_SETTLE, "a logout this instant: the whole wait");
    let two_s_ago = now.checked_sub(Duration::from_secs(2)).expect("two seconds before now");
    assert_eq!(settle_wait(Some(two_s_ago), now), LOGOUT_SETTLE - Duration::from_secs(2));
    let long_ago = now.checked_sub(LOGOUT_SETTLE + Duration::from_secs(1)).expect("before now");
    assert_eq!(settle_wait(Some(long_ago), now), Duration::ZERO, "a stale logout costs nothing");
    let later = now + Duration::from_secs(1);
    assert_eq!(
        settle_wait(Some(later), now),
        LOGOUT_SETTLE,
        "a logout stamped after `now` never waits longer"
    );
}
