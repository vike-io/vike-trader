//! The News window body — the filterable RSS headline list + reader pane (favicon avatars from
//! [`NewsInputs::logos`]); its multi-select filter pill is `filter`'s. Moved verbatim from
//! `vike-app`'s `main.rs` (tool-view extraction batch 1), then out of `vike-app-core` into this crate.

mod filter;

use std::collections::HashMap;
use std::sync::Arc;

use crate::NewsView;
use crate::category::{NEWS_CATEGORIES, NEWS_MARKETS, classify_news};
use crate::follow::NewsFollow;
use filter::news_filter_pill;
use vike_datahub_client::tool_feeds::NewsItem;
use vike_datahub_client::tool_feeds::poll::{
    DatahubPollHealth, DatahubPollInterest, DatahubPollLink,
};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::news;
use vike_ui_theme::{fonts, icons};

/// What the News window reads: the datahub poll's news half
/// (`vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot`'s `news`, `news_status`,
/// `news_health` and `feeds_link`) and the source icons the shell decoded. `vike-app-core`'s
/// `WindowToolContext::news_inputs` builds it.
pub struct NewsInputs<'a> {
    /// The headlines, as the datahub last answered them.
    pub items: &'a [NewsItem],
    /// The feed's ONE sentence: LIVE with its age, or the reason it is not.
    pub status: &'a str,
    /// How the feed stands, as DATA beside its sentence: the footer's colour reads this.
    pub health: DatahubPollHealth,
    /// The poll's link: the window records its interest on it, and Refresh wakes it.
    pub link: &'a DatahubPollLink,
    /// The providers' icon textures keyed by source name (`NewsItem::source`); a source without one
    /// draws its coloured initial.
    pub logos: &'a HashMap<String, egui::TextureHandle>,
    /// What "Follow chart" follows: the focused chart's `(venue, symbol)`, or `None` while no
    /// chart is on screen.
    pub chart: Option<(String, String)>,
    /// The instrument catalog the chart's asset is read through, once per chart
    /// ([`crate::NewsFollowCache`]).
    pub catalog: &'a Arc<vike_catalog::Catalog>,
}

