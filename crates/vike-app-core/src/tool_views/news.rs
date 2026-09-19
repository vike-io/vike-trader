//! The News tool body — the filterable RSS headline list + reader pane (favicon avatars from
//! `ToolCtx::logos`) and its multi-select filter pill. Moved verbatim from `vike-app`'s `main.rs`
//! (tool-view extraction batch 1).

use super::ToolCtx;
use crate::tools;
use vike_ui_theme::{font, palette as theme};

/// The News tool body: the filterable headline list + reader pane (favicon avatars from
/// `logos`). Split out of `tool_content`'s match arm verbatim (dedup-app refactor).
pub fn news_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::RichText;
    use egui::{Align, Align2, Color32, FontId, Layout, Sense, vec2};
    const C_SURF: Color32 = theme::SURFACE;
    const C_BORD: Color32 = theme::BORDER;
    const C_TEXT: Color32 = theme::TEXT;
    const C_T2: Color32 = theme::TEXT2;
    const C_T3: Color32 = theme::TEXT3;
    const C_ACC: Color32 = theme::ACCENT; // FIX: was (62,224,137), a 1-bit drift off ACCENT
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
        Color32::from_rgb(63, 224, 138),
        Color32::from_rgb(240, 169, 63),
        Color32::from_rgb(63, 155, 224),
        Color32::from_rgb(224, 100, 63),
        Color32::from_rgb(176, 111, 224),
        Color32::from_rgb(63, 224, 200),
        Color32::from_rgb(224, 63, 138),
        Color32::from_rgb(155, 224, 63),
    ];
    let avatar = |ui: &mut egui::Ui, sz: f32, src: &str| {
        let (ar, _) = ui.allocate_exact_size(vec2(sz, sz), Sense::hover());
        if let Some(tex) = logos.get(src) {
            // real favicon on a rounded white backing (Py news_logos: rounded badge)
            ui.painter().rect_filled(ar, sz * 0.28, Color32::from_gray(244));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            ui.painter().image(tex.id(), ar.shrink(3.0), uv, Color32::WHITE);
        } else {
            let col = AV[src.bytes().map(|b| b as usize).sum::<usize>() % 8];
            ui.painter().rect_filled(ar, sz * 0.28, col);
            ui.painter().text(
                ar.center(),
                Align2::CENTER_CENTER,
                src.chars().next().unwrap_or('•').to_uppercase().to_string(),
                FontId::proportional(sz * 0.5),
                theme::BG, // BG, matching Py avatar fg
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
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.spacing_mut().button_padding = vec2(14.0, 8.0); // vike toolbar controls are roomier
        news_filter_pill(ui, "Market", &market_opts, &mut tv.news_markets);
        news_filter_pill(ui, "Category", &category_opts, &mut tv.news_categories);
        news_filter_pill(ui, "Provider", &provider_opts, &mut tv.news_providers);
        let follow = ui.add(
            egui::Button::new(
                RichText::new("⌖ Follow chart")
                    .color(if tv.news_follow { C_ACC } else { C_T2 })
                    .size(13.0),
            )
            .fill(C_SURF)
            .stroke(egui::Stroke::new(1.0, if tv.news_follow { C_ACC } else { C_BORD }))
            .min_size(vec2(0.0, 34.0)),
        );
        if follow.clicked() {
            tv.news_follow = !tv.news_follow;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = ui.button("↻");
            ui.add(
                egui::TextEdit::singleline(&mut tv.news_query)
                    .hint_text("Search headlines…")
                    .desired_width(220.0),
            );
        });
    });
    ui.add_space(6.0);

    let full = ui.available_width();
    let reader_on = tv.news_reader_open && !visible.is_empty();
    let list_w =
        if reader_on { (full * 0.66).clamp(240.0, (full - 170.0).max(240.0)) } else { full };
    let reader_w = (full - list_w - 22.0).max(150.0);
    let split_h = (ui.available_height() - 24.0).max(120.0);
    ui.horizontal_top(|ui| {
        // ----- headline list -----
        ui.allocate_ui_with_layout(vec2(list_w, split_h), Layout::top_down(Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("news_list").show(ui, |ui| {
                if td.news.is_empty() {
                    ui.weak("loading…");
                } else if visible.is_empty() {
                    ui.add_space(8.0);
                    ui.weak("No headlines match the current filters.");
                }
                for (vi, n) in visible.iter().enumerate() {
                    let selected = vi == tv.news_sel;
                    let inner = egui::Frame::new()
                        .fill(if selected { C_SURF } else { Color32::TRANSPARENT })
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 7))
                        .show(ui, |ui| {
                            ui.set_width(list_w - 18.0);
                            ui.horizontal_top(|ui| {
                                avatar(ui, 30.0, &n.source);
                                ui.add_space(9.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        RichText::new(format!("{}  ·  {}", ago(n.ts_ms), n.source))
                                            .size(12.0)
                                            .color(C_T3),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&n.title).size(15.0).color(C_TEXT),
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
                    ui.add(egui::Separator::default().spacing(4.0));
                }
            });
        });
        if reader_on {
            ui.add_space(7.0);
            let (sr, _) = ui.allocate_exact_size(vec2(1.0, split_h), Sense::hover());
            ui.painter().rect_filled(sr, 0.0, C_BORD);
            ui.add_space(9.0);
            // ----- reader pane -----
            ui.allocate_ui_with_layout(
                vec2(reader_w, split_h),
                Layout::top_down(Align::Min),
                |ui| {
                    if let Some(n) = visible.get(tv.news_sel) {
                        ui.horizontal(|ui| {
                            avatar(ui, 30.0, &n.source);
                            ui.add_space(8.0);
                            ui.label(font::semibold(&n.source).size(13.0).color(C_TEXT));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("✕").clicked() {
                                    tv.news_reader_open = false;
                                }
                            });
                        });
                        ui.add_space(8.0);
                        ui.add(
                            egui::Label::new(font::bold(&n.title).size(26.0).color(C_TEXT)).wrap(),
                        );
                        ui.add_space(5.0);
                        let date = chrono::DateTime::from_timestamp_millis(n.ts_ms)
                            .map(|d| {
                                d.with_timezone(&chrono::Local)
                                    .format("%b %d, %Y · %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!("{}  ·  {}", date, ago(n.ts_ms)))
                                .size(13.0)
                                .color(C_T2),
                        );
                        ui.add_space(10.0);
                        ui.add(
                            egui::Label::new(RichText::new(&n.summary).size(16.0).color(C_T2))
                                .wrap(),
                        );
                        ui.add_space(12.0);
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
                                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                                for c in &chips {
                                    egui::Frame::new()
                                        .fill(C_SURF)
                                        .corner_radius(4.0)
                                        .inner_margin(egui::Margin::symmetric(10, 3))
                                        .show(ui, |ui| {
                                            ui.label(RichText::new(c).size(12.0).color(C_TEXT));
                                        });
                                }
                            });
                            ui.add_space(12.0);
                        }
                        let has_url = !n.url.is_empty();
                        let open = ui.add_enabled(
                            has_url,
                            egui::Button::new(
                                font::semibold("↗  Open original").size(14.0).color(C_TEXT),
                            ),
                        );
                        if open.clicked() {
                            tv.news_open_url = Some(n.url.clone());
                        }
                    }
                },
            );
        }
    });
    ui.add_space(3.0);
    let total = td.news.len();
    let status = if visible.len() == total {
        format!("{total} headlines · LIVE")
    } else {
        format!("{} of {} headlines", visible.len(), total)
    };
    ui.label(RichText::new(status).size(11.0).color(C_T3));
}

/// A multi-select filter pill (News toolbar): button labelled `Name (n) ▾` opening a checklist
/// popover that stays open while toggling (CloseOnClickOutside). Mutates the selected set.
fn news_filter_pill(
    ui: &mut egui::Ui,
    name: &str,
    opts: &[String],
    sel: &mut std::collections::HashSet<String>,
) {
    use egui::{Color32, RichText};
    const SURFACE: Color32 = theme::SURFACE;
    const BORD: Color32 = theme::BORDER;
    const TEXT2: Color32 = theme::TEXT2;
    let n = sel.len();
    let label = if n > 0 { format!("{name} ({n})") } else { name.to_string() };
    let resp = ui.add(
        egui::Button::new(RichText::new(label).color(TEXT2).size(13.0))
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORD))
            .min_size(egui::vec2(0.0, 34.0)),
    );
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(
        |ui| {
            ui.set_min_width(170.0);
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
