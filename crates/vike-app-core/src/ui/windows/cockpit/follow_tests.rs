use super::*;

use crate::ui::windows::POLYMARKET_PLACEHOLDER_TOKEN;

const FAMILY: &str = "btc-updown-5m";
/// The end of the first window in the answers below; each next window ends five minutes later.
const END: i64 = 1_794_906_000_000;
const WINDOW: i64 = 300_000;

fn token(family: &str, token: &str, outcome: &str, end_ms: i64) -> LiveFamilyToken {
    LiveFamilyToken {
        family: family.to_string(),
        token: token.to_string(),
        outcome: outcome.to_string(),
        end_ms: Some(end_ms),
    }
}

/// The datahub's answer while windows `first` and `first + 1` are live: an `Up` and a `Down` token each.
fn windows(first: i64) -> Vec<LiveFamilyToken> {
    (first..first + 2)
        .flat_map(|w| {
            let end = END + (w - 1) * WINDOW;
            [
                token(FAMILY, &format!("{w}up"), "Up", end),
                token(FAMILY, &format!("{w}down"), "Down", end),
            ]
        })
        .collect()
}

/// One frame whose ask answers `answer` at once (it lands on the next frame); `None` asks nothing. Returns whether
/// the frame asked.
fn frame(
    follow: &mut CockpitFollow,
    symbol: &mut String,
    now_ms: i64,
    answer: Option<Result<Vec<LiveFamilyToken>, String>>,
) -> bool {
    let mut asked = false;
    follow.step(symbol, now_ms, |slot| {
        asked = true;
        *slot.lock().expect("slot") = answer;
        Ok(())
    });
    asked
}

/// **The follower picks the live window's Up token, and moves to the next window when it ends**: the placeholder
/// window asks on its first frame, takes window 1's `Up` token when the answer lands, asks nothing more until that
/// window ends, then moves to window 2's `Up` token at once (the answer listed it) and asks again, for what comes
/// after.
#[test]
fn the_follower_takes_the_live_up_token_and_moves_on_when_its_window_ends() {
    let mut follow = CockpitFollow::default();
    let mut symbol = POLYMARKET_PLACEHOLDER_TOKEN.to_string();
    let now = END - 100_000;

    assert!(frame(&mut follow, &mut symbol, now, Some(Ok(windows(1)))), "the first frame asks");
    assert_eq!(symbol, POLYMARKET_PLACEHOLDER_TOKEN, "nothing is followed before the answer lands");
    assert_eq!(follow.status(), Some(ASKING));

    assert!(
        !frame(&mut follow, &mut symbol, now + 16, None),
        "the answer is taken, not asked again"
    );
    assert_eq!(symbol, "1up");
    assert_eq!(follow.status(), None);
    assert!(
        !frame(&mut follow, &mut symbol, END - 1, None),
        "nothing is asked while the window is live"
    );
    assert_eq!(symbol, "1up");

    assert!(frame(&mut follow, &mut symbol, END, Some(Ok(windows(2)))), "its end asks again");
    assert_eq!(symbol, "2up", "the next window, from the answer already held");
    assert!(!frame(&mut follow, &mut symbol, END + 16, None));
    assert_eq!(symbol, "2up");
    assert!(frame(&mut follow, &mut symbol, END + WINDOW, Some(Ok(windows(3)))));
    assert_eq!(symbol, "3up");
}

/// **A placeholder is never subscribed**: with no answer (no datahub, an older one, no recorder, nothing live yet) the
/// window keeps the placeholder, so `crate::ui::feed_lifecycle`'s live set (what is subscribed and kept) stays empty,
/// and the cockpit says why in words. An older datahub's words name it as older.
#[test]
fn a_placeholder_window_without_an_answer_keeps_the_placeholder_and_says_why() {
    let mut w = crate::ui::window::WindowState::tool(
        "polymarket-0",
        WindowKind::Polymarket,
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0)),
    );
    w.symbol = POLYMARKET_PLACEHOLDER_TOKEN.to_string();
    let mut follow = CockpitFollow::default();
    let older =
        "the datahub is older than this desktop: it does not advertise `live_family_tokens`"
            .to_string();
    frame(&mut follow, &mut w.symbol, END, Some(Err(older.clone())));
    frame(&mut follow, &mut w.symbol, END + 16, None);
    assert_eq!(w.symbol, POLYMARKET_PLACEHOLDER_TOKEN);
    assert_eq!(follow.status(), Some(older.as_str()));
    assert!(crate::ui::feed_lifecycle::live_polymarket_tokens(std::slice::from_ref(&w)).is_empty());
    assert!(
        !frame(&mut follow, &mut w.symbol, END + 1_000, None),
        "a failed ask waits for the rail's window end"
    );

    let mut follow = CockpitFollow::default();
    follow.step(&mut w.symbol, END, |_| Err(COCKPIT_FOLLOW_NO_DATAHUB.to_string()));
    assert_eq!(
        (w.symbol.as_str(), follow.status()),
        (POLYMARKET_PLACEHOLDER_TOKEN, Some(COCKPIT_FOLLOW_NO_DATAHUB))
    );

    let mut follow = CockpitFollow::default();
    frame(&mut follow, &mut w.symbol, END, Some(Ok(Vec::new())));
    frame(&mut follow, &mut w.symbol, END + 16, None);
    assert_eq!(
        (w.symbol.as_str(), follow.status()),
        (POLYMARKET_PLACEHOLDER_TOKEN, Some(NO_LIVE_WINDOW))
    );
    assert!(
        frame(&mut follow, &mut w.symbol, END + 16 + RETRY_AFTER_MS, None),
        "nothing live is asked again"
    );
}

/// A token the answer lists as live is kept (a seeded live token, even a `Down` one); the family is the one whose
/// window length is the rail's, whatever its name's order.
#[test]
fn a_live_token_is_kept_and_the_rails_family_is_preferred() {
    let mut answer = windows(1);
    answer.insert(0, token("aaa-updown-15m", "slow", "Up", END + WINDOW));
    assert_eq!(live_up_token(&answer, None, END - 1).map(|t| t.token.as_str()), Some("1up"));
    assert_eq!(
        live_up_token(&answer, Some("aaa-updown-15m"), END - 1).map(|t| t.token.as_str()),
        Some("slow")
    );
    assert_eq!(live_up_token(&answer, None, END).map(|t| t.token.as_str()), Some("2up"));

    let mut follow = CockpitFollow::default();
    let mut symbol = "1down".to_string();
    frame(&mut follow, &mut symbol, END - 1_000, Some(Ok(answer)));
    frame(&mut follow, &mut symbol, END - 900, None);
    assert_eq!(symbol, "1down");
}
