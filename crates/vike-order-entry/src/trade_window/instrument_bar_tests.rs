//! The instrument bar's unit tests: the feed words and rows, the bar's row count by width, and
//! the row salts, with the account and symbol rows every test builds; the screen tests over a
//! drawn bar are `screen`'s.

use crate::trade_window::{SymbolPickerRow, TradeWindowAccountPickerRow};
use vike_model::Grid;
use vike_ui_theme::appearance::ThemeAppearance;
use vike_ui_theme::theme::ThemePaletteId;
use vike_ui_theme::type_scale::ThemeTextSize;

use super::*;

/// The FEED badge never reuses the trading-mode words: the mode chip says where an ORDER goes,
/// the badge where DEPTH comes from, and the two fail independently (moved from the DOM's
/// source strip, Ruling R1).
#[test]
fn the_feed_words_are_not_the_mode_words() {
    for s in [
        DataClientConnectionState::Connected,
        DataClientConnectionState::Connecting,
        DataClientConnectionState::Disconnected,
        DataClientConnectionState::Error,
        DataClientConnectionState::Unknown,
    ] {
        let w = feed_word(s);
        assert!(w.starts_with("FEED "), "{w}");
        for mode in ["LIVE", "PAPER", "DEMO"] {
            assert!(!w.contains(mode), "{w}");
        }
    }
}

/// A dead depth link is the alarm: red, like the DOM's strip (owner decision 4, 2026-09-29). Pinned to the
/// status each link state's badge was drawn in before the badge moved to `ui-theme.toml`'s `feed_badge`
/// map: the outline is that status's colour, and the words are its ink (`chip::badge_ink`, which sets a
/// muted badge's words in the caption grey) — on every theme. Changing a row of the table changes the
/// badge, and this test is what says so by name.
#[test]
fn a_dead_link_is_red_and_an_unknown_one_is_muted() {
    for id in ThemePaletteId::ALL {
        let t = ThemeTokens::from_appearance(&ThemeAppearance {
            theme: id,
            ..ThemeAppearance::default()
        });
        for (state, status) in [
            (DataClientConnectionState::Connected, ThemeStatus::Ok),
            (DataClientConnectionState::Connecting, ThemeStatus::Warning),
            (DataClientConnectionState::Disconnected, ThemeStatus::Error),
            (DataClientConnectionState::Error, ThemeStatus::Error),
            (DataClientConnectionState::Unknown, ThemeStatus::Muted),
        ] {
            let row = feed_row(state);
            assert_eq!(row.stroke.resolve(&t), status.color(), "{id:?} {state:?}: the outline");
            assert_eq!(row.text.resolve(&t), chip::badge_ink(&t, status), "{id:?} {state:?}: ink");
        }
    }
}

/// The map is the badge's own: every link state reads the row of its own name, no two share one, and no
/// row of the `feed_badge` map is left without a state; and each row's word is the one the badge always
/// said.
#[test]
fn every_link_state_has_its_own_feed_row_and_every_row_its_state() {
    let all = [
        (DataClientConnectionState::Connected, "FEED UP"),
        (DataClientConnectionState::Connecting, "FEED DIALLING"),
        (DataClientConnectionState::Disconnected, "FEED DOWN"),
        (DataClientConnectionState::Error, "FEED FAULT"),
        (DataClientConnectionState::Unknown, "FEED ?"),
    ];
    for (state, word) in all {
        assert_eq!(feed_row(state).key, format!("{state:?}").to_uppercase(), "{state:?}");
        assert_eq!(feed_word(state), word, "{state:?}: the badge's words");
        assert_eq!(
            all.iter().filter(|(other, _)| feed_row(*other) == feed_row(state)).count(),
            1,
            "{state:?}'s row is shared"
        );
    }
    for row in maps::feed_badge::ALL {
        assert!(
            all.iter().any(|(state, _)| feed_row(*state) == *row),
            "{} is no link state",
            row.key
        );
    }
}

/// The look the thresholds are calibrated at: Normal density, Small text (the scale Standard was
/// before 2026-10-05). The default look is bigger now, and its thresholds are grown by [`scale`].
fn calibration() -> ThemeTokens {
    ThemeTokens::from_appearance(&ThemeAppearance {
        text_size: ThemeTextSize::Small,
        ..ThemeAppearance::default()
    })
}

#[test]
fn the_bar_takes_two_rows_below_its_threshold() {
    let t = calibration();
    assert!(height(trade::TWO_ROWS_BELOW - 1.0, &t) > height(trade::TWO_ROWS_BELOW, &t));
}

