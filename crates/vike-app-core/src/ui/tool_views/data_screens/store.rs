//! Store — what is mounted, and the actions on it this window knows about and does not perform.

use super::{KIND_COLUMNS, caption};
use crate::ui::tool_views::ToolCtx;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table;
use vike_ui_theme::components::{Status, Tokens, chip, section};
use vike_ui_theme::fmt::fmt_count_compact;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;
use vike_util::bytes::iec_bytes;

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
pub const STORE_ACTIONS: [(&str, &str); 3] = [
    (
        "+ Mount store…",
        "Nothing in this build mounts a second store. The one this window reads is resolved once, \
         at boot, and is fixed for the life of the process — pointing it elsewhere means the \
         store-root setting (or the settings-directory override that beats the project walk), and \
         then a restart.",
    ),
    (
        "Verify",
        "Verifying means re-reading every manifest against the part files beside it. No verb in \
         this build does that, and a button that merely re-ran the file-index would report a store \
         it never checked as clean.",
    ),
    (
        "Compact",
        "Compaction rewrites the store's part files. It is a write job with no implementation here, \
         and it could not run at all against a read-only mount.",
    ),
];

/// **Store** — which stores are mounted, what each can do, and what the layout on disk holds.
pub fn store(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let t = Tokens::of(ui.ctx());
    let remote = ctx.stored.delete_unavailable.is_some();
    let tree = ctx.stored.tree;
    // An access mode is words (Inter); a count and a size stand alone (mono) — spec §3.3.
    let words = |s: &str, ink: egui::Color32| {
        egui::RichText::new(s).font(t.font(TextRole::Body)).color(ink)
    };
    let number = |s: String, ink: egui::Color32| {
        egui::RichText::new(s).font(t.mono(TextRole::Body)).color(ink)
    };

    ui.label(caption(&t, "MOUNTED STORES"));
    ui.add_space(space::SM);
    egui::Grid::new("dm_stores")
        .num_columns(4)
        .spacing([space::XL2, space::MD])
        .striped(true)
        .show(ui, |ui| {
            for h in ["STORE", "ACCESS", "SERIES", "SIZE"] {
                ui.label(caption(&t, h));
            }
            ui.end_row();

            let series: usize = tree.iter().map(|v| v.total.series).sum();
            let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
            // The CURRENT mount is the one the grid is reading, and which one that is comes off
            // `delete_unavailable` — the same field the grid greys Delete from — rather than a second
            // probe that could disagree with it.
            let (cur, other) = if remote {
                ("remote datahub", "local store")
            } else {
                ("local store", "remote datahub")
            };
            // Mounted is a STATUS: a status dot beside the store's name, never the name drawn in the
            // accent (spec §2).
            chip::status_dot(ui, Status::Ok, cur);
            let access = if remote { "read · list" } else { "read · write · list" };
            ui.label(words(access, t.theme.text2));
            ui.label(number(series.to_string(), t.theme.text2));
            ui.label(number(iec_bytes(bytes), t.theme.text2));
            ui.end_row();

            chip::status_dot(ui, Status::Muted, other);
            ui.label(words("not mounted", t.theme.text3));
            ui.label(number("—".to_string(), t.theme.text3));
            ui.label(number("—".to_string(), t.theme.text3));
            ui.end_row();
        });

    ui.add_space(space::LG);
    // The design's store actions. Every one is dead, every one says why on hover, and the sentence
    // beneath them says it once more in the open — a hover text is not discoverable, and an
    // operator scanning for "can I compact this" deserves the answer without hunting for it.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        for (label, why) in STORE_ACTIONS {
            ui.add(ActionButton::secondary(label).disabled_because(why));
        }
    });
    ui.add_space(space::XS);
    ui.label(caption(
        &t,
        "None of the three is built — mounting, verifying and compacting are still CLI-only jobs. \
         Each button names the one it is waiting for.",
    ));

    ui.add_space(space::MD);
    let warning = |s: String| {
        egui::RichText::new(s).font(t.font(TextRole::Body)).color(Status::Warning.color())
    };
    if let Some(reason) = ctx.stored.delete_unavailable {
        // The whole reason store identity is in the header. A read-only mount costs exactly two
        // things, and before this screen each announced itself separately, in its own hover text.
        let line = warning(format!("On this mount: {reason}"));
        ui.label(icons::WARNING.before(ui.style(), line));
    }
    if let Some(note) = ctx.stored.partials_note {
        let line = warning(note.to_string());
        ui.label(icons::WARNING.before(ui.style(), line));
    }

    section::strip_rule(ui);
    ui.label(caption(&t, "LAYOUT ON DISK"));
    ui.add_space(space::SM);

    // Per-KIND totals, folded from the same tree. No new walk: `kind` is already on every
    // `SeriesRow`, so this is a group-by over data the grid rendered a moment ago.
    let mut by_kind: std::collections::BTreeMap<&str, (usize, u64, u64)> =
        std::collections::BTreeMap::new();
    for v in tree {
        for s in &v.symbols {
            for r in &s.series {
                let e = by_kind.entry(r.kind.as_str()).or_default();
                e.0 += 1;
                e.1 += r.cov.rows;
                e.2 += r.cov.bytes;
            }
        }
    }
    if by_kind.is_empty() {
        state::view(ui, Load::Empty("Nothing stored."));
        return;
    }
    // The kit's data table: every cell is text. Capped in width so four columns do not stretch
    // across the whole body.
    let kinds: Vec<(&str, (usize, u64, u64))> = by_kind.into_iter().collect();
    let kinds_w = ui.available_width().min(data::KINDS_TABLE_MAX_W);
    ui.allocate_ui(egui::vec2(kinds_w, ui.available_height()), |ui| {
        table::data_table(ui, "dm_kinds", &KIND_COLUMNS, kinds.len(), None, |r, c| {
            let (kind, (n, rows, bytes)) = kinds[r];
            match c {
                0 => kind.to_string(),
                1 => n.to_string(),
                2 => fmt_count_compact(rows),
                _ => iec_bytes(bytes),
            }
        })
    });
}
