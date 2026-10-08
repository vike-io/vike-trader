//! The confirm contract and the at-most-once update ledger.

use super::*;

// ---------------------------------------------------------------------------------------------
// The confirm contract
// ---------------------------------------------------------------------------------------------

/// THE mandatory-preview contract: the message carrying a write instruction can never execute it.
/// It answers with a preview + a token and stops there.
#[test]
fn write_without_confirm_executes_nothing() {
    let deps = StubDeps::new();
    deps.push_batch(vec![update(1, CHAT, "/submit hyperliquid BTC buy 0.5 64000")]);
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.previewed.len(), 1, "exactly one token issued");
    assert!(report.executed.is_empty(), "NOTHING was sent");
    assert!(deps.accepted().is_empty(), "`accept` — the only path to the core — was never called");
    assert_eq!(pending.len(), 1, "the command is held, not sent");

    let (chat, text) = deps.replies().first().cloned().expect("the operator gets a preview back");
    assert_eq!(chat, CHAT);
    assert!(text.contains("PREVIEW"), "{text}");
    assert!(text.contains("nothing has been sent"), "{text}");
    assert!(text.contains("/confirm"), "the reply must say how to execute: {text}");
    // The preview shows the RESOLVED command, including the coid minted for it.
    for needle in ["hyperliquid", "BTC", "buy", "0.5", "64000", "tg-coid-0"] {
        assert!(text.contains(needle), "{needle:?} missing from the preview: {text}");
    }
}

/// A token fires at most once. The second `/confirm` of the same token finds nothing — the entry is
/// REMOVED before the command is handed on, so even a duplicated message cannot double-place.
#[test]
fn confirm_token_is_single_use() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    // Both confirms arrive in ONE batch, so this is not a timing artifact.
    deps.push_batch(vec![
        update(2, CHAT, &format!("/confirm {token}")),
        update(3, CHAT, &format!("/confirm {token}")),
    ]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.executed.len(), 1, "exactly one execution");
    assert_eq!(deps.accepted().len(), 1, "the core saw the command exactly once");
    assert!(pending.is_empty(), "the token is gone after firing");
    let last = deps.replies().last().cloned().expect("a reply to the second confirm").1;
    assert!(last.contains("no such pending confirmation"), "{last}");
}

