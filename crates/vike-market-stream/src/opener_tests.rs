use vike_venues::reconnect::ReconnectBackoff;

use super::*;

/// **A hub link's reconnect waits, pinned verbatim before they moved onto the one ladder**
/// (`vike_venues::reconnect`'s `ReconnectBackoff`): the production [`RECONNECT_BASE`] doubling to
/// [`RECONNECT_MAX`], 250 ms to 5 s, one wait per failure, and a start-over (the reader's parked or
/// healthy session, the options feed's healthy one) waits the first step again. The reader
/// (`crate::market`'s `run`) and the gatewayhub client's options feed wait these.
#[test]
fn the_hub_links_waits_are_pinned_verbatim() {
    let millis = |ladder: &mut ReconnectBackoff, failures: usize| -> Vec<u64> {
        (0..failures)
            .map(|_| u64::try_from(ladder.failed().as_millis()).expect("a wait in range"))
            .collect()
    };
    let mut ladder = ReconnectBackoff::new(RECONNECT_BASE, RECONNECT_MAX);
    assert_eq!(millis(&mut ladder, 8), [250, 500, 1_000, 2_000, 4_000, 5_000, 5_000, 5_000]);
    ladder.reset();
    assert_eq!(millis(&mut ladder, 2), [250, 500], "started over");
}