/// The News window body: the filterable headline list + reader pane (favicon avatars from
/// `logos`). Split out of `tool_content`'s match arm verbatim (dedup-app refactor).
pub fn news_tool_content(ui: &mut egui::Ui, inputs: &NewsInputs<'_>, view: &mut NewsView) {
    use egui::RichText;
    use egui::{Align, Align2, Color32, FontId, Layout, Sense, vec2};
    let tokens = ThemeTokens::of(ui.ctx());
    let logos = inputs.logos;
    // Record that this window shows the news, so the poll thread keeps asking the datahub for it.
    inputs.link.want(DatahubPollInterest::News);
    let now = chrono::Utc::now().timestamp_millis();
    let ago = |timestamp: i64| -> String {
        if timestamp == 0 {
            return "—".into();
        }
        let s = ((now - timestamp) / 1000).max(0);
        if s < 60 {
            "just now".into()
        } else if s < 3600 {
            format!("{}m ago", s / 60)
        } else if s < 86400 {
            format!("{}h ago", s / 3600)
        } else {
            format!("{}d ago", s / 86400)
        }
    };
    // 8-colour fallback avatar palette, hashed by source (news.py _AV_COLORS)
    const AVATAR_PALETTE: [Color32; 8] = [
        news::AVATAR_1,
        news::AVATAR_2,
        news::AVATAR_3,
        news::AVATAR_4,
        news::AVATAR_5,
        news::AVATAR_6,
        news::AVATAR_7,
        news::AVATAR_8,
    ];
    let avatar = |ui: &mut egui::Ui, sz: f32, src: &str| {
        let (ar, _) = ui.allocate_exact_size(vec2(sz, sz), Sense::hover());
        if let Some(tex) = logos.get(src) {
            // real favicon on a rounded white backing (Py news_logos: rounded badge)
            ui.painter().rect_filled(ar, sz * news::AVATAR_RADIUS_FRACTION, news::LOGO_PLATE);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(tex.id(), ar.shrink(space::SM), uv, vike_ui_theme::color::UNTINTED);
        } else {
            let col = AVATAR_PALETTE[src.bytes().map(|b| b as usize).sum::<usize>() % 8];
            ui.painter().rect_filled(ar, sz * news::AVATAR_RADIUS_FRACTION, col);
            ui.painter().text(
                ar.center(),
                Align2::CENTER_CENTER,
                src.chars().next().unwrap_or('•').to_uppercase().to_string(),
                FontId::proportional(sz * news::AVATAR_LETTER_FRACTION),
                tokens.theme.background, // BG, matching Py avatar fg
            );
        }
    };

    // Read through the catalog only when the chart or the catalog changed, and only while on.
    let followed = if view.follow {
        view.follow_words.words(inputs.catalog, inputs.chart.as_ref()).cloned()
    } else {
        None
    };
    let visible = visible_headlines(inputs.items, view, followed.as_ref());
    if view.selection >= visible.len() {
        view.selection = 0;
    }
    let market_options: Vec<String> = NEWS_MARKETS.iter().map(|s| s.to_string()).collect();
    let category_options: Vec<String> = NEWS_CATEGORIES.iter().map(|s| s.to_string()).collect();
    let provider_options: Vec<String> = {
        let mut v: std::collections::HashSet<String> =
            inputs.items.iter().map(|n| n.source.clone()).collect();
        let mut v: Vec<String> = v.drain().collect();
        v.sort();
        v
    };

    // ---- filter toolbar: Market · Category · Provider · ⌖Follow · ……  Search · ↻ ----
    // WRAPPED: on a narrow window the controls take another row rather than running past its edge,
    // which would widen every row below the toolbar, the reader pane's included.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::LG;
        ui.spacing_mut().button_padding = vec2(space::XL2, space::LG); // vike toolbar controls are roomier
        news_filter_pill(ui, "Market", &market_options, &mut view.markets);
        news_filter_pill(ui, "Category", &category_options, &mut view.categories);
        news_filter_pill(ui, "Provider", &provider_options, &mut view.providers);
        // One size for the icon and its words.
        const FOLLOW_TEXT: ThemeTextRole = ThemeTextRole::Title;
        // ON is shown by the accent OUTLINE below (a shape); the words keep the text ink.
        let follow_ink = if view.follow { tokens.theme.text } else { tokens.theme.text_secondary };
        let follow = ui.add(
            egui::Button::new((
                icons::FOLLOW_CHART.rich().color(follow_ink).size(role_px(ui.ctx(), FOLLOW_TEXT)),
                RichText::new("Follow chart")
                    .color(follow_ink)
                    .size(role_px(ui.ctx(), FOLLOW_TEXT)),
            ))
            .fill(tokens.theme.surface)
            .stroke(egui::Stroke::new(
                stroke::HAIRLINE,
                if view.follow { tokens.theme.accent } else { tokens.theme.border },
            ))
            .min_size(vec2(0.0, news::TOOLBAR_BUTTON_HEIGHT)),
        );
        if follow.clicked() {
            view.follow = !view.follow;
        }
        // Search and Refresh hold the row's right end when they fit beside the pills, and take a
        // row of their own when they do not; the field narrows to what that row has.
        let refresh_w = egui::WidgetText::from(icons::REFRESH)
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            )
            .size()
            .x
            + 2.0 * ui.spacing().button_padding.x;
        if ui.available_size_before_wrap().x
            < news::SEARCH_WIDTH + refresh_w + ui.spacing().item_spacing.x
        {
            ui.end_row();
            // A new row starts zero tall, and the controls centre on it: give it their height.
            ui.set_row_height(news::TOOLBAR_BUTTON_HEIGHT);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Refresh pill → ask the datahub NOW instead of at the next minute.
            if icons::named(ui.button(icons::REFRESH), "Refresh").clicked() {
                inputs.link.refresh();
            }
            let search_w = news::SEARCH_WIDTH.min(ui.available_width());
            ui.add(
                egui::TextEdit::singleline(&mut view.query)
                    .hint_text("Search headlines…")
                    .desired_width(search_w)
                    // As tall as the buttons beside it, its text centred in that height.
                    .min_size(vec2(search_w, news::TOOLBAR_BUTTON_HEIGHT))
                    .align(egui::Align2([Align::LEFT, Align::Center])),
            );
        });
    });
    ui.add_space(space::MD);

    // The list and the reader SHARE this width: the list takes its fraction, the divider its gaps
    // (with the item spacing egui adds after the list and after the divider), and the reader what
    // is left, never more.
    let full = ui.available_width();
    let reader_on = view.reader_open && !visible.is_empty();
    let divider_w = 2.0 * (space::LG + ui.spacing().item_spacing.x) + stroke::HAIRLINE;
    // Too narrow for both panes at their least widths: the reader takes the whole width, and its
    // Close returns to the list.
    let stacked = reader_on && full < news::LIST_MIN_WIDTH + divider_w + news::READER_MIN_WIDTH;
    let list_w = if reader_on {
        (full * news::LIST_FRACTION).clamp(
            news::LIST_MIN_WIDTH,
            (full - divider_w - news::LIST_LEAVES_READER_WIDTH).max(news::LIST_MIN_WIDTH),
        )
    } else {
        full
    };
    // The footer's line and a gap above it: a clipped last row must not touch the sentence below.
    let split_h = (ui.available_height() - space::XL3 - space::MD).max(news::SPLIT_MIN_HEIGHT);
    ui.horizontal_top(|ui| {
        // ----- headline list -----
        if !stacked {
            ui.allocate_ui_with_layout(vec2(list_w, split_h), Layout::top_down(Align::Min), |ui| {
                egui::ScrollArea::vertical().id_salt("news_list").show(ui, |ui| {
                    if inputs.items.is_empty() {
                        // The footer's sentence says why nothing is here (a refusal, no datahub, a first
                        // poll); the list says only that it is waiting, and never repeats the sentence.
                        if inputs.status.is_empty() {
                            ui.weak("asking the datahub…");
                        }
                    } else if visible.is_empty() {
                        ui.add_space(space::LG);
                        ui.weak(empty_line(inputs.items, view, followed.as_ref()));
                    }
                    for (vi, n) in visible.iter().enumerate() {
                        let selected = vi == view.selection;
                        let inner = egui::Frame::new()
                            .fill(if selected {
                                tokens.theme.surface
                            } else {
                                Color32::TRANSPARENT
                            })
                            .corner_radius(egui::CornerRadius::same(RADIUS))
                            .inner_margin(egui::Margin::symmetric(space::MD as i8, space::LG as i8))
                            .show(ui, |ui| {
                                ui.set_width(list_w - news::CARD_INNER_RESERVE);
                                ui.horizontal_top(|ui| {
                                    avatar(ui, news::AVATAR_SIZE, &n.source);
                                    ui.add_space(space::LG);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{}  ·  {}",
                                                ago(n.timestamp_ms),
                                                n.source
                                            ))
                                            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                                            .color(tokens.theme.text_tertiary),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&n.title)
                                                    .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                                    .color(tokens.theme.text),
                                            )
                                            .wrap(),
                                        );
                                    });
                                });
                            });
                        let resp = ui.interact(
                            inner.response.rect,
                            egui::Id::unique(("news_row", vi)),
                            Sense::click(),
                        );
                        if resp.clicked() {
                            view.selection = vi;
                            view.reader_open = true;
                        }
                        // A selection that moved (a click, a reset, a pick made elsewhere) is brought into
                        // view ONCE, so it can sit far down the list without staying off screen.
                        if selected {
                            let seen = ui.scope_id().with("news_sel_seen");
                            if ui.data(|d| d.get_temp::<usize>(seen)) != Some(vi) {
                                ui.scroll_to_rect(inner.response.rect, None);
                                ui.data_mut(|d| d.insert_temp(seen, vi));
                            }
                        }
                        ui.add(egui::Separator::default().spacing(space::SM));
                    }
                });
            });
        }
        if reader_on {
            if !stacked {
                ui.add_space(space::LG);
                let (sharpe, _) =
                    ui.allocate_exact_size(vec2(stroke::HAIRLINE, split_h), Sense::hover());
                ui.painter().rect_filled(sharpe, 0.0, tokens.theme.border);
                ui.add_space(space::LG);
            }
            // ----- reader pane -----
            // What the row has LEFT, read after the list, the divider and the item spacing after
            // each: a reserve written down beside them was short by that spacing (7 points at the
            // default density) and the window's body scrolled sideways. Never floored at a minimum
            // either: a floor is a width past the window's edge.
            let reader_w = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(reader_w, split_h),
                Layout::top_down(Align::Min),
                |ui| {
                    if let Some(n) = visible.get(view.selection) {
                        // Close first, the source truncated into what it leaves: a long provider
                        // name never pushes the button past the pane's edge.
                        egui::Sides::new().height(news::AVATAR_SIZE).shrink_left().truncate().show(
                            ui,
                            |ui| {
                                avatar(ui, news::AVATAR_SIZE, &n.source);
                                ui.add_space(space::LG);
                                ui.label(
                                    RichText::new(&n.source)
                                        .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                        .color(tokens.theme.text),
                                );
                            },
                            |ui| {
                                if icons::named(ui.button(icons::CLOSE), "Close").clicked() {
                                    view.reader_open = false;
                                }
                            },
                        );
                        ui.add_space(space::LG);
                        ui.add(
                            egui::Label::new(
                                fonts::semibold(&n.title)
                                    .size(role_px(ui.ctx(), ThemeTextRole::Display))
                                    .color(tokens.theme.text),
                            )
                            .wrap(),
                        );
                        ui.add_space(space::MD);
                        let date = chrono::DateTime::from_timestamp_millis(n.timestamp_ms)
                            .map(|d| {
                                d.with_timezone(&chrono::Local)
                                    .format("%b %d, %Y · %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!("{}  ·  {}", date, ago(n.timestamp_ms)))
                                .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                .color(tokens.theme.text_secondary),
                        );
                        ui.add_space(space::XL);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&n.summary)
                                    .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                    .color(tokens.theme.text_secondary),
                            )
                            .wrap(),
                        );
                        ui.add_space(space::XL);
                        // topic chips: classify + feed tags + market, deduped, up to 6
                        let mut chips: Vec<String> = Vec::new();
                        let push = |s: String, chips: &mut Vec<String>| {
                            if !s.is_empty()
                                && !chips.iter().any(|c| c.eq_ignore_ascii_case(&s))
                                && chips.len() < 6
                            {
                                chips.push(s);
                            }
                        };
                        push(classify_news(&n.title, &n.tags).to_string(), &mut chips);
                        for t in &n.tags {
                            push(t.clone(), &mut chips);
                        }
                        if !n.market.is_empty() {
                            let mut m = n.market.clone();
                            if let Some(c) = m.get_mut(0..1) {
                                c.make_ascii_uppercase();
                            }
                            push(m, &mut chips);
                        }
                        if !chips.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(space::MD, space::MD);
                                for c in &chips {
                                    egui::Frame::new()
                                        .fill(tokens.theme.surface)
                                        .corner_radius(egui::CornerRadius::same(RADIUS))
                                        .inner_margin(egui::Margin::symmetric(
                                            space::XL as i8,
                                            space::SM as i8,
                                        ))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(c)
                                                    .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                                                    .color(tokens.theme.text),
                                            );
                                        });
                                }
                            });
                            ui.add_space(space::XL);
                        }
                        let has_url = !n.url.is_empty();
                        // One size for the icon and its words.
                        const OPEN_TEXT: ThemeTextRole = ThemeTextRole::Title;
                        let open = ui.add_enabled(
                            has_url,
                            egui::Button::new((
                                icons::EXTERNAL
                                    .rich()
                                    .size(role_px(ui.ctx(), OPEN_TEXT))
                                    .color(tokens.theme.text),
                                RichText::new("Open original")
                                    .size(role_px(ui.ctx(), OPEN_TEXT))
                                    .color(tokens.theme.text),
                            )),
                        );
                        if open.clicked() {
                            view.open_url = Some(n.url.clone());
                        }
                    }
                },
            );
        }
    });
    ui.add_space(space::SM + space::MD);
    let total = inputs.items.len();
    let filtered = visible.len() != total;
    let status = if filtered {
        format!("{} of {} headlines", visible.len(), total)
    } else {
        inputs.status.to_string()
    };
    // The sentence wears the feed's state (LIVE, STALE, a refusal): a filter count is not a state.
    let ink =
        footer_ink(filtered, inputs.health).map_or(tokens.theme.text_tertiary, ThemeStatus::color);
    ui.label(RichText::new(status).size(role_px(ui.ctx(), ThemeTextRole::Body)).color(ink));
}