/// Two pickers and a chip are wider than a 320 pt window, so the bar has a third row: the
/// symbol picker and the chip, the account picker, the price group. A 320 pt window — the
/// ticket-under view — leaves the bar 304 pt, which is three rows in every look.
#[test]
fn the_bar_takes_three_rows_below_its_second_threshold() {
    let t = calibration();
    assert!(height(trade::THREE_ROWS_BELOW - 1.0, &t) > height(trade::THREE_ROWS_BELOW, &t));
    assert_eq!(
        [trade::TWO_ROWS_BELOW, trade::THREE_ROWS_BELOW - 1.0].map(|w| rows(w, &t)),
        [1, 3],
        "at Normal density and Small text the thresholds are the widths themselves"
    );
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let a = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            assert_eq!(
                rows(304.0, &ThemeTokens::from_appearance(&a)),
                3,
                "{density:?} {text_size:?}"
            );
        }
    }
}

/// A venue with NO line is a link nobody reported on, so the badge says `FEED ?`, never `FEED DOWN`
/// about a link it knows nothing of. A line is read by the state its producer set, never by its
/// words: a datahub on a host named `error-box` that is live is up, and a line saying nothing that
/// its producer marked down is down.
#[test]
fn a_missing_line_is_an_unknown_link_and_a_line_is_its_producers_state() {
    assert_eq!(link_of(None), DataClientConnectionState::Unknown);
    assert_eq!(feed_word(link_of(None)), "FEED ?");
    assert_eq!(link_of(Some(&live_source())), DataClientConnectionState::Connected);
    assert_eq!(link_of(Some(&dialling_source())), DataClientConnectionState::Connecting);
    let named = DataClientStatus::new(
        DataClientConnectionState::Connected,
        "datahub error-box:7878 — 1/1 stream(s) live",
    );
    assert_eq!(link_of(Some(&named)), DataClientConnectionState::Connected);
    let silent = DataClientStatus::new(DataClientConnectionState::Disconnected, "");
    assert_eq!(link_of(Some(&silent)), DataClientConnectionState::Disconnected);
}

/// A list's salts are each row's key and the number of rows before it with the same key, as
/// the per-row scan gave them, so every id a held press is keyed on is unchanged and two rows
/// that look alike still never share one. They are worked out in ONE pass, each row's key read
/// once: the per-row scan read the rows before every row, every frame, while the picker was
/// open (n·(n+1)/2 reads: 100,000 at 450 accounts).
#[test]
fn a_lists_salts_are_each_rows_key_and_its_earlier_twins_read_in_one_pass() {
    let items = ["a", "b", "a", "c", "a", "b"];
    let reads = std::cell::Cell::new(0);
    let salts = row_salts(&items, |s| {
        reads.set(reads.get() + 1);
        *s
    });
    assert_eq!(salts, [("a", 0), ("b", 0), ("a", 1), ("c", 0), ("a", 2), ("b", 1)]);
    // CONTROL: the definition, worked out the slow way here.
    for (i, salt) in salts.iter().enumerate() {
        let twins = items[..i].iter().filter(|x| **x == items[i]).count();
        assert_eq!(*salt, (items[i], twins), "row {i}");
    }
    let distinct: std::collections::HashSet<_> = salts.iter().collect();
    assert_eq!(distinct.len(), items.len(), "two rows share a salt: {salts:?}");
    assert_eq!(reads.get(), items.len(), "each row's key is read once, not once per later row");
    assert!(row_salts(&[] as &[&str], |s| *s).is_empty(), "an empty list has no salts");
}

const BTC: Grid =
    Grid { tick_size: Some(0.1), lot_size: Some(0.001), min_notional: None, min_qty: Some(0.001) };
/// A live depth link's line, as the market-data session sets it.
fn live_source() -> DataClientStatus {
    DataClientStatus::new(
        DataClientConnectionState::Connected,
        "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
    )
}

/// A dialling depth link's line, as the market-data session sets it.
fn dialling_source() -> DataClientStatus {
    DataClientStatus::new(
        DataClientConnectionState::Connecting,
        "datahub 127.0.0.1:7878 — connecting",
    )
}

const NOT_RUNNING: &str = "Not running on the server.";

/// One account row: its label is its name, else "main".
fn row(
    venue: &'static str,
    venue_label: &'static str,
    account: Option<&'static str>,
    symbol: &'static str,
    mode: Option<AccountTier>,
) -> TradeWindowAccountPickerRow<'static> {
    TradeWindowAccountPickerRow {
        venue,
        venue_label,
        product: "",
        account,
        symbol,
        name: account.unwrap_or(vike_model::accounts::account_keys::UNLABELLED_ACCOUNT_NAME),
        mode,
        why_not: None,
        lost: None,
    }
}

/// One row of the picker's list, spelled the way the app builds it.
fn pick_row(
    venue: &'static str,
    venue_label: &'static str,
    symbol: &'static str,
    pair: &str,
) -> SymbolPickerRow<'static> {
    SymbolPickerRow { venue, venue_label, symbol, pair: pair.to_string(), current: false }
}

#[path = "instrument_bar_tests/screen.rs"]
#[cfg(test)]
mod screen;
