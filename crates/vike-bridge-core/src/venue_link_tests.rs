//! [`VenueLink`]: only a fresh "up" reads up, a loss stays a loss, and nothing said is not known.

use super::*;

/// A failure with no wait said: a wait counts down on read (`a_down_reading_counts_its_retry_wait_down`), so a reading
/// on the wall clock equals its word only when it has none.
fn lost(why: &str) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown {
        why: why.to_string(),
        effect: "orders are refused",
        failed_restarts: 2,
        retry_in: Duration::ZERO,
    })
}

/// A down path whose writer counts `failed_restarts` and waits `retry_in`.
fn down(why: &str, failed_restarts: u32, retry_in: Duration) -> VenueLinkDown {
    VenueLinkDown { why: why.to_string(), effect: "", failed_restarts, retry_in }
}

/// **Only a fresh "up" reads up.** A cell nobody wrote is not known, an "up" older than its writer's bound is not known
/// either (the writer stopped stepping), and a lost path stays lost however old the word.
#[test]
fn an_up_that_went_stale_is_unknown_and_a_loss_never_ages() {
    let link = VenueLink::default();
    let t = Instant::now();
    let fresh = Duration::from_secs(5);
    assert_eq!(link.state_at(t), VenueLinkState::Unknown, "nothing said yet");

    link.set_at(VenueLinkState::Up, Some(fresh), t);
    assert_eq!(link.state_at(t + fresh), VenueLinkState::Up);
    assert_eq!(link.state_at(t + fresh + Duration::from_millis(1)), VenueLinkState::Unknown);

    link.set_at(lost("the API socket is not connected"), Some(fresh), t);
    assert_eq!(
        link.state_at(t + fresh * 100),
        lost("the API socket is not connected"),
        "a lost path does not go stale into unknown"
    );

    link.set_at(VenueLinkState::Unknown, Some(fresh), t);
    assert_eq!(link.state_at(t), VenueLinkState::Unknown, "a new connection is not known yet");
}

/// An "up" with no bound holds until the next word, each word replaces the last, and a reset forgets it.
#[test]
fn an_up_with_no_bound_never_ages_and_a_reset_forgets_the_word() {
    let link = VenueLink::default();
    let t = Instant::now();
    link.set_at(VenueLinkState::Up, None, t);
    assert_eq!(link.state_at(t + Duration::from_secs(86_400)), VenueLinkState::Up);

    link.set(lost("reset by peer"), None);
    assert_eq!(link.state(), lost("reset by peer"));
    link.set(VenueLinkState::Up, None);
    assert_eq!(link.state(), VenueLinkState::Up, "the return reads up");

    link.reset();
    assert_eq!(link.state(), VenueLinkState::Unknown, "a new client is not known yet");
}

/// **A claim takes the cell for a new client**: it forgets the last word, the old claim's late write lands nowhere (an
/// exec thread that outlived its teardown), the new claim's does, and a reader sees the new client's word only.
#[test]
fn a_write_of_an_earlier_claim_is_ignored_after_a_new_claim() {
    let reader = venue_link("fxcm#VLINK-CLAIM");
    let old = reader.claim();
    old.set(VenueLinkState::Up, None);
    assert_eq!(reader.state(), VenueLinkState::Up);

    let new = reader.claim();
    assert_eq!(reader.state(), VenueLinkState::Unknown, "a claim forgets the last word");
    old.clone().set(lost("SessionLost"), None);
    assert_eq!(reader.state(), VenueLinkState::Unknown, "the old client's late word lands nowhere");
    new.clone().set(VenueLinkState::Up, None);
    assert_eq!(reader.state(), VenueLinkState::Up, "a clone of the new claim writes as it");
    reader.set(lost("unclaimed"), None);
    assert_eq!(reader.state(), lost("unclaimed"), "a handle that never claimed writes as before");
}

/// The cell is one per route key: an account reads its own, and every clone is the same cell.
#[test]
fn each_route_key_has_its_own_cell() {
    venue_link("ibkr#VLINK-A").set(VenueLinkState::Up, None);
    assert_eq!(venue_link("ibkr#VLINK-A").state(), VenueLinkState::Up);
    assert_eq!(venue_link("ibkr#VLINK-B").state(), VenueLinkState::Unknown);

    let held = venue_link("ibkr#VLINK-B");
    venue_link("ibkr#VLINK-B").set(lost("gone"), None);
    assert_eq!(held.state(), lost("gone"));
}