/// The headlines the list shows, in the poll's order: every filter must pass (pure, over the
/// already-fetched headlines). With Follow chart on, that includes naming `followed`, the focused
/// chart's asset ([`NewsFollow::is_named_in`]); with no chart to follow it filters nothing.
fn visible_headlines<'a>(
    items: &'a [NewsItem],
    view: &NewsView,
    followed: Option<&NewsFollow>,
) -> Vec<&'a NewsItem> {
    let q = view.query.to_lowercase();
    let followed = followed.filter(|_| view.follow);
    items
        .iter()
        .filter(|n| {
            (view.markets.is_empty()
                || view.markets.iter().any(|m| m.eq_ignore_ascii_case(&n.market)))
                && (view.providers.is_empty() || view.providers.contains(&n.source))
                && (view.categories.is_empty()
                    || view.categories.iter().any(|c| c == classify_news(&n.title, &n.tags)))
                && (q.is_empty()
                    || n.title.to_lowercase().contains(&q)
                    || n.summary.to_lowercase().contains(&q))
                && followed.is_none_or(|f| f.is_named_in(n))
        })
        .collect()
}

/// What an empty list says. When Follow chart is what emptied it (the other filters alone keep
/// some), it names the asset and the way out, so following never leaves a silent empty list.
fn empty_line(items: &[NewsItem], view: &NewsView, followed: Option<&NewsFollow>) -> String {
    match followed.filter(|_| view.follow && !visible_headlines(items, view, None).is_empty()) {
        Some(f) => format!("No headlines name {}; turn off Follow chart to see all.", f.ticker),
        None => "No headlines match the current filters.".to_string(),
    }
}

