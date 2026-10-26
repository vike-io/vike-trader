//! LIVE read-only PROBE: how long does the IB Gateway keep counting a CANCELLED account-summary request, and does IB
//! push the ledger at all once the dump is over?
//!
//! The question behind the `[322] Maximum number of account summary requests exceeded` bursts at the Gateway's nightly
//! `[1102]` and behind the kept subscription of `crates/bridges/vike-ibkr/src/recon/summary.rs`. IB allows two ACTIVE
//! summary subscriptions. The first run (2026-10-20) measured only this: on a fresh client id two requests were
//! accepted, each cancelled by dropping it, and the THIRD, 0.5 s after the second's cancel, was refused `[322]`, and so
//! was everything after it for as long as that run lasted. So a cancelled request IS still counted half a second later.
//! It did NOT measure for how long, or whose counter it is, and three models fit it:
//!
//! * **M1** a short window (seconds to a couple of minutes, per login): the kept subscription and its reopens are safe.
//! * **M2s** per TCP session (until the connection ends): a connection survives ONE reopen; the second is refused for
//!   good, so a reopen on silence must go (or the whole connection must be rebuilt instead).
//! * **M2c** per client id until the Gateway restarts (a new connection under the same id inherits the count): the
//!   recon connection would need a fresh client id per connection, or another request altogether.
//!
//! Run it (about 35 minutes), where the IB Gateway's port is reachable, BEFORE the kept-subscription daemon is
//! deployed (that daemon would hold one of the two slots for good and shift every number) or with the account's
//! adapter stopped:
//!
//!     cargo test -p vike-ibkr --features ibkr --test ibkr_summary_cancel_probe -- --ignored --nocapture
//!
//! with `VIKE_SETTINGS_DIR` (the settings database) and `VIKE_NO_LIVE_KEYS=true` exported, as the other live tests
//! need. The per-pass daemon of v0.1.82 may keep running: its request about once a minute is in the Gateway's count if
//! the counter is per LOGIN, so a refusal at a short offset can be its doing; an accept in spite of it says the
//! counter is not per login. Every connection here uses a client id taken from the clock (never one the daemon dials,
//! never one an earlier run spent: a spent id is the thing under test), printed at the start.
//!
//! * **C, cadence** (one connection, one request): the dump, then a PASSIVE 10-minute hold that prints the arrival time
//!   and size of every push, and whether the rows pushed repeat a value or change one. No push in 10 minutes means a
//!   quiet account is pushed nothing, so a kept subscription that reopened on silence would reopen every 20 minutes.
//! * **D, retention ladder** (a fresh id): A1 and A2 back to back, each read to its end and cancelled; then A3 is
//!   attempted at +5 s, +30 s, +2 min, +5 min, +10 min and +21 min after A2's cancel, stopping at the first accept.
//!   The first accepted offset is the upper bound of the window (M1); never accepted in 21 minutes is M2s or M2c.
//! * **E, scope** (only when D never accepted): disconnect and reconnect under the SAME id and ask once (accepted =
//!   per session, M2s; refused = per client id or per login), then connect under a DIFFERENT fresh id and ask once
//!   (accepted = per client id, M2c; refused = per login, a Gateway-wide count).
//! * **B, held together** (a fresh id, last): three requests held open at once. Meaningful only when D accepted (M1),
//!   because otherwise everything is refused for a reason D already gave: it shows the cap of two ACTIVE requests.
//!
//! READ-ONLY (it makes and cancels summary requests, nothing else) and triple-gated like the other live tests:
//! `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on the `socket` backend, exists and the
//! gateway answers; and a hard assert refuses anything but a paper `DU…` account. Its OUTPUT, the RESULT block at the
//! end, is the result; the only failure is a run in which not one request was answered (nothing was measured).
#![cfg(feature = "ibkr")]

mod common;
use common::ibkr_settings;

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummaryResult, AccountSummaryTags};
use ibapi::client::blocking::{Client, Subscription};
use ibapi::subscriptions::SubscriptionItem;
use vike_ibkr::config::load_ibkr_config_from;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_venues::IbkrBackend;

