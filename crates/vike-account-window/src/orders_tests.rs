use super::*;
use crate::test_support::{mount, venue_block};
use vike_execution::{CoreSnapshot, VenueBlock};

fn order_view(
    order_client_id: &str,
    side: i32,
    delay: Option<vike_model::StreamDelay>,
) -> vike_execution::OrderView {
    vike_execution::OrderView {
        order_client_id: order_client_id.into(),
        venue: "ibkr".into(),
        account: None,
        symbol: "AAPL.SMART.USD".into(),
        side,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
        data_delay: delay,
    }
}

#[test]
fn only_a_delayed_or_frozen_order_gets_a_badge() {
    use vike_model::StreamDelay;
    assert_eq!(delay_badge(None), None);
    assert_eq!(delay_badge(Some(StreamDelay::RealTime)), None);
    assert_eq!(delay_badge(Some(StreamDelay::DELAYED)), Some("DELAYED"));
    let stated = StreamDelay::Delayed { lag_ms: Some(900_000) };
    assert_eq!(delay_badge(Some(stated)), Some("DELAYED"));
    assert_eq!(delay_badge(Some(StreamDelay::Frozen)), Some("FROZEN"));
}

/// The order-row LAYOUT (decision 0140), read off a real frame: the delayed order's row carries
/// ONE `DELAYED` badge, on its own row (nearer that row than the other), right of its status;
/// the real-time order's row carries none.
#[test]
fn the_delay_badge_sits_on_the_delayed_orders_row_right_of_its_status() {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(
        &context,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let mut snapshot = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    snapshot.orders = vec![
        std::sync::Arc::new(order_view("d", 1, Some(vike_model::StreamDelay::DELAYED))),
        std::sync::Arc::new(order_view("r", -1, Some(vike_model::StreamDelay::RealTime))),
    ];
    let mut cancel = None;
    let out = context
        .run_ui(egui::RawInput::default(), |ui| orders_section(ui, &snapshot, &[], &mut cancel));
    let texts: Vec<(String, egui::Rect)> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => {
                Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())))
            }
            _ => None,
        })
        .collect();
    out.drop_without_applying_deltas();
    let find = |w: &str| -> Vec<egui::Rect> {
        texts.iter().filter(|(s, _)| s == w).map(|(_, r)| *r).collect()
    };
    let badges = find("DELAYED");
    assert_eq!(badges.len(), 1, "one badge, for the one delayed order: {texts:?}");
    let (buy, sell) = (find("BUY")[0], find("SELL")[0]);
    assert!(buy.center().y < sell.center().y, "the delayed order is the first row");
    let dy = |a: egui::Rect, b: egui::Rect| (a.center().y - b.center().y).abs();
    let statuses = find("ACCEPTED");
    assert_eq!(statuses.len(), 2, "{texts:?}");
    let buy_status = statuses
        .iter()
        .copied()
        .min_by(|a, b| dy(buy, *a).total_cmp(&dy(buy, *b)))
        .expect("the delayed row's status");
    assert!(dy(buy, badges[0]) < dy(sell, badges[0]), "the badge is on the delayed row");
    assert!(badges[0].left() >= buy_status.right(), "right of the status");
}

/// A v2 id's friendly words (decision 0270) are named from the frame: the account's label off
/// the block with its code (`(no label)` for the default account), the strategy's name off the
/// mount with its code. A v1 or a foreign id has none.
#[test]
fn a_v2_order_ids_words_name_the_account_and_the_strategy() {
    use vike_model::orders::client_id::OrderClientIdAccountCode;
    let mut snapshot = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    let demo = vike_model::accounts::account_keys::AccountLabel::parse("DEMO").ok();
    let code = |id| Some(OrderClientIdAccountCode::of_id(id));
    snapshot.portfolio.venues = vec![
        VenueBlock { account: demo, account_code: code(7), ..venue_block("ibkr", Vec::new()) },
        VenueBlock { account_code: code(4), ..venue_block("bybit", Vec::new()) },
    ];
    let mut maker = mount("bybit", Some("demo"));
    maker.order_client_id_code = Some("00A3".into());
    let mounts = [maker];
    assert_eq!(
        order_client_id_words("S00A307k3x9q2m7d1p4CSSLR2", &snapshot, &mounts).as_deref(),
        Some(
            "strategy (auto) · strategy 00A3 spread_maker · account #7 DEMO · closes short \
             (cannot open) · stop of group k3x9q2m7d1p4 · replacement 2"
        )
    );
    assert_eq!(
        order_client_id_words("G000004k3x9q2m7d1p4OL", &snapshot, &[]).as_deref(),
        Some("GUI (by hand) · no strategy · account #4 (no label) · opens or adds long")
    );
    assert_eq!(
        order_client_id_words("G00000Bk3x9q2m7d1p4FL", &snapshot, &[]).as_deref(),
        Some(
            "GUI (by hand) · no strategy · account #11 · closes long with a plain order \
             (may open a short)"
        ),
        "an account this frame has no block for is its number alone"
    );
    for alone in ["deadbeef17", "uicafef00d3", "ui-3-0", "EXT-1", "my hedge"] {
        assert_eq!(order_client_id_words(alone, &snapshot, &mounts), None, "{alone}");
    }
}

/// The words sit on the order's row, right of its id, and a v1 id's row carries none.
#[test]
fn the_words_sit_right_of_a_v2_id_and_a_v1_id_shows_alone() {
    let context = egui::Context::default();
    vike_ui_theme::appearance::install(
        &context,
        &vike_ui_theme::appearance::ThemeAppearance::default(),
    );
    let mut snapshot = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    let g = "G000004k3x9q2m7d1p4OL";
    snapshot.orders = vec![
        std::sync::Arc::new(order_view(g, 1, None)),
        std::sync::Arc::new(order_view("deadbeef17", -1, None)),
    ];
    let mut cancel = None;
    let out = context
        .run_ui(egui::RawInput::default(), |ui| orders_section(ui, &snapshot, &[], &mut cancel));
    let texts: Vec<(String, egui::Rect)> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => {
                Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2())))
            }
            _ => None,
        })
        .collect();
    out.drop_without_applying_deltas();
    let at = |w: &str| texts.iter().find(|(s, _)| s == w).map(|(_, r)| *r);
    let words = "GUI (by hand) · no strategy · account #4 · opens or adds long";
    let (id, said) = (at(g).expect("the v2 id"), at(words).expect("its words"));
    assert!(said.left() >= id.right() && (said.center().y - id.center().y).abs() < 1.0);
    assert!(at("deadbeef17").is_some(), "{texts:?}");
    assert_eq!(texts.iter().filter(|(s, _)| s.contains(" · ")).count(), 1, "{texts:?}");
}
