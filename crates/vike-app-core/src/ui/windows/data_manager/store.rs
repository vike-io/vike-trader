//! Store — what is mounted, and the actions on it this window knows about and does not perform.

use super::caption;
use crate::ui::windows::WindowToolContext;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::table;
use vike_ui_theme::components::table::ThemeTableColumn;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip, section};
use vike_ui_theme::fmt::format_count_compact;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;
use vike_utility::bytes::iec_bytes;

/// The Store screen's actions, each carrying the reason it is DARK.
///
/// ⚠ They are disabled rather than absent, and that is the design's call rather than an oversight
/// waiting to be tidied. An absent button teaches nothing; a dim one with its reason on hover tells
/// an operator that mounting, verifying and compacting are jobs this window knows about and does
/// not perform — which is the answer they came for. The one thing that may NEVER happen here is a
/// live button: see this module's doc for what the ten of those in the old toolbar cost.
///
/// ⚠ The plus sign is ASCII `+`, not the design's fullwidth one: no face the app bundles draws the
/// fullwidth forms block. A symbol last assumed present shipped as blank squares — window controls
/// on Linux, first reported from a screenshot — and `crates/vike-ops/tests/gui/ui_glyph_coverage.rs`
/// now fails any character the GUI prints that no bundled face draws.
pub(crate) const STORE_ACTIONS: [(&str, &str); 3] = [
    (
        "+ Mount store…",
        "This window reads one store: the one of the datahub it is connected to. Connect to \
         another backend to read another store.",
    ),
    (
        "Verify",
        "Checking every stored file against the store's index is not available in this window.",
    ),
    (
        "Compact",
        "Compaction rewrites the store's files on the server, where the datahub's own maintenance \
         runs it; this window cannot start it.",
    ),
];

/// **Store** — which stores are mounted, what each can do, and what the layout on disk holds.
pub(super) fn store(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    let t = ThemeTokens::of(ui.ctx());
    let remote = context.execution_history.datahub_address.is_some();
    let tree = context.stored.tree;
    // An access mode is words (Inter); a count and a size stand alone (mono) — spec §3.3.
    let words = |s: &str, ink: egui::Color32| {
        egui::RichText::new(s).font(t.font(ThemeTextRole::Body)).color(ink)
    };
    let number = |s: String, ink: egui::Color32| {
        egui::RichText::new(s).font(t.mono(ThemeTextRole::Body)).color(ink)
    };

    ui.label(caption(&t, "MOUNTED STORES"));
    ui.add_space(space::SM);
    egui::Grid::new("data_manager_stores")
        .num_columns(4)
        .spacing([space::XL2, space::MD])
        .striped(true)
        .show(ui, |ui| {
            for h in ["STORE", "ACCESS", "SERIES", "SIZE"] {
                ui.label(caption(&t, h));
            }
            ui.end_row();

            // ONE store can be mounted, the resolved datahub: this build has no local store engine.
            // Whether it is mounted comes off the resolved datahub address — the value `stored_mode`
            // decides the grid from — rather than a second probe that could disagree with it. Mounted
            // is a STATUS: a status dot beside the store's name, never the name drawn in the accent
            // (spec §2).
            if remote {
                let series: usize = tree.iter().map(|v| v.total.series).sum();
                let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
                chip::status_dot(ui, ThemeStatus::Ok, "remote datahub");
                ui.label(words("read · list", t.theme.text_secondary));
                ui.label(number(series.to_string(), t.theme.text_secondary));
                ui.label(number(iec_bytes(bytes), t.theme.text_secondary));
            } else {
                chip::status_dot(ui, ThemeStatus::Muted, "remote datahub");
                ui.label(words("not mounted", t.theme.text_tertiary));
                ui.label(number("—".to_string(), t.theme.text_tertiary));
                ui.label(number("—".to_string(), t.theme.text_tertiary));
            }
            ui.end_row();
        });

    ui.add_space(space::LG);
    // The design's store actions. Every one is dead, every one says why on hover, and the sentence
    // beneath them says it once more in the open — a hover text is not discoverable, and an
    // operator scanning for "can I compact this" deserves the answer without hunting for it.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        for (label, why) in STORE_ACTIONS {
            ui.add(ThemeButtonAction::secondary(label).disabled_because(why));
        }
    });
    ui.add_space(space::XS);
    ui.label(caption(
        &t,
        "Mounting, verifying and compacting are not available in this window; hover a button for \
         why.",
    ));

    ui.add_space(space::MD);
    let warning = |s: String| {
        egui::RichText::new(s).font(t.font(ThemeTextRole::Body)).color(ThemeStatus::Warning.color())
    };
    if let Some(reason) = context.stored.delete_unavailable {
        // The whole reason store identity is in the header. A read-only mount costs exactly two
        // things, and before this screen each announced itself separately, in its own hover text.
        let line = warning(format!("On this mount: {reason}"));
        ui.label(icons::WARNING.before(ui.style(), line));
    }
    if let Some(note) = context.stored.partials_note {
        let line = warning(note.to_string());
        ui.label(icons::WARNING.before(ui.style(), line));
    }

    section::strip_rule(ui);
    ui.label(caption(&t, "LAYOUT ON DISK"));
    ui.add_space(space::SM);

    // Per-KIND totals, folded from the same tree. No new walk: `kind` is already on every
    // `DataManagerInventorySeries`, so this is a group-by over data the grid rendered a moment ago.
    let mut by_kind: std::collections::BTreeMap<&str, (usize, u64, u64)> =
        std::collections::BTreeMap::new();
    for v in tree {
        for s in &v.symbols {
            for r in &s.series {
                let e = by_kind.entry(r.kind.word()).or_default();
                e.0 += 1;
                e.1 += r.coverage.rows;
                e.2 += r.coverage.bytes;
            }
        }
    }
    if by_kind.is_empty() {
        state::view(ui, ThemeEmptyPane::Empty("Nothing stored."));
        return;
    }
    // The kit's data table: every cell is text. Capped in width so four columns do not stretch
    // across the whole body.
    let kinds: Vec<(&str, (usize, u64, u64))> = by_kind.into_iter().collect();
    let kinds_w = ui.available_width().min(data::KINDS_TABLE_MAX_WIDTH);
    ui.allocate_ui(egui::vec2(kinds_w, ui.available_height()), |ui| {
        table::data_table(ui, "data_manager_kinds", &KIND_COLUMNS, kinds.len(), None, |r, c| {
            let (kind, (n, rows, bytes)) = kinds[r];
            match c {
                0 => kind.to_string(),
                1 => n.to_string(),
                2 => format_count_compact(rows),
                _ => iec_bytes(bytes),
            }
        })
    });
}

/// The Store screen's layout-by-kind table. Every cell is text, so it is the kit's data table.
const KIND_COLUMNS: [ThemeTableColumn<'static>; 4] = [
    ThemeTableColumn {
        title: "KIND",
        weight: data::STORE_KIND_KIND_WEIGHT,
        min_w: data::STORE_KIND_KIND_MIN_WIDTH,
        numeric: false,
    },
    ThemeTableColumn {
        title: "SERIES",
        weight: data::STORE_KIND_SERIES_WEIGHT,
        min_w: data::STORE_KIND_SERIES_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "ROWS",
        weight: data::STORE_KIND_ROWS_WEIGHT,
        min_w: data::STORE_KIND_ROWS_MIN_WIDTH,
        numeric: true,
    },
    ThemeTableColumn {
        title: "SIZE",
        weight: data::STORE_KIND_SIZE_WEIGHT,
        min_w: data::STORE_KIND_SIZE_MIN_WIDTH,
        numeric: true,
    },
];
