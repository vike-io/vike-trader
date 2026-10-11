//! The News tool body — the filterable RSS headline list + reader pane (favicon avatars from
//! `ToolCtx::logos`) and its multi-select filter pill. Moved verbatim from `vike-app`'s `main.rs`
//! (tool-view extraction batch 1).

use super::ToolCtx;
use crate::tools;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::news;
use vike_ui_theme::{font, icons};

/// The News tool body: the filterable headline list + reader pane (favicon avatars from
/// `logos`). Split out of `tool_content`'s match arm verbatim (dedup-app refactor).
pub fn news_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::RichText;
    use egui::{Align, Align2, Color32, FontId, Layout, Sense, vec2};
    let tok = Tokens::of(ui.ctx());
    let (td, logos) = (ctx.td, ctx.logos);
    let now = chrono::Utc::now().timestamp_millis();
    let ago = |ts: i64| -> String {
        if ts == 0 {
            return "—".into();
        }
        let s = ((now - ts) / 1000).max(0);
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
    const AV: [Color32; 8] = [
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
            ui.painter().rect_filled(ar, sz * news::AVATAR_RADIUS_FRAC, news::LOGO_PLATE);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(tex.id(), ar.shrink(space::SM), uv, vike_ui_theme::color::UNTINTED);
        } else {
            let col = AV[src.bytes().map(|b| b as usize).sum::<usize>() % 8];
            ui.painter().rect_filled(ar, sz * news::AVATAR_RADIUS_FRAC, col);
            ui.painter().text(
                ar.center(),
                Align2::CENTER_CENTER,
                src.chars().next().unwrap_or('•').to_uppercase().to_string(),
                FontId::proportional(sz * news::AVATAR_LETTER_FRAC),
                tok.theme.bg, // BG, matching Py avatar fg
            );
        }
    };

    // ---- filter (pure, over the already-fetched headlines) ----
    let q = tv.news_query.to_lowercase();
    let visible: Vec<&tools::NewsItem> = td
        .news
        .iter()
        .filter(|n| {
            (tv.news_markets.is_empty()
                || tv.news_markets.iter().any(|m| m.eq_ignore_ascii_case(&n.market)))
                && (tv.news_providers.is_empty() || tv.news_providers.contains(&n.source))
                && (tv.news_categories.is_empty()
                    || tv
                        .news_categories
                        .iter()
                        .any(|c| c == tools::classify_news(&n.title, &n.tags)))
                && (q.is_empty()
                    || n.title.to_lowercase().contains(&q)
                    || n.summary.to_lowercase().contains(&q))
        })
        .collect();
    if tv.news_sel >= visible.len() {
        tv.news_sel = 0;
    }
    let market_opts: Vec<String> = tools::NEWS_MARKETS.iter().map(|s| s.to_string()).collect();
    let category_opts: Vec<String> = tools::NEWS_CATEGORIES.iter().map(|s| s.to_string()).collect();
    let provider_opts: Vec<String> = {
        let mut v: std::collections::HashSet<String> =
            td.news.iter().map(|n| n.source.clone()).collect();
        let mut v: Vec<String> = v.drain().collect();
        v.sort();
        v
    };

    // ---- filter toolbar: Market · Category · Provider · ⌖Follow · ……  Search · ↻ ----
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::LG;
        ui.spacing_mut().button_padding = vec2(space::XL2, space::LG); // vike toolbar controls are roomier
        news_filter_pill(ui, "Market", &market_opts, &mut tv.news_markets);
        news_filter_pill(ui, "Category", &category_opts, &mut tv.news_categories);
        news_filter_pill(ui, "Provider", &provider_opts, &mut tv.news_providers);
        // One size for the icon and its words.
        const FOLLOW_TEXT: TextRole = TextRole::Title;
        // ON is shown by the accent OUTLINE below (a shape); the words keep the text ink.
        let follow_ink = if tv.news_follow { tok.theme.text } else { tok.theme.text2 };
        let follow = ui.add(
            egui::Button::new((
                icons::FOLLOW_CHART.rich().color(follow_ink).size(role_px(ui.ctx(), FOLLOW_TEXT)),
                RichText::new("Follow chart")
                    .color(follow_ink)
                    .size(role_px(ui.ctx(), FOLLOW_TEXT)),
            ))
            .fill(tok.theme.surface)
            .stroke(egui::Stroke::new(
                stroke::HAIRLINE,
                if tv.news_follow { tok.theme.accent } else { tok.theme.border },
            ))
            .min_size(vec2(0.0, news::TOOLBAR_BTN_H)),
        );
        if follow.clicked() {
            tv.news_follow = !tv.news_follow;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = icons::named(ui.button(icons::REFRESH), "Refresh");
            ui.add(
                egui::TextEdit::singleline(&mut tv.news_query)
                    .hint_text("Search headlines…")
                    .desired_width(news::SEARCH_W),
            );
        });
    });
    ui.add_space(space::MD);

    let full = ui.available_width();
    let reader_on = tv.news_reader_open && !visible.is_empty();
    let list_w = if reader_on {
        (full * news::LIST_FRAC)
            .clamp(news::LIST_MIN_W, (full - news::LIST_LEAVES_READER_W).max(news::LIST_MIN_W))
    } else {
        full
    };
    let reader_w = (full - list_w - news::READER_WIDTH_RESERVE).max(news::READER_MIN_W);
    let split_h = (ui.available_height() - space::XL3).max(news::SPLIT_MIN_H);
    ui.horizontal_top(|ui| {
        // ----- headline list -----
        ui.allocate_ui_with_layout(vec2(list_w, split_h), Layout::top_down(Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("news_list").show(ui, |ui| {
                if td.news.is_empty() {
                    ui.weak("loading…");
                } else if visible.is_empty() {
                    ui.add_space(space::LG);
                    ui.weak("No headlines match the current filters.");
                }
                for (vi, n) in visible.iter().enumerate() {
                    let selected = vi == tv.news_sel;
                    let inner = egui::Frame::new()
                        .fill(if selected { tok.theme.surface } else { Color32::TRANSPARENT })
                        .corner_radius(egui::CornerRadius::same(RADIUS))
                        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::LG as i8))
                        .show(ui, |ui| {
                            ui.set_width(list_w - news::CARD_INNER_RESERVE);
                            ui.horizontal_top(|ui| {
                                avatar(ui, news::AVATAR_SIZE, &n.source);
                                ui.add_space(space::LG);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(format!("{}  ·  {}", ago(n.ts_ms), n.source))
                                            .size(role_px(ui.ctx(), TextRole::Strong))
                                            .color(tok.theme.text3),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&n.title)
                                                .size(role_px(ui.ctx(), TextRole::Title))
                                                .color(tok.theme.text),
                                        )
                                        .wrap(),
                                    );
                                });
                            });
                        });
                    let resp = ui.interact(
                        inner.response.rect,
                        egui::Id::new(("news_row", vi)),
                        Sense::click(),
                    );
                    if resp.clicked() {
                        tv.news_sel = vi;
                        tv.news_reader_open = true;
                    }
                    ui.add(egui::Separator::default().spacing(space::SM));
                }
            });
        });
        if reader_on {
            ui.add_space(space::LG);
            let (sr, _) = ui.allocate_exact_size(vec2(stroke::HAIRLINE, split_h), Sense::hover());
            ui.painter().rect_filled(sr, 0.0, tok.theme.border);
            ui.add_space(space::LG);
            // ----- reader pane -----
            ui.allocate_ui_with_layout(
                vec2(reader_w, split_h),
                Layout::top_down(Align::Min),
                |ui| {
                    if let Some(n) = visible.get(tv.news_sel) {
                        ui.horizontal(|ui| {
                            avatar(ui, news::AVATAR_SIZE, &n.source);
                            ui.add_space(space::LG);
                            ui.label(
                                RichText::new(&n.source)
                                    .size(role_px(ui.ctx(), TextRole::Title))
                                    .color(tok.theme.text),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icons::named(ui.button(icons::CLOSE), "Close").clicked() {
                                    tv.news_reader_open = false;
                                }
                            });
                        });
                        ui.add_space(space::LG);
                        ui.add(
                            egui::Label::new(
                                font::semibold(&n.title)
                                    .size(role_px(ui.ctx(), TextRole::Display))
                                    .color(tok.theme.text),
                            )
                            .wrap(),
                        );
                        ui.add_space(space::MD);
                        let date = chrono::DateTime::from_timestamp_millis(n.ts_ms)
                            .map(|d| {
                                d.with_timezone(&chrono::Local)
                                    .format("%b %d, %Y · %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!("{}  ·  {}", date, ago(n.ts_ms)))
                                .size(role_px(ui.ctx(), TextRole::Title))
                                .color(tok.theme.text2),
                        );
                        ui.add_space(space::XL);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&n.summary)
                                    .size(role_px(ui.ctx(), TextRole::Title))
                                    .color(tok.theme.text2),
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
                        push(tools::classify_news(&n.title, &n.tags).to_string(), &mut chips);
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
                                        .fill(tok.theme.surface)
                                        .corner_radius(egui::CornerRadius::same(RADIUS))
                                        .inner_margin(egui::Margin::symmetric(
                                            space::XL as i8,
                                            space::SM as i8,
                                        ))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(c)
                                                    .size(role_px(ui.ctx(), TextRole::Strong))
                                                    .color(tok.theme.text),
                                            );
                                        });
                                }
                            });
                            ui.add_space(space::XL);
                        }
                        let has_url = !n.url.is_empty();
                        // One size for the icon and its words.
                        const OPEN_TEXT: TextRole = TextRole::Title;
                        let open = ui.add_enabled(
                            has_url,
                            egui::Button::new((
                                icons::EXTERNAL
                                    .rich()
                                    .size(role_px(ui.ctx(), OPEN_TEXT))
                                    .color(tok.theme.text),
                                RichText::new("Open original")
                                    .size(role_px(ui.ctx(), OPEN_TEXT))
                                    .color(tok.theme.text),
                            )),
                        );
                        if open.clicked() {
                            tv.news_open_url = Some(n.url.clone());
                        }
                    }
                },
            );
        }
    });
    ui.add_space(space::SM);
    let total = td.news.len();
    let status = if visible.len() == total {
        format!("{total} headlines · LIVE")
    } else {
        format!("{} of {} headlines", visible.len(), total)
    };
    ui.label(RichText::new(status).size(role_px(ui.ctx(), TextRole::Body)).color(tok.theme.text3));
}

/// A multi-select filter pill (News toolbar): button labelled `Name (n) ▾` opening a checklist
/// popover that stays open while toggling (CloseOnClickOutside). Mutates the selected set.
fn news_filter_pill(
    ui: &mut egui::Ui,
    name: &str,
    opts: &[String],
    sel: &mut std::collections::HashSet<String>,
) {
    use egui::RichText;
    let t = Tokens::of(ui.ctx());
    let n = sel.len();
    let label = if n > 0 { format!("{name} ({n})") } else { name.to_string() };
    let resp = ui.add(
        egui::Button::new(
            RichText::new(label).color(t.theme.text2).size(role_px(ui.ctx(), TextRole::Title)),
        )
        .fill(t.theme.surface)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .min_size(egui::vec2(0.0, news::TOOLBAR_BTN_H)),
    );
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(
        |ui| {
            ui.set_min_width(news::FILTER_MENU_MIN_W);
            for o in opts {
                let mut on = sel.contains(o);
                if ui.checkbox(&mut on, o).changed() {
                    if on {
                        sel.insert(o.clone());
                    } else {
                        sel.remove(o);
                    }
                }
            }
        },
    );
}