/// **The reading in the one state vocabulary**: not known is `Unknown`, up is `Connected`, a loss with no failed restart
/// yet is a reopen (`Connecting`) and one with a failed restart is `Error`.
#[test]
fn each_reading_states_its_connection_state() {
    use vike_model::feed_status::DataClientConnectionState::{
        Connected, Connecting, Error, Unknown,
    };
    let down = |failed_restarts| {
        VenueLinkState::Down(VenueLinkDown { failed_restarts, ..VenueLinkDown::default() })
    };
    assert_eq!(VenueLinkState::Unknown.connection_state(), Unknown);
    assert_eq!(VenueLinkState::Up.connection_state(), Connected);
    assert_eq!(down(0).connection_state(), Connecting);
    assert_eq!(down(1).connection_state(), Error);
    assert_eq!(lost("reset by peer").connection_state(), Error, "two restarts failed");
}

/// **A loss for a real reason is a failure, counted or not**: a bridge that counts no restarts still reads `Error`
/// (before, its `0` read as a routine reopen and the tradehub said the outage one ask late), and a count it keeps is kept.
#[test]
fn a_path_down_for_a_real_reason_reads_error_whatever_its_bridge_counts() {
    use vike_model::feed_status::DataClientConnectionState::Error;
    let uncounted = VenueLinkDown::failed("session lost".to_string(), "refused", 0, Duration::ZERO);
    assert_eq!(uncounted.failed_restarts, 1, "the loss is the first failure");
    assert_eq!(VenueLinkState::Down(uncounted).connection_state(), Error);
    let counted =
        VenueLinkDown::failed("restart refused".to_string(), "", 3, Duration::from_secs(40));
    assert_eq!(
        (counted.failed_restarts, counted.retry_in),
        (3, Duration::from_secs(40)),
        "a bridge's own count and wait are kept"
    );
}

/// **A cell of named sub-paths reads the worst of them**: a claimed path that said nothing keeps the cell not known, the
/// cell is up only when every path is, any loss is the reading (the first-claimed loss when two are lost), and a path
/// whose "up" went stale is not known.
#[test]
fn a_cell_of_sub_paths_reads_the_worst_of_them() {
    let link = VenueLink::default();
    let t = Instant::now();
    let fresh = Duration::from_secs(5);
    let orders = link.claim_path("order session");
    let stream = link.claim_path("trade stream");
    orders.set_at(VenueLinkState::Up, None, t);
    assert_eq!(link.state_at(t), VenueLinkState::Unknown, "the stream has said nothing yet");

    stream.set_at(VenueLinkState::Up, Some(fresh), t);
    assert_eq!(link.state_at(t), VenueLinkState::Up, "every path is up");
    assert_eq!(
        link.state_at(t + fresh + Duration::from_millis(1)),
        VenueLinkState::Unknown,
        "the stream's up went stale"
    );

    stream.set_at(lost("the trade stream dropped"), None, t);
    assert_eq!(link.state_at(t), lost("the trade stream dropped"), "one loss is the reading");
    orders.set_at(lost("the order session's login failed"), None, t);
    assert_eq!(
        link.state_at(t),
        lost("the order session's login failed"),
        "of two losses, the path claimed first is the reading"
    );
}

/// **A path claim is a writer's, as the cell's own claim is**: a later claim of the same name silences the earlier
/// handle and forgets its word, a path that leaves no longer counts, and a reset or a cell claim drops every path, so a
/// handle from before it writes nothing, even after the same name is claimed again.
#[test]
fn a_stale_path_handle_writes_nothing_and_a_path_that_left_no_longer_counts() {
    let link = venue_link("ctrader#VLINK-PATHS");
    let first = link.claim_path("ctid 1");
    first.set(lost("not authorized"), None);
    let again = link.claim_path("ctid 1");
    assert_eq!(link.state(), VenueLinkState::Unknown, "a new claim forgets the path's word");
    first.set(lost("the old actor's late refusal"), None);
    assert_eq!(link.state(), VenueLinkState::Unknown, "the earlier claim writes nothing");
    again.set(VenueLinkState::Up, None);
    let other = link.claim_path("ctid 2");
    other.set(lost("not authorized"), None);
    assert_eq!(link.state(), lost("not authorized"));
    other.leave();
    assert_eq!(link.state(), VenueLinkState::Up, "the path that left no longer counts");

    link.reset();
    assert_eq!(link.state(), VenueLinkState::Unknown, "a reset drops every path");
    let renewed = link.claim_path("ctid 1");
    again.set(lost("before the reset"), None);
    assert_eq!(
        link.state(),
        VenueLinkState::Unknown,
        "a handle from before the reset writes nothing"
    );
    renewed.set(VenueLinkState::Up, None);
    assert_eq!(link.state(), VenueLinkState::Up);
    let _claimed = link.claim();
    renewed.set(lost("before the claim"), None);
    assert_eq!(link.state(), VenueLinkState::Unknown, "a cell claim drops every path too");
}