/// How long one request may take to reach its end marker.
const WAIT: Duration = Duration::from_secs(15);
/// How long part C holds one subscription open, passively.
const PASSIVE_HOLD: Duration = Duration::from_secs(10 * 60);
/// Rows of a push arrive within milliseconds of each other: this much silence ends a push.
const PUSH_GAP: Duration = Duration::from_millis(1500);
/// When part D attempts A3, counted from A2's cancel.
const LADDER: [Duration; 6] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(21 * 60),
];

/// One line of the log, with the time since the start of the run.
fn say(t0: Instant, line: &str) {
    eprintln!("[T+{:7.1}s] {line}", t0.elapsed().as_secs_f64());
}

/// Four client ids this gateway has not seen from us: counted in fours from the clock, so a rerun an hour later gets
/// others (the daemon's ids are small and configured; these are `1000 + 4k`).
fn fresh_ids() -> [i32; 4] {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let base = 1_000 + (secs % 8_000) as i32 * 4;
    [base, base + 1, base + 2, base + 3]
}

fn sleep_until(due: Instant) {
    let now = Instant::now();
    if due > now {
        std::thread::sleep(due - now);
    }
}

/// Connect, retrying a few times: an id the Gateway has only just been told to release answers `[326]` for a moment.
fn connect(addr: &str, id: i32) -> Result<Client, String> {
    let mut last = String::new();
    for _ in 0..5 {
        match Client::connect(addr, id) {
            Ok(client) => return Ok(client),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Err(last)
}

/// How one request ended.
enum Answer {
    /// The dump reached its end marker: this many rows.
    Rows(usize),
    /// The Gateway refused it: `[322] Maximum number of account summary requests exceeded`.
    Refused322(String),
    /// Anything else (a timeout, another error, a request that could not be written).
    Other(String),
}

impl std::fmt::Display for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Answer::Rows(n) => write!(f, "answered: {n} rows to the end marker"),
            Answer::Refused322(text) => write!(f, "REFUSED {text}"),
            Answer::Other(text) => write!(f, "failed: {text}"),
        }
    }
}

impl Answer {
    fn is_rows(&self) -> bool {
        matches!(self, Answer::Rows(_))
    }
}

/// One `$LEDGER:ALL` request, read to its end marker. The subscription comes back so the caller decides when it is
/// cancelled (drop it) or held (keep it).
fn ask(
    client: &Client,
    group: &AccountGroup,
) -> (Answer, Option<Subscription<AccountSummaryResult>>) {
    let sub = match client.account_summary(group, &[AccountSummaryTags::LEDGER_ALL]) {
        Ok(sub) => sub,
        Err(e) => return (Answer::Other(format!("not sent: {e}")), None),
    };
    let end = Instant::now() + WAIT;
    let mut rows = 0;
    let answer = loop {
        match sub.next_timeout(end.saturating_duration_since(Instant::now())) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(_)))) => rows += 1,
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => {
                break Answer::Rows(rows);
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                eprintln!("      notice on the request: [{}] {}", n.code, n.message);
            }
            Some(Err(ibapi::Error::Notice(n))) if n.code == 322 => {
                break Answer::Refused322(format!("[{}] {}", n.code, n.message));
            }
            Some(Err(e)) => break Answer::Other(e.to_string()),
            None => break Answer::Other(format!("no end marker within {WAIT:?}")),
        }
    };
    (answer, Some(sub))
}

// ---- C: cadence ------------------------------------------------------------------------------

/// One push: when it began (into the hold), its rows, and how many of them changed a value.
struct Push {
    at: Duration,
    rows: usize,
    changed: usize,
}

struct Cadence {
    dump_rows: usize,
    pushes: Vec<Push>,
}

fn flush(open: &mut Option<(Push, Instant)>, pushes: &mut Vec<Push>, t0: Instant) {
    if let Some((push, _)) = open.take() {
        say(
            t0,
            &format!(
                "C: push at +{:.0}s of the hold: {} rows, {} changed a value, {} repeated one",
                push.at.as_secs_f64(),
                push.rows,
                push.changed,
                push.rows - push.changed
            ),
        );
        pushes.push(push);
    }
}

