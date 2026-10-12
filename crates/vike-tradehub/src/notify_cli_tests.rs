use super::*;
use std::cell::Cell;
use std::sync::{Arc, Mutex};

/// A transport that records every POST and answers from a script, one entry per attempt (the last
/// entry repeats). It never touches the network.
#[derive(Clone)]
struct ScriptedTransport {
    calls: Arc<Mutex<Vec<(String, String)>>>,
    answers: Arc<Vec<Result<(), String>>>,
}

impl ScriptedTransport {
    fn answering(answers: Vec<Result<(), String>>) -> Self {
        ScriptedTransport { calls: Arc::default(), answers: Arc::new(answers) }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl WebhookTransport for ScriptedTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
        let mut calls = self.calls.lock().unwrap();
        calls.push((url.to_string(), body.to_string()));
        let i = (calls.len() - 1).min(self.answers.len() - 1);
        self.answers[i].clone()
    }
}

const TOKEN: &str = "123456:SECRET-TOKEN";

fn sink(t: &ScriptedTransport) -> WebhookSink<ScriptedTransport> {
    let cfg = WebhookConfig {
        name: TELEGRAM_TARGET.to_string(),
        kind: WebhookKind::Telegram { token: TOKEN.to_string(), chat_id: "9001".to_string() },
    };
    WebhookSink::new(cfg, t.clone())
}

fn pager<'a>(dir: &'a Path, unit: &'a str, now: i64) -> Pager<'a> {
    let p = Pager { dir, unit, now, now_h: utc_human(now), host: "box1".to_string() };
    p.ensure_dirs().expect("state dirs");
    p
}

fn verdict(prop: &str) -> Option<String> {
    match prop {
        "Result" => Some("exit-code".to_string()),
        "ExecMainStatus" => Some("1".to_string()),
        "NRestarts" => Some("4".to_string()),
        _ => None,
    }
}

fn no_sleep(_: Duration) {}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_default()
}

/// The Telegram `text` of the one recorded POST.
fn text_of(call: &(String, String)) -> String {
    let v: serde_json::Value = serde_json::from_str(&call.1).expect("a JSON body");
    v["text"].as_str().expect("a text field").to_string()
}

const T0: i64 = 1_760_000_000; // 2025-10-09 08:53:20 UTC

#[test]
fn the_command_line_takes_one_verbatim_unit_and_an_optional_scope() {
    let a = |s: &[&str]| s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert_eq!(
        parse(&a(&["hl-perps-orders.service"])),
        Ok(Invocation::Page { unit: "hl-perps-orders.service".into(), scope: Scope::System }),
        "the unit is taken VERBATIM — no systemd unescaping, which maps `-` to `/`"
    );
    assert_eq!(
        parse(&a(&["--user", "poly-l2-monitor.service"])),
        Ok(Invocation::Page { unit: "poly-l2-monitor.service".into(), scope: Scope::User })
    );
    assert_eq!(
        parse(&a(&[])),
        Ok(Invocation::Page { unit: UNKNOWN_UNIT.into(), scope: Scope::System }),
        "no unit still pages (the bash script's `${{1:-unknown.unit}}`)"
    );
    assert_eq!(parse(&a(&["-h"])), Ok(Invocation::Help));
    assert!(parse(&a(&["a.service", "b.service"])).is_err());
    assert!(parse(&a(&["--cooldown", "60"])).is_err(), "no knob exists to tune");
}