/// **The cell's own word counts beside its paths once said**, and never while unsaid: a bridge that writes paths alone
/// reads its paths, and one that writes both reads the worse.
#[test]
fn the_cells_own_word_counts_beside_its_paths_only_once_said() {
    let link = VenueLink::default();
    link.claim_path("stream").set(VenueLinkState::Up, None);
    assert_eq!(link.state(), VenueLinkState::Up, "an unsaid own word does not count");
    link.set(lost("the session ended"), None);
    assert_eq!(link.state(), lost("the session ended"), "a said own word does");
}

/// **A Down's wait counts down** (the cell's rule 1): a reading serves `retry_in` less the time since it was said,
/// floored at zero, for the cell's own word and for a sub-path alike.
#[test]
fn a_down_reading_counts_its_retry_wait_down() {
    let t = Instant::now();
    let said = down("the sidecar died", 1, Duration::from_secs(30));
    let waiting = |retry_in| VenueLinkState::Down(VenueLinkDown { retry_in, ..said.clone() });

    let link = VenueLink::default();
    link.set_at(VenueLinkState::Down(said.clone()), None, t);
    assert_eq!(link.state_at(t), waiting(Duration::from_secs(30)), "as said");
    assert_eq!(link.state_at(t + Duration::from_secs(10)), waiting(Duration::from_secs(20)));
    assert_eq!(
        link.state_at(t + Duration::from_secs(40)),
        waiting(Duration::ZERO),
        "floored at zero"
    );

    let paths = VenueLink::default();
    paths.claim_path("stream").set_at(VenueLinkState::Down(said.clone()), None, t);
    assert_eq!(paths.state_at(t + Duration::from_secs(25)), waiting(Duration::from_secs(5)));
}

/// **A failure outranks a reopen** (rule 2): a path whose reopen has not failed yet (`failed_restarts == 0`) is not the
/// reading while a later-claimed path has failed.
#[test]
fn a_failure_outranks_an_earlier_claimed_reopen() {
    let t = Instant::now();
    let link = VenueLink::default();
    let reopening = link.claim_path("order session");
    let failing = link.claim_path("trade stream");
    reopening.set_at(
        VenueLinkState::Down(down("the venue ended the session", 0, Duration::ZERO)),
        None,
        t,
    );
    failing.set_at(
        VenueLinkState::Down(down("the stream's open was refused", 3, Duration::ZERO)),
        None,
        t,
    );
    assert_eq!(
        link.state_at(t),
        VenueLinkState::Down(down("the stream's open was refused", 3, Duration::ZERO))
    );
}

/// **A path that gave up outranks every other loss** (rule 2): an earlier-claimed failure is not the reading once a
/// later path gave up, whatever either counts.
#[test]
fn a_path_that_gave_up_outranks_an_earlier_claimed_failure() {
    let t = Instant::now();
    let link = VenueLink::default();
    let failing = link.claim_path("private stream 1");
    let ended = link.claim_path("private stream 2");
    failing.set_at(VenueLinkState::Down(down("the stream stalled", 5, Duration::ZERO)), None, t);
    ended.gave_up_at(down("the venue refused the key", 0, Duration::ZERO), t);
    assert_eq!(
        link.state_at(t),
        VenueLinkState::Down(down("the venue refused the key", 0, Duration::ZERO))
    );
    let later = link.claim_path("private stream 3");
    later.set_at(VenueLinkState::Down(down("a later stall", 9, Duration::ZERO)), None, t);
    assert_eq!(
        link.state_at(t),
        VenueLinkState::Down(down("the venue refused the key", 0, Duration::ZERO)),
        "and a later-claimed failure"
    );
}

/// **Among equal losses the first claimed is the reading** (rule 2), for two reopens, two failures and two paths that
/// gave up alike.
#[test]
fn equal_losses_read_the_first_claimed() {
    let t = Instant::now();
    for (failed_restarts, gives_up) in [(0, false), (2, false), (1, true)] {
        let link = VenueLink::default();
        let first = link.claim_path("first");
        let second = link.claim_path("second");
        for (path, why) in [(&first, "first's loss"), (&second, "second's loss")] {
            let loss = down(why, failed_restarts, Duration::ZERO);
            if gives_up {
                path.gave_up_at(loss, t);
            } else {
                path.set_at(VenueLinkState::Down(loss), None, t);
            }
        }
        assert_eq!(
            link.state_at(t),
            VenueLinkState::Down(down("first's loss", failed_restarts, Duration::ZERO)),
            "failed_restarts {failed_restarts}, gave up {gives_up}"
        );
    }
}
