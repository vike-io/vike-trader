//! The four gates and the chat/user authorization.

use super::*;

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

/// With ANY of the four gates closed, NOTHING is constructed — and crucially the workspace `.env`
/// is not even READ, so the bot token never enters the process. That is the property the
/// function-not-value `load_vars` parameter exists to make provable.
#[test]
fn all_gates_absent_constructs_nothing() {
    // Counters the two lazily-invoked constructors bump, so "nothing was constructed" is an
    // observation rather than an assumption.
    let loads = Arc::new(AtomicUsize::new(0));
    let builds = Arc::new(AtomicUsize::new(0));
    // A perfectly USABLE ledger throughout, so every `None` below is attributable to the gate under
    // test and never to the ledger precondition. It is never even opened — the file's absence at
    // the end of this test is itself the proof that a shut gate short-circuits before it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = ledger_path_in(dir.path());

    let closed: &[(Option<&str>, Option<&str>)] = &[
        (None, None),              // neither flag
        (Some("1"), None),         // tradehub control only
        (None, Some("1")),         // telegram control only
        (Some("1"), Some("true")), // a fuzzy truthy spelling arms NOTHING …
        (Some("true"), Some("1")), // … in either position
        (Some("0"), Some("1")),    // an explicit off
        (Some("1"), Some("")),     // set-but-blank is not "1"
        (Some("1"), Some(" 1 ")),  // untrimmed is not the EXACT string
    ];
    for (tradehub, telegram) in closed {
        assert!(
            !control_gates_open(*tradehub, *telegram),
            "{tradehub:?}/{telegram:?} must be shut"
        );
        let (l, b) = (Arc::clone(&loads), Arc::clone(&builds));
        let handle = maybe_spawn(
            *tradehub,
            *telegram,
            || {
                l.fetch_add(1, Ordering::Relaxed);
                HashMap::new()
            },
            Some(path.clone()),
            |_cfg| {
                b.fetch_add(1, Ordering::Relaxed);
                Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
            },
        );
        assert!(handle.is_none(), "{tradehub:?}/{telegram:?} must not spawn a poller");
    }
    assert_eq!(
        loads.load(Ordering::Relaxed),
        0,
        "the workspace .env must NOT be read while a process-env gate is shut — that is what keeps \
         the bot token out of memory"
    );
    assert_eq!(builds.load(Ordering::Relaxed), 0, "no agent/deps may be constructed either");

    // Both flags open, but NO credentials in the `.env`: the loader runs (that is how we learn),
    // the deps are still never built and no thread is spawned.
    let (l, b) = (Arc::clone(&loads), Arc::clone(&builds));
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        || {
            l.fetch_add(1, Ordering::Relaxed);
            HashMap::new()
        },
        Some(path.clone()),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_none(), "absent credentials are the gate — no channel");
    assert_eq!(loads.load(Ordering::Relaxed), 1, "the loader runs exactly once on the open path");
    assert_eq!(builds.load(Ordering::Relaxed), 0, "still nothing constructed");

    // A token WITHOUT an allowlist is likewise nothing — an empty allowlist can never mean
    // "any chat".
    let vars: HashMap<String, String> = [
        ("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123:abc".to_string()),
        ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), String::new()),
    ]
    .into_iter()
    .collect();
    let b = Arc::clone(&builds);
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        move || vars,
        Some(path.clone()),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_none(), "an empty allowlist disables the channel");
    assert_eq!(builds.load(Ordering::Relaxed), 0);
}

// ---------------------------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------------------------

/// A message from a chat that is not on the allowlist is dropped: nothing is previewed, nothing is
/// lowered, and — the part that matters — **no reply of any kind is sent**. A reply would confirm
/// to a stranger holding a leaked bot token that a trading node is on the other end.
#[test]
fn unlisted_chat_is_ignored() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, STRANGER, "/submit hyperliquid BTC buy 100 64000"),
        update(2, STRANGER, "/status"),
        update(3, STRANGER, "/confirm anything"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.ignored_unlisted, vec![STRANGER, STRANGER, STRANGER]);
    assert_eq!(report.replies, 0, "an unlisted chat is NEVER answered");
    assert!(deps.replies().is_empty(), "not one byte goes back to the stranger");
    assert!(report.previewed.is_empty() && report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "nothing was ever lowered toward the core");
    assert!(pending.is_empty(), "no token was minted for a stranger");
    // The updates are still CONSUMED, so Telegram stops redelivering them (the offset advances).
    assert!(led.is_processed(3));
    assert_eq!(led.offset(), 4);
}