/// The status colour of the footer sentence, from the feed's STATE as data: `None` is the dim
/// ink (a filter's "x of y headlines" count, a feed not asked yet, or a link riding out a short
/// failure), where LIVE, STALE, a first poll in flight and a refusal each wear their own.
fn footer_ink(filtered: bool, health: DatahubPollHealth) -> Option<ThemeStatus> {
    if filtered {
        return None;
    }
    match health {
        DatahubPollHealth::Unasked | DatahubPollHealth::Reconnecting => None,
        DatahubPollHealth::Live => Some(ThemeStatus::Ok),
        DatahubPollHealth::Stale => Some(ThemeStatus::Warning),
        DatahubPollHealth::Warming => Some(ThemeStatus::Info),
        DatahubPollHealth::Unavailable => Some(ThemeStatus::Error),
    }
}

#[cfg(test)]
mod tests {
    use super::{empty_line, footer_ink, visible_headlines};
    use crate::NewsView;
    use crate::follow::NewsFollow;
    use vike_datahub_client::tool_feeds::NewsItem;
    use vike_datahub_client::tool_feeds::poll::DatahubPollHealth;
    use vike_ui_theme::components::ThemeStatus;

    fn headline(title: &str, summary: &str) -> NewsItem {
        NewsItem {
            source: "CoinDesk".into(),
            title: title.into(),
            summary: summary.into(),
            url: String::new(),
            market: "crypto".into(),
            tags: Vec::new(),
            timestamp_ms: 0,
        }
    }