/// Hold ONE subscription for [`PASSIVE_HOLD`], reading it, and log every push.
fn passive_hold(client: &Client, group: &AccountGroup, t0: Instant) -> Result<Cadence, String> {
    let sub = client
        .account_summary(group, &[AccountSummaryTags::LEDGER_ALL])
        .map_err(|e| format!("not sent: {e}"))?;
    let started = Instant::now();
    let end = started + PASSIVE_HOLD;
    let mut seen: BTreeMap<(String, String, String), String> = BTreeMap::new();
    let (mut dump_rows, mut in_dump) = (0, true);
    let mut pushes: Vec<Push> = Vec::new();
    let mut open: Option<(Push, Instant)> = None;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match sub.next_timeout(left.min(Duration::from_secs(5))) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => {
                let key = (row.account.clone(), row.tag.clone(), row.currency.clone());
                let changed = seen.get(&key) != Some(&row.value);
                seen.insert(key, row.value);
                if in_dump {
                    dump_rows += 1;
                    continue;
                }
                let joins = open.as_ref().is_some_and(|(_, last)| last.elapsed() < PUSH_GAP);
                if joins {
                    if let Some((push, last)) = open.as_mut() {
                        push.rows += 1;
                        push.changed += usize::from(changed);
                        *last = Instant::now();
                    }
                } else {
                    flush(&mut open, &mut pushes, t0);
                    let push =
                        Push { at: started.elapsed(), rows: 1, changed: usize::from(changed) };
                    open = Some((push, Instant::now()));
                }
            }
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => {
                in_dump = false;
                say(
                    t0,
                    &format!(
                        "C: the dump ended ({dump_rows} rows); holding {PASSIVE_HOLD:?} passively"
                    ),
                );
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                say(t0, &format!("C: notice [{}] {}", n.code, n.message));
            }
            Some(Err(e)) => {
                flush(&mut open, &mut pushes, t0);
                return Err(format!(
                    "the stream failed {:?} into the hold: {e}",
                    started.elapsed()
                ));
            }
            None => {}
        }
        if open.as_ref().is_some_and(|(_, last)| last.elapsed() >= PUSH_GAP) {
            flush(&mut open, &mut pushes, t0);
        }
        if in_dump && started.elapsed() > WAIT {
            return Err(format!("no end marker within {WAIT:?}"));
        }
    }
    flush(&mut open, &mut pushes, t0);
    Ok(Cadence { dump_rows, pushes })
}

// ---- D: the retention ladder -----------------------------------------------------------------

enum Ladder {
    /// A1 or A2 was not answered: the allowance was spent before the ladder began (inconclusive).
    SpentBefore(&'static str, String),
    /// A3 was accepted at this attempt, this long after A2's cancel, after this many refusals.
    Accepted { attempt: usize, after: Duration, refused: usize },
    /// A3 was refused at every offset.
    Never { refused: usize },
}

fn ladder(client: &Client, group: &AccountGroup, t0: Instant) -> Ladder {
    let (a1, sub) = ask(client, group);
    say(t0, &format!("D: A1 (read to its end, then cancelled): {a1}"));
    drop(sub);
    if !a1.is_rows() {
        return Ladder::SpentBefore("A1", a1.to_string());
    }
    std::thread::sleep(Duration::from_millis(500));
    let (a2, sub) = ask(client, group);
    say(t0, &format!("D: A2 (read to its end, then cancelled): {a2}"));
    drop(sub);
    let cancelled_at = Instant::now();
    if !a2.is_rows() {
        return Ladder::SpentBefore("A2", a2.to_string());
    }
    let mut refused = 0;
    for (n, offset) in LADDER.iter().enumerate() {
        sleep_until(cancelled_at + *offset);
        let (a3, sub) = ask(client, group);
        let after = cancelled_at.elapsed();
        say(
            t0,
            &format!(
                "D: A3 attempt {} at +{:.0}s after A2's cancel: {a3}",
                n + 1,
                after.as_secs_f64()
            ),
        );
        drop(sub);
        if a3.is_rows() {
            return Ladder::Accepted { attempt: n + 1, after, refused };
        }
        refused += 1;
    }
    Ladder::Never { refused }
}

#[test]
#[ignore = "network + a running IB Gateway logged into a paper DU… account, ~35 minutes — run manually (see module doc)"]
fn how_long_a_cancelled_summary_request_counts_and_whether_ib_pushes() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(cfg) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    if cfg.backend == IbkrBackend::Cpapi {
        eprintln!("skip: venue.ibkr.demo.backend is cpapi; this probe reads the IB Gateway");
        return;
    }
    // HARD SAFETY: paper only — this assert stays a hard failure, never a skip.
    assert!(
        cfg.account.starts_with("DU"),
        "the probe must target a paper (DU\u{2026}) account, got {}",
        cfg.account
    );
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let [id_c, id_d, id_e, id_b] = fresh_ids();
    let t0 = Instant::now();
    say(
        t0,
        &format!(
            "gateway {addr}, account {}; client ids C={id_c} D={id_d} E={id_e} B={id_b}",
            cfg.account
        ),
    );
    let group = AccountGroup("All".to_string());
    let mut answered = 0_usize;

