//! Tests for `chip`: the status chip, the account tier chip and the presence marks.

use super::*;
use crate::appearance::Appearance;
use crate::color_math::contrast_ratio;
use crate::components::ON_FILL;
use crate::components::testing::{circles, ctx_with, fills, harness, named, paint, strokes, texts};
use crate::theme::{Theme, ThemeId};
use egui::accesskit::Role;

/// Every mode the kit draws has its `[[mode]]` row, and every row of `ui-theme.toml` is a mode the
/// kit draws: the two lists are compared by the label the chip prints, and the registry is the
/// GENERATED one, so a row added to the TOML without a variant (or the reverse) fails here.
#[test]
fn every_mode_has_its_row_and_every_row_its_mode() {
    for m in AccountTier::ALL {
        assert_eq!(tier_roles(m).name, tier_label(m), "{m:?}: its row is named for another mode");
        assert!(
            modes::ALL.iter().any(|row| row.name == tier_label(m)),
            "{m:?}: ui-theme.toml has no [[mode]] row named {}",
            tier_label(m)
        );
    }
    for row in modes::ALL {
        assert!(
            AccountTier::ALL.iter().any(|m| tier_label(*m) == row.name),
            "ui-theme.toml's [[mode]] {} is no mode of the kit",
            row.name
        );
    }
    assert_eq!(modes::ALL.len(), AccountTier::ALL.len(), "a mode has two rows");
}

/// The chip's word is the tier's ONE spelling upper-cased (decision 0156 item 6): what the account
/// row says (`paper`, `demo`, `live`) is what the chip prints, so the two cannot disagree. And the
/// legend's order lists every tier exactly once.
#[test]
fn a_chip_shows_the_tiers_own_word_upper_cased() {
    for m in AccountTier::ALL {
        assert_eq!(tier_label(m), m.as_str().to_uppercase(), "{m:?}");
        assert_eq!(TIERS_LISTED.iter().filter(|l| **l == m).count(), 1, "{m:?} listed once");
    }
    assert_eq!(
        TIERS_LISTED[0],
        AccountTier::Live,
        "the legend leads with the one real money moves on"
    );
}

/// The chip reads its colours from the row: for every mode on every theme, `colours` is the
/// row's three roles resolved, and a part whose role is `none` is `TRANSPARENT` / `Stroke::NONE`.
#[test]
fn a_chip_is_its_rows_roles_resolved() {
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for m in AccountTier::ALL {
            let row = tier_roles(m);
            let (fill, edge, ink) = tier_colours(m, &t);
            assert_eq!(fill, row.fill.resolve(&t), "{id:?} {m:?}: fill");
            assert_eq!(ink, row.text.resolve(&t), "{id:?} {m:?}: text");
            assert_eq!(edge, row.stroke.stroke(stroke::HAIRLINE, &t), "{id:?} {m:?}: stroke");
            assert_eq!(edge == Stroke::NONE, row.stroke == crate::roles::ColourRole::None);
        }
    }
}

/// LIVE is the accent SHAPE (spec §2, and the owner's ruling B of 2026-10-03, which took it
/// back from the danger red the Trade window's build had filled it in): the installed theme's
/// accent fill with the on-fill label, and no other mode is filled in the accent. DEMO is
/// outlined in the info blue, PAPER in the border grey. The label reads on its fill:
/// `contrast_ratio`, the 4.5:1 text floor, on every theme.
///
/// LIVE and DEMO also differ by SHAPE on every theme — filled and never outlined, against
/// outlined and never filled — so the mode never rests on colour alone (spec §3.1).
#[test]
fn live_is_the_accent_demo_the_info_paper_the_border() {
    let danger = Status::Error.color();
    let info = Status::Info.color();
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        let th = Theme::of(id);
        let live = paint(&ctx, |ui| {
            tier(ui, AccountTier::Live);
        });
        assert!(fills(&live).contains(&th.accent), "{id:?}: LIVE is the theme's accent fill");
        assert!(!fills(&live).contains(&danger), "{id:?}: LIVE is not the danger red");
        assert!(texts(&live).contains(&("LIVE".to_string(), ON_FILL)), "{id:?}");
        let (fill, _, ink) = tier_colours(AccountTier::Live, &Tokens::of(&ctx));
        let r = contrast_ratio(ink, fill);
        assert!(r >= 4.5, "{id:?}: the LIVE label on its fill: {r:.2}");
        let demo = paint(&ctx, |ui| {
            tier(ui, AccountTier::Demo);
        });
        assert!(strokes(&demo).contains(&info), "{id:?}");
        let paper = paint(&ctx, |ui| {
            tier(ui, AccountTier::Paper);
        });
        assert!(strokes(&paper).contains(&th.border), "{id:?}");
        for (other, shapes) in [("DEMO", &demo), ("PAPER", &paper)] {
            let f = fills(shapes);
            assert!(!f.contains(&th.accent), "{id:?}: {other} is not filled in the accent");
        }
        // The SHAPE: every rectangle LIVE paints is unoutlined, every one DEMO paints unfilled.
        let clear = |c: &Color32| *c == Color32::TRANSPARENT;
        assert!(strokes(&live).iter().all(clear), "{id:?}: LIVE is outlined: {live:?}");
        assert!(fills(&demo).iter().all(clear), "{id:?}: DEMO is filled: {demo:?}");
    }
}

