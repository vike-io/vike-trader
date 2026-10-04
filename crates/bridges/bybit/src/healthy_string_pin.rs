//! **One healthy string per VENUE, not per lane** — the gate behind [`super::LIVE_STATUS`].
//!
//! All three of this venue's market lanes (kline, mark, trades) write ONE shared
//! `Arc<Mutex<String>>`, and [`super::FeedCtx::set_status`] emits a journal line only on a text
//! TRANSITION. So per-lane healthy spellings would make every session boundary on either lane a
//! transition against the OTHER lane's text — a line per reconnect per lane, into a file layer
//! defaulting to `trace` (root `CLAUDE.md`'s 341 GB shape). That hazard is NEW with the success
//! disclosure: before it, healthy text was written once per feed and could not alternate.
//!
//! This is a SOURCE scan rather than an equality assertion, deliberately: with every arm
//! spelling `LIVE_STATUS` an equality check is vacuously true, and the thing that can actually
//! go wrong is a future lane inventing its own literal — which is exactly what `trades_main`
//! used to do (`"LIVE · Bybit trades {series_symbol}"`).

const SRC: &str = include_str!("market_feed.rs");

/// Every healthy literal in this module's CODE must be the ONE venue string. A suffixed
/// spelling (the old `trades_main` write) is the failure this refuses.
///
/// ⚠ COMMENT lines are skipped and the scan stops at this module, both load-bearing rather
/// than tidy: the comments above quote the very literal being refused (a citation is the
/// evidence for the rule, so deleting it to satisfy a scanner would be the wrong fix), and
/// this module's own assertion text spells two more. A scan that read them would be red on a
/// correct tree — measured, not hypothetical: the first round of this pin was.
#[test]
fn the_three_lanes_publish_one_healthy_string() {
    let mut found = Vec::new();
    for line in SRC.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("mod healthy_string_pin") {
            break;
        }
        let mut rest = line;
        while let Some(at) = rest.find("\"LIVE · ") {
            rest = &rest[at + 1..];
            let Some(end) = rest.find('"') else { break };
            found.push(&rest[..end]);
            rest = &rest[end..];
        }
    }
    assert!(
        !found.is_empty(),
        "no `LIVE · ` literal found at all — this scan has stopped seeing the module and is \
             checking nothing"
    );
    for lit in &found {
        assert_eq!(
            *lit,
            super::LIVE_STATUS,
            "every lane must publish the IDENTICAL healthy text on the shared status mutex; \
                 `{lit}` is a per-lane spelling. Found: {found:?}"
        );
    }
}

/// …and the constant itself must classify the way both consumers expect: `Connected` for
/// `vike_model::parse_feed_status` (hence `Healthy` for the reconcile gate) and `info!` for
/// [`super::FeedCtx::set_status`]'s level split. A healthy string that parses as anything else
/// is the ig defect — `"IG stream up (…)"` carried no Connected token and painted the muted dot.
#[test]
fn the_healthy_string_reads_as_connected_and_logs_as_info() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};
    assert_eq!(parse_feed_status(super::LIVE_STATUS), ConnectionState::Connected);
    assert!(super::LIVE_STATUS.starts_with(super::HEALTHY_STATUS_PREFIX));
}