    // C: cadence, one request held passively.
    let Ok(client) = connect(&addr, id_c) else {
        eprintln!("skip: could not connect to {addr} as client id {id_c}");
        return;
    };
    let passive = passive_hold(&client, &group, t0);
    client.disconnect();
    drop(client);
    match &passive {
        Ok(c) => {
            answered += 1;
            say(
                t0,
                &format!("C: done, {} pushes after a dump of {} rows", c.pushes.len(), c.dump_rows),
            );
        }
        Err(e) => say(t0, &format!("C: not measured: {e}")),
    }
    std::thread::sleep(Duration::from_secs(3));

    // D: the retention ladder, on a fresh id.
    let verdict = match connect(&addr, id_d) {
        Ok(client) => {
            let verdict = ladder(&client, &group, t0);
            if !matches!(verdict, Ladder::SpentBefore("A1", _)) {
                answered += 1;
            }
            // E: scope, only when A3 was never accepted.
            let scope = if matches!(verdict, Ladder::Never { .. }) {
                client.disconnect();
                drop(client);
                std::thread::sleep(Duration::from_secs(3));
                let same_id = connect(&addr, id_d).map(|again| {
                    let (a, sub) = ask(&again, &group);
                    say(t0, &format!("E: the SAME id {id_d}, a new connection, one request: {a}"));
                    drop(sub);
                    again.disconnect();
                    a.is_rows()
                });
                let other_id = connect(&addr, id_e).map(|other| {
                    let (a, sub) = ask(&other, &group);
                    say(t0, &format!("E: a DIFFERENT fresh id {id_e}, one request: {a}"));
                    drop(sub);
                    other.disconnect();
                    a.is_rows()
                });
                Some((same_id.ok(), other_id.ok()))
            } else {
                client.disconnect();
                None
            };
            Some((verdict, scope))
        }
        Err(e) => {
            say(t0, &format!("D: could not connect as client id {id_d}: {e}"));
            None
        }
    };

    // B: three requests held together, on a fresh id, last.
    std::thread::sleep(Duration::from_secs(3));
    let mut held_answers = Vec::new();
    if let Ok(client) = connect(&addr, id_b) {
        let mut held = Vec::new();
        for n in 1..=3 {
            let (answer, sub) = ask(&client, &group);
            say(t0, &format!("B{n} (held open): {answer}"));
            held_answers.push(answer.is_rows());
            held.push(sub);
        }
        drop(held);
        client.disconnect();
    }

    report(t0, &passive, verdict, &held_answers);
    assert!(answered > 0, "not one request was answered, so nothing was measured");
}

// ---- the RESULT block ------------------------------------------------------------------------

type Scope = Option<(Option<bool>, Option<bool>)>;