/// ⚠ **THE DEFAULT IS PER-CHAT, AND A CHAT IS NOT A PERSON.** With no user allowlist configured,
/// any sender in an allowlisted chat commands the node — the documented policy, pinned here so it
/// cannot change silently in either direction. What it must NOT be is unattributable: the audit
/// rationale names the sender, so an accepted order in a group can be traced to a person.
#[test]
fn chat_only_authorization_admits_any_member_but_records_who() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, OTHER_USER, "/marketexit hyperliquid")]);
    deps.push_batch(vec![update_from(2, CHAT, OTHER_USER, "/confirm tok0")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(previewed.previewed, vec!["tok0"], "a second group member may command the node");
    assert!(previewed.ignored_unlisted_user.is_empty(), "no user allowlist ⇒ nothing user-refused");

    let executed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(executed.executed.len(), 1, "and their /confirm executes");
    let (_, reason) = deps.accepted().pop().expect("one command reached accept");
    assert!(
        reason.contains(&format!("user {OTHER_USER}")),
        "the audit rationale must name the PERSON, not just the chat: {reason}"
    );
    assert!(reason.contains(&format!("chat {CHAT}")), "…and still the chat: {reason}");
}

/// The OPT-IN tightening. With `VIKE_TELEGRAM_ALLOWED_USER_IDS` configured, a chat-allowlisted but
/// user-unlisted sender is refused — and, exactly like an unlisted chat, is **never answered**.
#[test]
fn a_configured_user_allowlist_refuses_an_unlisted_member_silently() {
    let deps = StubDeps::new();
    deps.push_batch(vec![
        update_from(1, CHAT, OTHER_USER, "/submit hyperliquid BTC buy 100 64000"),
        update_from(2, CHAT, OTHER_USER, "/status"),
        update_from(3, CHAT, OTHER_USER, "/confirm anything"),
    ]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config_user_allowlisted(), &led, &mut pending);

    assert_eq!(report.ignored_unlisted_user, vec![OTHER_USER; 3]);
    assert!(
        report.ignored_unlisted.is_empty(),
        "the CHAT passed — it is the USER that was refused"
    );
    assert_eq!(
        report.replies, 0,
        "same discipline as an unlisted chat: no reply, not even an error"
    );
    assert!(deps.replies().is_empty());
    assert!(deps.accepted().is_empty(), "nothing was ever lowered toward the core");
    assert!(pending.is_empty(), "no token minted for an unlisted member");
    // …and the updates are still consumed, so Telegram stops redelivering them.
    assert_eq!(led.offset(), 4);
}

/// …while the allowlisted operator in that same chat is unaffected — the tightening narrows, it
/// does not break the deployment.
#[test]
fn a_configured_user_allowlist_still_admits_its_own_member() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, USER, "/marketexit hyperliquid")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config_user_allowlisted(), &led, &mut pending);
    assert_eq!(report.previewed, vec!["tok0"]);
    assert!(report.ignored_unlisted_user.is_empty());
}

/// PREVIEW and CONFIRM can be two different people, because the token binds to the CHAT. That is
/// deliberately unchanged — but the audit line must then name BOTH, or an order looks like it was
/// authorized by whoever happened to type the instruction.
#[test]
fn a_cross_user_confirm_records_both_actors() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update_from(1, CHAT, USER, "/marketexit hyperliquid")]);
    deps.push_batch(vec![update_from(2, CHAT, OTHER_USER, "/confirm tok0")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    poll_once(&deps, &config(), &led, &mut pending);
    let executed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(executed.executed.len(), 1, "another member of the chat CAN confirm — unchanged");

    let (_, reason) = deps.accepted().pop().expect("one command reached accept");
    assert!(reason.contains(&format!("user {USER}")), "who instructed: {reason}");
    assert!(reason.contains(&format!("/confirm by user {OTHER_USER}")), "who authorized: {reason}");
}
