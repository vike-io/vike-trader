//! [`open_observe_door`]'s non-loopback refusal, as the operator reads it.

use super::*;

/// **A wildcard `config.tradehub_addr` with no consent opens no door, and its one ERROR line names
/// the cure** (decision 0280, the owner's ruling of 2026-11-12). The container image consents for
/// nobody now, so this line is the whole of what a `docker run` operator learns at start. It returns
/// before any key read or socket, so the test needs neither.
#[test]
fn a_public_address_is_refused_without_the_consent_and_the_refusal_names_the_cure() {
    let (door, events) = vike_log::capture::captured(|| {
        open_observe_door(Some("0.0.0.0:7879"), true, false, None, None, None)
    });
    assert!(door.is_none(), "no consent, no door");
    let errors: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(errors.len(), 1, "one refusal line: {events:?}");
    let refusal = errors[0];
    assert_eq!(refusal.field("addr"), Some("0.0.0.0:7879"));
    for words in [
        "NOT loopback — observe server NOT started",
        "To open trading to the network on purpose, start the container with \
         `--allow-public-bind` and firewall the port to your own IP. Otherwise connect with \
         `vike-cli backend connect HOST --server-project DIR` over ssh.",
        "`vike-cli config set flags.tradehub_allow_public_bind true`",
    ] {
        assert!(refusal.message.contains(words), "{words:?} is missing from: {}", refusal.message);
    }
}