fn report(
    t0: Instant,
    passive: &Result<Cadence, String>,
    d: Option<(Ladder, Scope)>,
    held: &[bool],
) {
    eprintln!(
        "\n================ RESULT ================ (decision table: M1 short window / M2s per session / M2c per client id)"
    );
    match passive {
        Err(e) => eprintln!("C pushes: NOT MEASURED ({e})"),
        Ok(c) if c.pushes.is_empty() => eprintln!(
            "C pushes: NONE in {PASSIVE_HOLD:?} after a dump of {} rows. A quiet account is pushed nothing (IB's page: \
             'those values which have changed'), so a kept subscription that reopened on silence would reopen every 20 \
             min (~68 a day). Fine under M1; FATAL under M2s/M2c: then never reopen on silence.",
            c.dump_rows
        ),
        Ok(c) => {
            let starts: Vec<f64> = c.pushes.iter().map(|p| p.at.as_secs_f64()).collect();
            let gaps: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
            let repeated: usize = c.pushes.iter().map(|p| p.rows - p.changed).sum();
            let gap_text = if gaps.is_empty() {
                "one push only".to_string()
            } else {
                format!(
                    "gaps between pushes min {:.0}s max {:.0}s",
                    gaps.iter().copied().fold(f64::INFINITY, f64::min),
                    gaps.iter().copied().fold(0.0, f64::max)
                )
            };
            eprintln!(
                "C pushes: {} in {PASSIVE_HOLD:?} (first at +{:.0}s), {gap_text}, {} rows REPEATED an unchanged value in \
                 all: {}. {}",
                c.pushes.len(),
                starts[0],
                repeated,
                if repeated > 0 {
                    "unchanged rows ARE resent (a heartbeat)"
                } else {
                    "only changed values are sent"
                },
                if repeated > 0 {
                    "a silence of ~3 missed pushes is a real signal (the frozen-figure warning could be tightened to it)."
                } else {
                    "A silent feed proves nothing: a silence is warned about, never acted on."
                }
            );
        }
    }
    match d {
        None => eprintln!("D retention: NOT MEASURED (no connection)"),
        Some((Ladder::SpentBefore(which, why), _)) => eprintln!(
            "D retention: INCONCLUSIVE. {which} was not answered ({why}): the allowance was spent before the ladder began \
             (the passive hold's cancelled request counts, or the per-pass daemon's does: the counter is per LOGIN and \
             cumulative). Rerun with the account's adapter stopped."
        ),
        Some((Ladder::Accepted { attempt, after, refused }, _)) => eprintln!(
            "D retention: M1. A3 was refused {refused} time(s) and ACCEPTED at attempt {attempt}, {:.0}s after A2's cancel: a \
             cancelled request stops counting within {:.0}s (and not before the previous offset). A reopen is safe if reopens \
             are further apart than that (a reopen every 20 min on silence was the first design, the ladder's last rung 21).{}",
            after.as_secs_f64(),
            after.as_secs_f64(),
            if after >= Duration::from_secs(20 * 60) {
                " ⚠ the window is as long as a 20-minute silence: reopen on silence is NOT safe."
            } else {
                ""
            }
        ),
        Some((Ladder::Never { refused }, scope)) => {
            eprintln!(
                "D retention: A3 REFUSED at all {refused} offsets up to 21 min: M2s or M2c (or a per-login count)."
            );
            match scope {
                Some((Some(true), _)) => eprintln!(
                    "E scope: M2s, PER SESSION. A new connection under the same id was accepted. A connection survives ONE \
                     reopen; the second is refused for good. Do not reopen on silence; rebuild the connection instead."
                ),
                Some((Some(false), Some(true))) => eprintln!(
                    "E scope: M2c, PER CLIENT ID (kept across sessions). The same id is still refused on a new connection, a \
                     different fresh id is accepted. The recon connection needs a fresh client id per connection, or another \
                     request (reqAccountUpdates). Do not ship the kept subscription as the cure."
                ),
                Some((Some(false), Some(false))) => eprintln!(
                    "E scope: PER LOGIN (Gateway-wide). Neither the same id nor a different fresh id is accepted: the count is \
                     the login's, not the connection's or the id's. The kept subscription cannot be the only holder; check \
                     what else (the per-pass daemon) is counted."
                ),
                _ => eprintln!("E scope: NOT MEASURED (a connection failed)"),
            }
        }
    }
    match held {
        [] => eprintln!("B held: NOT MEASURED"),
        [true, true, false] => eprintln!(
            "B held: the cap is TWO ACTIVE requests on a fresh connection (B3 refused after B1 and B2): the live limit is real \
             and a refusal is visible as [322]."
        ),
        other => eprintln!(
            "B held: {other:?} (accepted?). Read it only against D: if D never accepted, B1 is refused because cancelled \
             requests still count (D's answer), not because of the cap of two; B2 refused after B1 accepted means another \
             holder shares the count."
        ),
    }
    say(t0, "end of the run");
}
