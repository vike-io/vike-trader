//! [`VenueLink`]: only a fresh "up" reads up, a loss stays a loss, and nothing said is not known.

use super::*;

fn lost(why: &str) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown {
        why: why.to_string(),
        effect: "orders are refused",
        failed_restarts: 2,
        retry_in: Duration::from_secs(30),
    })
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
