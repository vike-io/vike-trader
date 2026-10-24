use super::*;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

/// A client over no ForexConnect session: it keeps the pairs it is told, as the real one does.
#[derive(Default)]
struct Fake {
    admitted: AdmittedSymbols,
}

impl ReconClient for Fake {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        Ok(Some(42.0))
    }
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit(symbol, "EURUSD");
    }
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }
}

/// An opener the test lets succeed when it says so, counting every attempt.
fn gated_opener(
    allow: &Arc<AtomicBool>,
    attempts: &Arc<AtomicU32>,
) -> impl FnMut() -> Attempt<Fake> + Send + 'static {
    let (allow, attempts) = (Arc::clone(allow), Arc::clone(attempts));
    move || {
        attempts.fetch_add(1, Ordering::SeqCst);
        if allow.load(Ordering::SeqCst) {
            Attempt::Opened(Fake::default())
        } else {
            Attempt::TryAgain
        }
    }
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for the retry to open the session");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The whole point of the module**: a session that did not open answers an ERROR (never an empty
/// report: an empty Trades table reads as a flat position), keeps being retried, and the attempt that
/// succeeds serves from then on, having been told every pair the engine admitted meanwhile.
#[test]
fn a_session_that_opens_late_errors_until_then_and_is_told_what_it_missed() {
    let (allow, attempts) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)));
    let client =
        Redial::start("EURUSD", gated_opener(&allow, &attempts), |_| Duration::from_millis(20));

    assert!(client.fetch_balance().unwrap_err().contains("not open"));
    assert!(client.fetch_order_status_reports(0).is_err());
    assert!(client.fetch_fill_reports(0).is_err());
    assert!(client.fetch_position_status_reports().is_err());
    client.admit_symbol("usdjpy");
    client.admit_symbol("EURUSD"); // the mounted pair is always read: not an admitted one
    assert_eq!(client.admitted_symbols(), Some(vec!["USDJPY".to_string()]));
    wait_until(|| attempts.load(Ordering::SeqCst) >= 3);
    assert!(client.fetch_balance().is_err(), "still down while every attempt fails");

    allow.store(true, Ordering::SeqCst);
    wait_until(|| client.fetch_balance().is_ok());
    assert_eq!(client.fetch_balance(), Ok(Some(42.0)));
    assert!(client.fetch_fill_reports(0).is_ok());
    assert_eq!(
        client.admitted_symbols(),
        Some(vec!["USDJPY".to_string()]),
        "the opened client was told the pair admitted while it was down"
    );
    // ...and one told after it opened reaches it too.
    client.admit_symbol("GBPJPY");
    assert_eq!(client.admitted_symbols(), Some(vec!["USDJPY".to_string(), "GBPJPY".to_string()]));
    let tries = attempts.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(attempts.load(Ordering::SeqCst), tries, "an opened session is not dialled again");
}

/// Dropping the client ends the retrying AT ONCE, not after the pause (up to ten minutes in
/// production): the gatewayhub's teardown joins this, and the shipped pause would otherwise stall it.
#[test]
fn dropping_the_client_wakes_the_retry_thread_out_of_its_pause() {
    let (allow, attempts) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)));
    let client =
        Redial::start("EURUSD", gated_opener(&allow, &attempts), |_| Duration::from_secs(3600));
    let started = Instant::now();
    drop(client);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(attempts.load(Ordering::SeqCst), 0, "no attempt was made inside the pause");
}

/// The shipped schedule: 30 s, 1 min, 2 min, 5 min, then every 10 minutes for good.
#[test]
fn the_pause_grows_and_the_last_one_repeats() {
    let secs: Vec<u64> = (1..=7).map(|n| pause_after(n).as_secs()).collect();
    assert_eq!(secs, [30, 60, 120, 300, 600, 600, 600]);
    assert_eq!(pause_after(u32::MAX).as_secs(), 600);
    assert_eq!(
        pause_after(0).as_secs(),
        30,
        "no failure yet is still the first pause, never a panic"
    );
}

/// The two credential refusals MEASURED against the real host, in the shape each reaches a log
/// (`crate::sys::FxcmError`'s `Display`), are terminal; the failures that heal are not.
#[test]
fn only_the_two_measured_credential_refusals_are_terminal() {
    for refused in [
        "FXCM login failed — the venue said: Login failed. Incorrect user name or password",
        "FXCM login failed — the venue said: User or connection doesn't exist.",
        "fxcm session unavailable: FXCM login failed — the venue said: login failed. INCORRECT USER NAME OR PASSWORD",
    ] {
        assert!(refuses_credentials(refused), "{refused}");
    }
    for heals in [
        "FXCM login failed — the venue said: Unable to obtain station descriptor. HTTP request failed \
         object='/Hosts.jsp?<query redacted: it carries the FXCM login name>' errorCode=56 (ORA-499)",
        "FXCM login failed (no answer from ForexConnect): no ForexConnect session-status callback arrived \
         inside the shim's wait window",
        "FXCM login failed (disconnected with no reason given): the ForexConnect session went Disconnected \
         without the SDK naming a reason",
        "FXCM ForexConnect shim not loaded (see `vike_fxcm::sdk_unavailable_reason`)",
        "",
    ] {
        assert!(!refuses_credentials(heals), "{heals}");
    }
}

/// **A refused credential is tried ONCE** and the thread ends, while a host that is down keeps being
/// tried: the same opener harness, only the verdict of the attempt differs. The reads then say why.
#[test]
fn a_refused_credential_stops_the_retrying_and_a_down_host_does_not() {
    fn attempts_in_a_while(verdict: fn() -> Attempt<Fake>) -> (u32, Redial<Fake>) {
        let attempts = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&attempts);
        let client = Redial::start(
            "EURUSD",
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                verdict()
            },
            |_| Duration::from_millis(10),
        );
        std::thread::sleep(Duration::from_millis(300));
        (attempts.load(Ordering::SeqCst), client)
    }

    let (tries, client) = attempts_in_a_while(|| Attempt::GiveUp);
    assert_eq!(tries, 1, "a refused credential is one failed login, not one per pause");
    let why = client.fetch_balance().unwrap_err();
    assert!(why.contains("refused") && why.contains("not retried"), "{why}");

    let (tries, client) = attempts_in_a_while(|| Attempt::TryAgain);
    assert!(tries >= 3, "a host that is down keeps the schedule: {tries} attempts");
    assert!(client.fetch_balance().unwrap_err().contains("retried"));
}

/// **Two retries never log in at the same moment**: the exec thread's and this client's both take
/// `one_retry_at_a_time` around the login, and they would otherwise tick in step.
#[test]
fn two_retries_take_turns_at_the_login() {
    let (inside, widest) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let slow_login = |inside: &Arc<AtomicU32>, widest: &Arc<AtomicU32>| {
        let (inside, widest) = (Arc::clone(inside), Arc::clone(widest));
        move || {
            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
            widest.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            inside.fetch_sub(1, Ordering::SeqCst);
            Attempt::<Fake>::TryAgain
        }
    };
    let pause = |_: u32| Duration::from_millis(1);
    let first = Redial::start("EURUSD", slow_login(&inside, &widest), pause);
    let second = Redial::start("EURUSD", slow_login(&inside, &widest), pause);
    std::thread::sleep(Duration::from_millis(400));
    drop((first, second));
    assert_eq!(widest.load(Ordering::SeqCst), 1, "two logins were in flight at once");
}
