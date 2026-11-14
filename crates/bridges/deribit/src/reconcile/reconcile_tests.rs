use std::cell::RefCell;

use super::*;

fn trade(id: &str, ts: i64) -> Value {
    json!({"trade_id": id, "timestamp": ts})
}

/// Asks `trade_pages` over `pages` (one answer per request, in order) and returns the rows' ids and the
/// params of every request.
fn walk(pages: Vec<Value>, since: i64) -> (Result<Vec<String>, String>, Vec<Value>) {
    let asked = RefCell::new(Vec::new());
    let params =
        json!({"instrument_name": "BTC-PERPETUAL", "count": TRADES_PAGE_COUNT, "sorting": "asc"});
    let rows = trade_pages(
        params,
        since,
        |p| {
            let n = asked.borrow().len();
            asked.borrow_mut().push(p.clone());
            Ok(pages.get(n).cloned().unwrap_or_else(|| pages[pages.len() - 1].clone()))
        },
        |why| why,
    );
    let ids = rows.map(|r| r.iter().map(|t| json_text(&t["trade_id"])).collect());
    (ids, asked.into_inner())
}

/// F-2: a page that says `has_more` is followed from its newest trade (that millisecond included), a trade
/// handed again is dropped by its id, and `has_more: false` ends the walk.
#[test]
fn has_more_is_followed_from_the_newest_trade_and_a_repeat_is_dropped() {
    let (ids, asked) = walk(
        vec![
            json!({"trades": [trade("a", 10), trade("b", 20)], "has_more": true}),
            json!({"trades": [trade("b", 20), trade("c", 20), trade("d", 30)], "has_more": false}),
        ],
        5,
    );
    assert_eq!(ids.unwrap(), ["a", "b", "c", "d"]);
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0]["start_timestamp"], 5, "the window's start");
    assert_eq!(asked[1]["start_timestamp"], 20, "the newest trade read, its millisecond included");
    assert_eq!(asked[1]["count"], 1000);
}

/// A full page is followed even when the venue leaves `has_more` out; a page with no new trade and the
/// page cap fail the walk instead of returning what it has; no `since` asks no start.
#[test]
fn a_walk_that_cannot_finish_fails() {
    let full: Vec<Value> = (0..TRADES_PAGE_COUNT).map(|i| trade(&format!("t{i}"), 7)).collect();
    let (ids, asked) = walk(vec![json!({ "trades": full })], 0);
    assert!(ids.unwrap_err().contains("brought no new one"), "a full page, then the same page");
    assert_eq!(asked.len(), 2);
    assert!(asked[0].get("start_timestamp").is_none(), "no lower bound asks no start");

    let (ids, asked) = walk(
        (0..20)
            .map(|i| json!({"trades": [trade(&format!("t{i}"), i)], "has_more": true}))
            .collect(),
        1,
    );
    assert!(ids.unwrap_err().contains("still paging after 10 pages"));
    assert_eq!(asked.len(), TRADES_MAX_PAGES);
}