    fn btc() -> NewsFollow {
        NewsFollow { ticker: "BTC".into(), names: Vec::new() }
    }

    /// **Follow chart filters the list to the focused chart's asset** (`NewsFollow::is_named_in`);
    /// off, or with no chart to follow, every headline shows.
    #[test]
    fn follow_chart_keeps_the_headlines_that_name_the_charts_asset() {
        let items = [
            headline("BTC tops $96,000 as ETF inflows resume", ""),
            headline("Fed holds rates", "Risk assets, BTC included, rallied after the decision"),
            headline("Ethereum sets an upgrade date", ""),
            headline("WBTC supply slips", "A wrapped token, not the asset itself"),
        ];
        let titles = |view: &NewsView, followed: Option<&NewsFollow>| -> Vec<String> {
            visible_headlines(&items, view, followed).iter().map(|n| n.title.clone()).collect()
        };
        let on = NewsView::default();
        assert!(on.follow, "Follow chart starts on");
        assert_eq!(
            titles(&on, Some(&btc())),
            ["BTC tops $96,000 as ETF inflows resume", "Fed holds rates"],
            "the title or the summary, as a word"
        );
        assert_eq!(titles(&on, None).len(), 4, "no chart to follow: every headline");
        let off = NewsView { follow: false, ..NewsView::default() };
        assert_eq!(titles(&off, Some(&btc())).len(), 4, "Follow chart off: every headline");
    }