#[test]
fn a_first_failure_sends_systemds_verdict_and_records_the_send() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Ok(())]);
    let p = pager(dir.path(), "hl-perps-orders.service", T0);
    let out = p.page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::Sent { suppressed_included: 0, hourly: 1 });
    assert_eq!(out.exit_code(), ExitCode::SUCCESS);

    let calls = t.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, format!("https://api.telegram.org/bot{TOKEN}/sendMessage"));
    assert_eq!(
        text_of(&calls[0]),
        "\u{1F534} unit failed on box1\n\
         unit:    hl-perps-orders.service\n\
         result:  exit-code (exit 1, restarts 4)\n\
         time:    2025-10-09 08:53:20 UTC"
    );
    assert_eq!(read(dir.path(), "sends"), format!("{T0}\n"));
    assert_eq!(
        read(dir.path(), "units/hl-perps-orders.service"),
        format!("{T0} 0 2025-10-09 08:53:20 UTC\n")
    );
    assert_eq!(read(dir.path(), "units/hl-perps-orders.service.cooldown"), "3600\n");
    assert_eq!(read(dir.path(), "last_ok"), "2025-10-09 08:53:20 UTC\n");
    assert!(!dir.path().join("last_error").exists(), "nothing failed");
}

#[test]
fn an_unknown_verdict_reads_unknown_and_question_marks() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Ok(())]);
    pager(dir.path(), "x.service", T0).page(&sink(&t), &|_| None, &no_sleep);
    assert!(text_of(&t.calls()[0]).contains("result:  unknown (exit ?, restarts ?)"));
}

#[test]
fn repeats_inside_the_cooldown_are_counted_then_reported_in_the_next_message() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Ok(())]);
    let unit = "flappy.service";
    pager(dir.path(), unit, T0).page(&sink(&t), &verdict, &no_sleep);
    for (i, dt) in [60, 900, 3599].into_iter().enumerate() {
        let out = pager(dir.path(), unit, T0 + dt).page(&sink(&t), &verdict, &no_sleep);
        assert_eq!(
            out,
            Outcome::CooldownSuppressed {
                cooldown: 3600,
                since_last: dt,
                total_suppressed: i as u64 + 1
            }
        );
        assert_eq!(out.exit_code(), ExitCode::SUCCESS, "suppression is not a failure");
    }
    assert_eq!(t.calls().len(), 1, "nothing was sent inside the cooldown");

    let out = pager(dir.path(), unit, T0 + 3600).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::Sent { suppressed_included: 3, hourly: 2 });
    assert!(
        text_of(&t.calls()[1])
            .ends_with("\nnote:    +3 further failure(s) suppressed since 2025-10-09 08:53:20 UTC"),
        "{}",
        text_of(&t.calls()[1])
    );
}

#[test]
fn a_flapping_unit_doubles_its_cooldown_to_six_hours_and_a_quiet_stretch_resets_it() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Ok(())]);
    let unit = "hl-perps-staking.service";
    let cooldown = || read(dir.path(), "units/hl-perps-staking.service.cooldown");
    let mut now = T0;
    pager(dir.path(), unit, now).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(cooldown(), "3600\n");
    // Each next send lands as soon as the cooldown allows — inside twice it — so it doubles.
    for expected in ["7200\n", "14400\n", "21600\n", "21600\n"] {
        let current: i64 = cooldown().trim().parse().unwrap();
        now += current;
        let out = pager(dir.path(), unit, now).page(&sink(&t), &verdict, &no_sleep);
        assert!(matches!(out, Outcome::Sent { .. }), "{out:?}");
        assert_eq!(cooldown(), expected, "capped at MAX_COOLDOWN_SECS");
    }
    // A quiet stretch longer than twice the cooldown: back to the base.
    now += 2 * MAX_COOLDOWN_SECS + 1;
    pager(dir.path(), unit, now).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(cooldown(), "3600\n");
}

#[test]
fn a_tampered_cooldown_file_is_clamped_into_its_bounds() {
    assert_eq!(parse_cooldown(None), BASE_COOLDOWN_SECS);
    assert_eq!(parse_cooldown(Some("garbage")), BASE_COOLDOWN_SECS);
    assert_eq!(parse_cooldown(Some("5\n")), BASE_COOLDOWN_SECS);
    assert_eq!(parse_cooldown(Some("999999\n")), MAX_COOLDOWN_SECS);
    assert_eq!(parse_cooldown(Some("7200\n")), 7200);
    assert_eq!(next_cooldown(0, T0, 3600), BASE_COOLDOWN_SECS, "no previous send");
}