/// Past the 60 s window the token is refused and the command is dropped — an operator who walked
/// away cannot have a stale intent execute later.
#[test]
fn confirm_token_expires() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.set_now(1_000_000);
    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    // One millisecond past the window.
    deps.set_now(1_000_000 + CONFIRM_WINDOW_MS + 1);
    deps.push_batch(vec![update(2, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.expired, vec![token], "reported as EXPIRED, not as unknown");
    assert!(report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "an expired confirmation never reaches the core");
    assert!(pending.is_empty(), "the expired token is burned, not left for a later retry");
    let last = deps.replies().last().cloned().expect("a reply").1;
    assert!(last.contains("EXPIRED"), "{last}");

    // Exactly AT the window is still valid (the boundary is not off by one).
    deps.set_now(2_000_000);
    deps.push_batch(vec![update(3, CHAT, "/cancel ORDER-B")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");
    deps.set_now(2_000_000 + CONFIRM_WINDOW_MS);
    deps.push_batch(vec![update(4, CHAT, &format!("/confirm {token}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.executed, vec!["ORDER-B".to_string()]);
}

/// A token executes the command it PREVIEWED — never a later one. Two previews are outstanding; the
/// first token executes the first command, verbatim, and the second stays untouched.
#[test]
fn confirm_token_bound_to_command() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(1, CHAT, "/cancel ORDER-A"), update(2, CHAT, "/cancel ORDER-B")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(previewed.previewed.len(), 2, "two independent previews");
    let (token_a, token_b) = (previewed.previewed[0].clone(), previewed.previewed[1].clone());
    assert_ne!(token_a, token_b);

    deps.push_batch(vec![update(3, CHAT, &format!("/confirm {token_a}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(report.executed, vec!["ORDER-A".to_string()]);
    let accepted = deps.accepted();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0].0,
        WireCommand::Cancel("ORDER-A".into()),
        "token A executed exactly the command A previewed — never B's"
    );
    assert_eq!(pending.len(), 1, "B's token is untouched");

    // And a token cannot be spent from a DIFFERENT chat, even the right token.
    deps.push_batch(vec![update(4, STRANGER, &format!("/confirm {token_b}"))]);
    let report = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(report.ignored_unlisted, vec![STRANGER]);
    assert_eq!(deps.accepted().len(), 1, "still exactly one command ever reached the core");
    assert_eq!(pending.len(), 1, "B's token survived the stranger's attempt");
}

// ---------------------------------------------------------------------------------------------
// At-most-once
// ---------------------------------------------------------------------------------------------

/// Telegram redelivers anything below the acked offset, and a restarted daemon re-reads the ledger.
/// Neither may re-execute a command: a replayed `update_id` is skipped outright, and the next poll
/// asks for `last + 1`.
#[test]
fn duplicate_update_id_is_not_reprocessed() {
    let deps = StubDeps::new();
    let (_dir, led) = ledger();
    let mut pending = PendingConfirms::default();

    deps.push_batch(vec![update(5, CHAT, "/cancel ORDER-A")]);
    let previewed = poll_once(&deps, &config(), &led, &mut pending);
    let token = previewed.previewed.first().cloned().expect("a token was issued");

    deps.push_batch(vec![update(6, CHAT, &format!("/confirm {token}"))]);
    let first = poll_once(&deps, &config(), &led, &mut pending);
    assert_eq!(first.executed, vec!["ORDER-A".to_string()]);
    assert_eq!(deps.accepted().len(), 1);

    // Telegram redelivers BOTH updates (an ack that never landed / a restart).
    deps.push_batch(vec![
        update(5, CHAT, "/cancel ORDER-A"),
        update(6, CHAT, &format!("/confirm {token}")),
    ]);
    let replayed = poll_once(&deps, &config(), &led, &mut pending);

    assert_eq!(replayed.skipped_duplicate, vec![5, 6]);
    assert!(replayed.previewed.is_empty() && replayed.executed.is_empty());
    assert_eq!(deps.accepted().len(), 1, "the order was NOT placed a second time");
    assert_eq!(replayed.replies, 0, "a replayed update is not even answered");

    // The ACK: each poll asks for last + 1, so Telegram drops what we already consumed.
    assert_eq!(deps.offsets(), vec![1, 6, 7]);
    assert_eq!(led.last(), 6);
}

/// ⚠ **THE DEFECT, gated.** This file's whole job is at-most-once ACROSS A RESTART, and its
/// persistence used to be best-effort: a write that could not land was swallowed, so the in-memory
/// mark held for the running process and the NEXT one started from `0` — i.e. it replayed
/// everything Telegram still held.
///
/// Not hypothetical. The ledger used to live beside the EXECUTABLE (`<project>/bin/` for
/// `deploy/vike-tradehub.service`'s `ExecStart`), and that unit runs `ProtectSystem=strict`, which
/// makes the whole filesystem read-only except what `ReadWritePaths=` names. Every append failed,
/// every failure was swallowed, and the at-most-once record of a REMOTE ORDER-ORIGINATION path was
/// silently absent.
///
/// This test is the RED-before gate: against the pre-fix code it asserted the mark survived a
/// restart and FAILED (`left: 0, right: 7`). The contract it holds now is the fix's — the write
/// cannot silently no-op because it cannot silently do anything.
#[test]
fn a_ledger_write_that_cannot_land_is_an_error_not_a_silent_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = unwritable_ledger_paths(dir.path());

    let err = UpdateLedger::open(&paths).expect_err("an unwritable ledger must refuse to open");
    assert!(
        err.contains(&paths.path.display().to_string()),
        "the error names the path an operator has to fix: {err}"
    );

    // And the runtime half: a directory that stops being writable UNDER a live daemon. The mark
    // still advances in memory (it is the Telegram ACK — see `UpdateLedger::mark`), but the caller
    // is TOLD, which is what lets `poll_once` refuse to dispatch.
    let live = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(live.path())).expect("writable at open");
    ledger.mark(7).expect("the first append lands");
    let path = live.path().join("telegram_updates.ledger");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir_all(&path).unwrap(); // a DIRECTORY where the file was
    assert!(ledger.mark(8).is_err(), "an append that cannot land must report it, never shrug");
}

/// The ARMING gate: a channel whose at-most-once record cannot be kept never starts. Nothing is
/// constructed — no `ureq` agent, no bot token in a struct, no poller thread — because a remote
/// order path without its replay guard is worse than no remote order path.
///
/// This is the production failure shape made loud: `ProtectSystem=strict` + a ledger path the unit
/// grants no write access to. RED before the fix (`maybe_spawn` never opened the ledger at all, so
/// it returned `Some` and built the deps).
#[test]
fn an_unwritable_ledger_refuses_to_arm_the_channel() {
    let dir = tempfile::tempdir().expect("tempdir");
    let builds = Arc::new(AtomicUsize::new(0));
    let vars: HashMap<String, String> = [
        ("VIKE_TELEGRAM_BOT_TOKEN".to_string(), "123:abc".to_string()),
        ("VIKE_TELEGRAM_ALLOWED_CHAT_IDS".to_string(), CHAT.to_string()),
    ]
    .into_iter()
    .collect();

    for paths in [Some(unwritable_ledger_paths(dir.path())), None] {
        let b = Arc::clone(&builds);
        let v = vars.clone();
        let handle = maybe_spawn(
            Some("1"),
            Some("1"),
            move || v,
            paths,
            |_cfg| {
                b.fetch_add(1, Ordering::Relaxed);
                Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
            },
        );
        assert!(handle.is_none(), "no usable ledger ⇒ no channel");
    }
    assert_eq!(
        builds.load(Ordering::Relaxed),
        0,
        "the deps (and with them the bot-token-bearing agent) are never built"
    );

    // …and the control: the SAME configuration with a writable ledger DOES arm, so the assertions
    // above are about the ledger and not about some other gate being shut.
    let ok = tempfile::tempdir().expect("tempdir");
    let b = Arc::clone(&builds);
    let handle = maybe_spawn(
        Some("1"),
        Some("1"),
        move || vars,
        Some(ledger_paths_in(ok.path())),
        |_cfg| {
            b.fetch_add(1, Ordering::Relaxed);
            Box::new(StubDeps::new()) as Box<dyn TelegramDeps + Send>
        },
    );
    assert!(handle.is_some(), "a writable ledger arms the channel");
    assert_eq!(builds.load(Ordering::Relaxed), 1);
    drop(handle); // the StopHandle signals AND joins the poller thread
}

/// The RUNNING half: an update whose consumption cannot be recorded is DROPPED, not dispatched.
/// Nothing is previewed, nothing is lowered toward the core, and not one reply goes out — including
/// for a read verb, because the ledger's guarantee is about UPDATES, not about which verb one
/// happens to carry.
///
/// RED before the fix: `mark` swallowed the error, so `/status` was answered and `/cancel` minted a
/// confirmation token, both with no durable record that the update had been consumed.
#[test]
fn an_update_whose_mark_cannot_be_persisted_is_never_dispatched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = UpdateLedger::open(&ledger_paths_in(dir.path())).expect("writable at open");
    // Now make the append impossible, exactly as a remount or a revoked grant would.
    let path = dir.path().join("telegram_updates.ledger");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir_all(&path).unwrap();

    let deps = StubDeps::new();
    deps.push_batch(vec![
        update(1, CHAT, "/status"),
        update(2, CHAT, "/cancel ORDER-A"),
        update(3, STRANGER, "/status"),
    ]);
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &ledger, &mut pending);

    assert_eq!(report.unrecorded, vec![1, 2, 3], "every update is reported as dropped");
    assert_eq!(report.replies, 0, "not one reply — a read verb is dropped too");
    assert!(deps.replies().is_empty());
    assert!(report.previewed.is_empty(), "no confirmation token is minted");
    assert!(report.executed.is_empty());
    assert!(deps.accepted().is_empty(), "nothing reached the core");
    assert!(pending.is_empty());
    // The allowlist check never even ran — the drop happens at step 2, before step 3.
    assert!(report.ignored_unlisted.is_empty());
}

/// The MOVE itself: an install whose mark still sits at the legacy `<exe_dir>` path keeps it, so
/// relocating the ledger cannot itself cause the replay it exists to prevent.
#[test]
fn the_legacy_exe_dir_mark_migrates_instead_of_replaying() {
    let dir = tempfile::tempdir().expect("tempdir");
    let legacy = dir.path().join("bin").join("telegram_updates.log");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "5\n").unwrap();
    let state = dir.path().join("settings").join("state");

    let ledger = UpdateLedger::open(&LedgerPaths {
        path: state.join("telegram_updates.ledger"),
        legacy: Some(legacy),
    })
    .expect("the state directory is created and writable");
    assert_eq!(ledger.last(), 5, "the pre-move mark carried over");

    // …and the redelivered backlog Telegram still holds is skipped, not re-previewed.
    let deps = StubDeps::new();
    deps.push_batch(vec![update(5, CHAT, "/cancel ORDER-A")]);
    let mut pending = PendingConfirms::default();
    let report = poll_once(&deps, &config(), &ledger, &mut pending);
    assert_eq!(report.skipped_duplicate, vec![5]);
    assert!(report.previewed.is_empty() && deps.accepted().is_empty());
    assert_eq!(deps.offsets(), vec![6], "the ACK resumes where the legacy file left off");
}
