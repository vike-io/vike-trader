use super::layouts::{LAST_LAYOUT_FILE, LAYOUTS_SUBDIR, last_layout_path, list_layouts_in};
use super::store::{
    FAMILY_MEMBERS, WORKSPACE_FILE, load_from, relocation_notice, save_path, save_to, state_dir,
};
use super::*;
use egui::{Rect, pos2, vec2};
use std::assert_matches;
use vike_chart::panes::MoveTarget;

fn chart_win(sym: &str, style: ChartStyle) -> WinState {
    let r = Rect::from_min_size(pos2(40.0, 30.0), vec2(700.0, 440.0));
    let mut w = WinState::new(&format!("t-{sym}"), sym, "1m", WinKind::Chart, r);
    w.style = style;
    w.options.show_volume = false;
    w.follow.on = false;
    w.add_indicator("sma", &[]);
    w.add_indicator("macd", &[]);
    if let Some(a) = w.indicators.last_mut() {
        a.visible = false;
    }
    w
}

#[path = "persist_tests/panes.rs"]
#[cfg(test)]
mod panes;
#[path = "persist_tests/snapshot.rs"]
#[cfg(test)]
mod snapshot;
#[path = "persist_tests/state_files.rs"]
#[cfg(test)]
mod state_files;
#[path = "persist_tests/user_studies.rs"]
#[cfg(test)]
mod user_studies;