    /// **An empty list says why**: when Follow chart emptied it, the line names the asset and the
    /// way out; when the other filters did, it says so as before.
    #[test]
    fn a_list_follow_chart_emptied_names_the_asset_and_the_way_out() {
        let items = [headline("Ethereum sets an upgrade date", "")];
        let on = NewsView::default();
        assert!(visible_headlines(&items, &on, Some(&btc())).is_empty());
        assert_eq!(
            empty_line(&items, &on, Some(&btc())),
            "No headlines name BTC; turn off Follow chart to see all."
        );
        let other = NewsView { query: "no such words".into(), ..NewsView::default() };
        assert_eq!(
            empty_line(&items, &other, Some(&btc())),
            "No headlines match the current filters.",
            "the search emptied it: turning Follow chart off would show nothing either"
        );
    }

    /// Each feed state wears its own status colour, so LIVE and STALE no longer read alike; a
    /// filter's count and an unasked feed stay dim.
    #[test]
    fn the_footer_wears_the_feeds_state_and_a_filter_count_stays_dim() {
        assert_eq!(footer_ink(false, DatahubPollHealth::Live), Some(ThemeStatus::Ok));
        assert_eq!(footer_ink(false, DatahubPollHealth::Stale), Some(ThemeStatus::Warning));
        assert_eq!(footer_ink(false, DatahubPollHealth::Warming), Some(ThemeStatus::Info));
        assert_eq!(footer_ink(false, DatahubPollHealth::Unavailable), Some(ThemeStatus::Error));
        assert_eq!(footer_ink(false, DatahubPollHealth::Unasked), None);
        assert_eq!(
            footer_ink(false, DatahubPollHealth::Reconnecting),
            None,
            "a short outage is grey"
        );
        for health in
            [DatahubPollHealth::Live, DatahubPollHealth::Stale, DatahubPollHealth::Unavailable]
        {
            assert_eq!(footer_ink(true, health), None, "{health:?}: a filter count is no state");
        }
    }
}