#[test]
fn the_global_cap_suppresses_the_thirteenth_send_in_an_hour_across_units() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Ok(())]);
    for i in 0..MAX_PER_HOUR {
        let unit = format!("u{i}.service");
        let out = pager(dir.path(), &unit, T0 + i as i64).page(&sink(&t), &verdict, &no_sleep);
        assert_eq!(out, Outcome::Sent { suppressed_included: 0, hourly: i + 1 });
    }
    let out = pager(dir.path(), "u12.service", T0 + 100).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::CapSuppressed { sent_last_hour: MAX_PER_HOUR });
    assert_eq!(out.exit_code(), ExitCode::SUCCESS);
    assert_eq!(t.calls().len(), MAX_PER_HOUR);
    assert!(read(dir.path(), "last_error").ends_with("\tu12.service\tglobal-cap-suppressed\n"));
    // The capped unit gets its next chance five minutes out, carrying its suppressed count…
    let at = T0 + 100 - BASE_COOLDOWN_SECS + CAP_RETRY_SECS;
    assert_eq!(read(dir.path(), "units/u12.service"), format!("{at} 1 \n"));
    // …and once the hour has rolled the ledger is pruned and the unit gets through.
    let out = pager(dir.path(), "u12.service", T0 + 3700).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::Sent { suppressed_included: 1, hourly: 1 });
    assert_eq!(read(dir.path(), "sends"), format!("{}\n", T0 + 3700));
}

#[test]
fn a_send_that_does_not_land_exits_non_zero_and_leaves_a_breadcrumb_without_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Err("transport error".to_string())]);
    let sleeps = Cell::new(0);
    let out = pager(dir.path(), "x.service", T0).page(&sink(&t), &verdict, &|d| {
        assert_eq!(d, RETRY_SPACING);
        sleeps.set(sleeps.get() + 1);
    });
    assert_eq!(out, Outcome::Failed { category: "transport error".into(), attempts: ATTEMPTS });
    assert_eq!(out.exit_code(), ExitCode::FAILURE, "the notifier must land in --failed");
    assert_eq!(t.calls().len(), 3, "three attempts");
    assert_eq!(sleeps.get(), 2, "two pauses between three attempts");
    let crumb = read(dir.path(), "last_error");
    assert!(crumb.contains("\tx.service\tdelivery-failed err=transport error attempts=3\n"));
    let line = out.log_line(&pager(dir.path(), "x.service", T0));
    for leak in [crumb.as_str(), line.as_str()] {
        assert!(!leak.contains("SECRET-TOKEN"), "a credential reached a log: {leak}");
    }
    // Nothing that records a SEND was touched.
    assert!(!dir.path().join("sends").exists());
    assert!(!dir.path().join("last_ok").exists());
    assert!(!dir.path().join("units/x.service").exists());
}

#[test]
fn a_definite_refusal_is_not_retried_and_a_transient_one_is() {
    let dir = tempfile::tempdir().unwrap();
    let t = ScriptedTransport::answering(vec![Err("http 401".to_string())]);
    let out = pager(dir.path(), "a.service", T0).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::Failed { category: "http 401".into(), attempts: 1 });

    let t = ScriptedTransport::answering(vec![Err("http 502".to_string()), Ok(())]);
    let out = pager(dir.path(), "b.service", T0).page(&sink(&t), &verdict, &no_sleep);
    assert_eq!(out, Outcome::Sent { suppressed_included: 0, hourly: 1 });
    assert_eq!(t.calls().len(), 2);

    for (category, transient) in [
        ("transport error", true),
        ("http 500", true),
        ("http 429", true),
        ("http 408", true),
        ("http 400", false),
        ("http 403", false),
    ] {
        assert_eq!(is_transient(category), transient, "{category}");
    }
}

#[test]
fn no_attempt_starts_that_could_overrun_the_unit_timeout() {
    // The budget arithmetic the unit's `TimeoutStartSec=30` rests on: every attempt timing out
    // fits two attempts and one pause, never a third.
    let worst_two = 2 * ATTEMPT_BOUND + RETRY_SPACING;
    let worst_three = 3 * ATTEMPT_BOUND + 2 * RETRY_SPACING;
    assert!(worst_two <= SEND_BUDGET && SEND_BUDGET < Duration::from_secs(30));
    assert!(worst_three > SEND_BUDGET, "a third timed-out attempt must not be started");
}