/// LIVE's wash (the Trade window's LIVE confirm prompt) is a band of its own — neither the
/// background nor LIVE's fill — and everything that prompt writes on it reads: the theme's
/// text, the warning amber of a Stop that triggers at once, and the word LIVE in LIVE's own
/// fill (the theme's accent), each at the 4.5:1 text floor, on every theme. Carbon is the case
/// to watch: its wash is a tint of an amber, and the warning amber is written on it. CONTROL:
/// an unfilled mode's wash is the background itself.
#[test]
fn the_live_wash_keeps_what_is_written_on_it_readable() {
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        let (fill, ..) = tier_colours(AccountTier::Live, &t);
        let wash = tier_wash(AccountTier::Live, &t);
        assert_eq!(wash.a(), 255, "{id:?}: opaque, so it is measured exactly");
        assert!(wash != t.theme.bg && wash != fill, "{id:?}: a band of its own");
        for (what, ink) in [
            ("the text", t.theme.text),
            ("the warning amber", Status::Warning.color()),
            ("LIVE's fill", fill),
        ] {
            let r = contrast_ratio(ink, wash);
            assert!(r >= 4.5, "{id:?}: {what} on LIVE's wash: {r:.2}");
        }
        for m in [AccountTier::Demo, AccountTier::Paper] {
            assert_eq!(tier_wash(m, &t), t.theme.bg, "{id:?}: CONTROL: {m:?} has no fill to wash");
        }
    }
}

#[test]
fn the_selected_account_is_a_label_and_the_others_buttons() {
    let h = harness(Appearance::default(), |ui| {
        account(ui, "main", AccountTier::Live, true);
        account(ui, "hedge", AccountTier::Demo, false);
    });
    assert_eq!(named(&h, Role::Label), ["main"]);
    assert_eq!(named(&h, Role::Button), ["hedge"]);
}

/// The mode rests on SHAPE, not on colour alone (spec §3.1). So an account shows its mode as
/// that mode's own chip — LIVE filled in the accent, DEMO outlined in the info blue — never as
/// a coloured dot, and the selected account is outlined in the accent: on every theme.
#[test]
fn an_account_shows_its_mode_as_a_chip_not_a_dot_on_every_theme() {
    let info = Status::Info.color();
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        let th = Theme::of(id);
        let live = paint(&ctx, |ui| {
            account(ui, "main", AccountTier::Live, false);
        });
        let demo = paint(&ctx, |ui| {
            account(ui, "hedge", AccountTier::Demo, false);
        });
        assert!(circles(&live).is_empty() && circles(&demo).is_empty(), "{id:?}: a mode dot");
        assert!(fills(&live).contains(&th.accent), "{id:?}: LIVE is the accent fill");
        assert!(texts(&live).contains(&("LIVE".to_string(), ON_FILL)), "{id:?}");
        assert!(strokes(&demo).contains(&info), "{id:?}: DEMO is outlined in the info blue");
        assert!(!fills(&demo).contains(&info), "{id:?}: DEMO is filled");
        assert!(texts(&demo).contains(&("DEMO".to_string(), info)), "{id:?}");
        let selected = paint(&ctx, |ui| {
            account(ui, "hedge", AccountTier::Demo, true);
        });
        assert!(strokes(&selected).contains(&th.accent), "{id:?}: the selection outline");
    }
}

/// A status dot is its status colour on every theme — "connected" is never the accent.
#[test]
fn a_status_dot_is_its_status_colour_on_every_theme() {
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        for s in Status::ALL {
            let shapes = paint(&ctx, |ui| {
                status_dot(ui, s, "Connected");
            });
            assert_eq!(circles(&shapes), [s.color()], "{id:?} {s:?}");
        }
    }
}

