//! `vike-ui-theme` — the ONE canonical egui UI theme, extracted so every GUI crate reads the same
//! values from a single leaf crate (dep: `egui` only) instead of hand-copying them.
//!
//! Before this crate the palette lived at the TOP of the dependency stack (`vike-app/src/theme.rs`),
//! which forced hand-copied `Color32` constants down into `vike-chart`, `vike-connections`,
//! `vike-app-core` and back into `vike-app` itself — the exact drift a real shipped bug proved
//! dangerous: the News/Calendar arms once spelled the accent `(62,224,137)` instead of the canonical
//! `(62,224,138)`, a 1-bit green drift on a color that is supposed to be identical everywhere. The
//! same story played out for the byte/count formatters (`human_bytes` was a line-for-line copy of
//! `fmt_bytes`, and two thousands-groupers / K-M-B compactors coexisted). Reference these modules and
//! nothing can drift again.
//!
//! - [`palette`] — the `Color32` consts ported from vike `theme.py` (`vike-app/src/theme.rs`), plus
//!   the series blue [`palette::BLUE`]; the module goes once its last caller follows the chosen
//!   theme (spec §9). A const-equality test pins every rgb tuple. [`palette::trading`] is the
//!   SECOND, deliberately separate dark trading-terminal palette (Polymarket colour language)
//!   shared by vike-cockpit and vike-panels — previously FIVE hand-copied const blocks; its values
//!   differ from the app palette ON PURPOSE and a pin test guards the boundary.
//! - [`font`] — the font-weight `RichText` helpers (`semibold`/`bold`/`mono_semibold`), moved out
//!   of `vike-app`'s inline `font` module. Each maps to a named egui `FontFamily` that
//!   [`fonts::definitions`] binds.
//! - [`fmt`] — the number/byte formatters: [`fmt::fmt_bytes`], the thousands-groupers
//!   ([`fmt::fmt_count`] u64, [`fmt::fmt_thousands`] f64) and the K/M/B compactors
//!   ([`fmt::fmt_count_compact`] u64, [`fmt::fmt_compact`] f64), deduped into one home.
//! - [`theme`] — the design system's four dark themes (Graphite, Midnight, Dusk, Carbon), each a
//!   background with its own accent plus the neutrals derived from them. The derivation rule and
//!   the contrast floors are tests; production reads constants.
//! - [`market`] — the four market-colour sets ("up"/"down"), independent of the theme.
//! - [`status`] — the five status colours (ok, warning, error, info, muted), the same in every theme
//!   (spec §3.2); a status is never the accent.
//! - [`type_scale`] — the seven text roles and the Standard/Large scales behind them.
//! - [`metrics`] — the corner radius and the three densities' control/row/padding/gap/header sizes.
//! - [`appearance`] — the five appearance settings as one value; `install` (fonts, once), `apply`
//!   (a live change, no fonts) and `current` (what painters read). `install` replaced the desktop
//!   binary's own `install_visuals`.
//! - [`fonts`] — the bundled faces (Inter, JetBrains Mono, and the Phosphor icon face) and the one
//!   `egui::FontDefinitions` the app, its examples and its tests all install.
//! - [`header`] — the window-header background: nothing unless the header gradient is on, then the
//!   theme's gradient with the frame's rounded top corners.
//! - [`heat`] — the liquidity heatmap's ramp, the same in every theme.
//! - [`icons`] — the app's icons: one Phosphor glyph per meaning, drawn from a font family of their
//!   own.
//! - [`preview`] — painted previews of each theme and each market-colour set, in the option's own
//!   tokens, for the Settings window.
//! - [`brand`] — the Vike mark, its orange, the name and the app id, and the app icon drawn from
//!   them: the title-bar mark, the window icon, and the files `assets/brand/` holds.
//! - [`color`] — colour math: stored bytes to a colour, a colour at an alpha, a faded colour. The
//!   one place a GUI crate derives a colour.
//! - [`components`] — the component kit (spec §4): every control, built once, painted from the
//!   installed appearance.

//! - `frame_sanity` (feature `test-support`) — `frame_sanity::assert_frame_sane`, the ONE shared
//!   geometry assertion every headless egui frame test calls. It lives here for the same reason the
//!   palette does: every GUI crate is already above this crate, so one copy serves all of them, and
//!   a law spelled twice is the defect this crate exists to prevent. A default build compiles none
//!   of it.
//! - `frame_record` (feature `test-support`) — the rung ABOVE it: a canonical TEXT record of the
//!   TESSELLATED frame (paint order, clip rects, bounding boxes, colours), compared against a
//!   committed golden. `frame_sanity` says nothing is `NaN`; `frame_record` says WHERE things were
//!   drawn, in WHAT ORDER, and under WHICH CLIP — the three questions that carry a layout
//!   regression. Same crate, same feature, same reason.
//! - `pixel_liveness` (feature `test-support`) — the RASTERIZED rung: non-golden liveness
//!   floors (`pixel_liveness::assert_pixels_live`) over the RGBA readback of the two
//!   `png-export` offscreen harnesses, so a run that saved a fully-blank frame reddens instead of
//!   exiting 0. `frame_sanity` says the geometry is paintable, `frame_record` says where it was
//!   drawn — this is the only rung that sees what a rasterizer actually PAINTED, and it judges
//!   liveness only, never correctness (no goldens, nothing to rebaseline). Same crate, same
//!   feature, same reason.
//! - `harness` (feature `test-support`) — `harness::type_ready`, the ONE first-frame guard a
//!   headless `egui_kittest` harness calls before drawing an icon or a named weight: the frame
//!   kittest runs at build time has egui's default fonts, which bind neither family, and epaint
//!   panics on an unbound one. Its twin `harness::appearance_ready` installs a whole appearance
//!   for a harness that tests a surface under a theme, a density or a text size. Same crate, same
//!   feature, same reason.

pub mod appearance;
pub mod brand;
pub mod color;
pub mod components;
pub mod fmt;
pub mod font;
pub mod fonts;
pub mod header;
pub mod heat;
pub mod icons;
pub mod market;
pub mod metrics;
/// The ONE offscreen rasterizer both `png-export` harnesses drive. Behind its OWN feature
/// rather than `test-support`, which promises to pull no new dependency and this pulls four.
#[cfg(feature = "offscreen")]
pub mod offscreen;
pub mod palette;
pub mod preview;
pub mod status;
pub mod theme;
pub mod type_scale;

#[cfg(test)]
mod color_math;

#[cfg(feature = "test-support")]
pub mod frame_record;
#[cfg(feature = "test-support")]
pub mod frame_sanity;
#[cfg(feature = "test-support")]
pub mod harness;
#[cfg(feature = "test-support")]
pub mod pixel_liveness;