#[test]
fn breadcrumbs_keep_only_the_last_two_hundred_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = pager(dir.path(), "x.service", T0);
    for i in 0..(BREADCRUMB_KEEP + 5) {
        p.breadcrumb(&format!("crumb-{i}"));
    }
    let text = read(dir.path(), "last_error");
    assert_eq!(text.lines().count(), BREADCRUMB_KEEP);
    assert!(text.lines().next().unwrap().ends_with("\tcrumb-5"));
}

#[test]
fn the_state_file_name_is_the_bash_scripts_tr_byte_for_byte() {
    assert_eq!(safe_name("hl-perps-orders.service"), "hl-perps-orders.service");
    assert_eq!(safe_name("vike-notify@x.service"), "vike-notify@x.service");
    assert_eq!(safe_name("a/b c\\d"), "a_b_c_d");
    assert_eq!(safe_name("é"), "__", "one `_` per byte, as `tr` does");
}

#[test]
fn a_unit_record_tolerates_garbage_as_the_bash_read_did() {
    assert_eq!(UnitRecord::parse(""), UnitRecord::default());
    assert_eq!(UnitRecord::parse("x y"), UnitRecord::default());
    let r = UnitRecord::parse("100 2 2025-10-09 08:53:20 UTC\n");
    assert_eq!(
        r,
        UnitRecord { last_ts: 100, suppressed: 2, since_h: "2025-10-09 08:53:20 UTC".into() }
    );
    assert_eq!(UnitRecord::parse(&r.render()), r, "render and parse round-trip");
    assert_eq!(utc_human(0), "1970-01-01 00:00:00 UTC");
}

#[test]
fn the_telegram_key_names_are_derived_from_the_alerting_reader() {
    let keys = telegram_keys();
    assert_eq!(keys.len(), 2, "the token and the chat id: {keys:?}");
    assert!(keys.iter().all(|k| WEBHOOK_KEYS.contains(k)));
    // Both present builds the target; either alone does not.
    let map = |names: &[&str]| -> HashMap<String, String> {
        names.iter().map(|k| ((*k).to_string(), "v".to_string())).collect()
    };
    assert!(telegram_from_map(&map(&keys)).is_some());
    assert!(telegram_from_map(&map(&keys[..1])).is_none());
    assert!(telegram_from_map(&map(&keys[1..])).is_none());
    // The refusal names both KEYS and no value.
    let msg = CredentialProblem::Missing.to_string();
    for k in &keys {
        assert!(msg.contains(k), "{msg}");
    }
}

#[test]
fn a_store_without_the_keys_is_a_missing_credential_not_an_empty_success() {
    // No database at all: the store is Absent, every declared name absent.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_telegram_target(dir.path()).unwrap_err(), CredentialProblem::Missing);
    // An EMPTY store, created the one way a store comes into being.
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path()))
        .expect("a fresh settings store");
    assert_eq!(read_telegram_target(dir.path()).unwrap_err(), CredentialProblem::Missing);
}

#[test]
fn a_store_that_will_not_open_is_reported_as_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a sqlite database").unwrap();
    let err = read_telegram_target(dir.path()).unwrap_err();
    assert!(matches!(err, CredentialProblem::Unreadable(_)), "{err:?}");
    assert_eq!(err.breadcrumb(), "credential-store-unreadable");
}

#[test]
fn the_systemd_verdict_is_asked_of_the_right_manager() {
    assert_eq!(
        systemctl_args(Scope::System, "Result", "x.service"),
        ["show", "-p", "Result", "--value", "x.service"]
    );
    assert_eq!(
        systemctl_args(Scope::User, "NRestarts", "x.service"),
        ["--user", "show", "-p", "NRestarts", "--value", "x.service"],
        "a user-scope unit's verdict lives in the USER manager"
    );
}