/// An unknown count draws NOTHING; a known zero draws "0" (spec §4.2).
#[test]
fn an_unknown_count_draws_nothing_and_a_known_zero_draws_zero() {
    let ctx = ctx_with(&Appearance::default());
    let mut none = Some(());
    let unknown = paint(&ctx, |ui| none = count(ui, None).map(|_| ()));
    assert!(unknown.is_empty() && none.is_none());
    let zero = paint(&ctx, |ui| {
        count(ui, Some(0));
    });
    assert!(texts(&zero).iter().any(|(s, _)| s == "0"));
}

/// A badge's text reads (4.5:1) on the background and on a card, for every status and theme.
#[test]
fn every_badge_reads_on_every_theme() {
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for s in Status::ALL {
            for ground in [t.theme.bg, t.theme.card] {
                let r = contrast_ratio(badge_ink(&t, s), ground);
                assert!(r >= 4.5, "{id:?} {s:?}: {r:.2}");
            }
        }
    }
}

#[test]
fn the_legend_names_every_mark() {
    let ctx = ctx_with(&Appearance::default());
    let shapes = paint(&ctx, presence_legend);
    let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
    for m in Presence::ALL {
        assert!(
            t.contains(&m.glyph().to_string()) && t.contains(&m.word().to_string()),
            "{m:?}: {t:?}"
        );
    }
}

/// The marks' colours and words, pinned to what the kit printed before they moved to
/// `ui-theme.toml`'s `presence` map: configured the status green, not set the status grey, no such
/// tier the installed theme's caption grey, not measured the status amber — on every theme. Changing
/// a row of the table changes the mark, and this test is what says so by name.
#[test]
fn a_mark_wears_the_colour_and_the_word_the_presence_map_gives_it() {
    for id in ThemeId::ALL {
        let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
        for (mark, colour, word) in [
            (Presence::Configured, Status::Ok.color(), "configured"),
            (Presence::NotSet, Status::Muted.color(), "not set"),
            (Presence::NoSuchTier, t.theme.text3, "no such tier"),
            (Presence::Unknown, Status::Warning.color(), "not measured"),
        ] {
            assert_eq!(mark.color(&t), colour, "{id:?} {mark:?}: the colour");
            assert_eq!(mark.word(), word, "{id:?} {mark:?}: the word");
        }
    }
}

/// The map is the kit's own: every mark reads the row of its own name, no two share one, and no row of
/// the `presence` map is left without a mark.
#[test]
fn every_mark_has_its_own_row_and_every_row_its_mark() {
    let all = [
        (Presence::Configured, "CONFIGURED"),
        (Presence::NotSet, "NOT_SET"),
        (Presence::NoSuchTier, "NO_SUCH_TIER"),
        (Presence::Unknown, "UNKNOWN"),
    ];
    assert_eq!(all.map(|(m, _)| m), Presence::ALL, "the list names every mark, in ALL's order");
    for (m, key) in all {
        assert_eq!(m.row().key, key, "{m:?} reads another row");
        assert_eq!(
            all.iter().filter(|(o, _)| o.row() == m.row()).count(),
            1,
            "{m:?}'s row is shared"
        );
    }
    for row in maps::presence::ALL {
        assert!(all.iter().any(|(m, _)| m.row() == *row), "{} is no mark of the kit", row.key);
    }
}

/// A badge in two named colours is the status badge when they are the status's own: the same outline
/// and the same ink, on every theme — which is what lets the Trade window's FEED badge be drawn from
/// its map row without changing a pixel of the badge it was.
#[test]
fn a_badge_in_two_named_colours_is_the_status_badge_in_those_colours() {
    for id in ThemeId::ALL {
        let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
        let t = Tokens::of(&ctx);
        for s in Status::ALL {
            let by_status = paint(&ctx, |ui| {
                badge(ui, "FEED UP", s);
            });
            let by_colours = paint(&ctx, |ui| {
                badge_coloured(ui, &t, "FEED UP", s.color(), badge_ink(&t, s));
            });
            assert_eq!(strokes(&by_colours), strokes(&by_status), "{id:?} {s:?}: the outline");
            assert_eq!(texts(&by_colours), texts(&by_status), "{id:?} {s:?}: the words");
            assert!(
                strokes(&by_colours).contains(&s.color()),
                "{id:?} {s:?}: outlined in the status"
            );
        }
    }
}
