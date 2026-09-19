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
//!   [`palette::BLUE`] (previously referenced only in a comment in `vike-chart/src/options_chain.rs`).
//!   A const-equality test pins every rgb tuple. [`palette::trading`] is the SECOND, deliberately
//!   separate dark trading-terminal palette (Polymarket colour language, plus the `dim` alpha
//!   helper) shared by vike-cockpit and vike-panels — previously FIVE hand-copied const blocks;
//!   its values differ from the app palette ON PURPOSE and a pin test guards the boundary.
//! - [`font`] — the font-weight `RichText` helpers (`extralight`/`semibold`/`bold`), moved out of
//!   `vike-app`'s inline `font` module. Each maps to a named egui `FontFamily` a binary registers.
//! - [`fmt`] — the number/byte formatters: [`fmt::fmt_bytes`], the thousands-groupers
//!   ([`fmt::fmt_count`] u64, [`fmt::fmt_thousands`] f64) and the K/M/B compactors
//!   ([`fmt::fmt_count_compact`] u64, [`fmt::fmt_compact`] f64), deduped into one home.
//!
//! `vike-studio` deliberately KEEPS its own local brand `ACCENT` (a test-pinned divergence) and does
//! NOT depend on this crate.

//! - [`frame_sanity`] (feature `test-support`) — [`frame_sanity::assert_frame_sane`], the ONE shared
//!   geometry assertion every headless egui frame test calls. It lives here for the same reason the
//!   palette does: every GUI crate is already above this crate, so one copy serves all of them, and
//!   a law spelled twice is the defect this crate exists to prevent. A default build compiles none
//!   of it.
//! - [`frame_record`] (feature `test-support`) — the rung ABOVE it: a canonical TEXT record of the
//!   TESSELLATED frame (paint order, clip rects, bounding boxes, colours), compared against a
//!   committed golden. `frame_sanity` says nothing is `NaN`; `frame_record` says WHERE things were
//!   drawn, in WHAT ORDER, and under WHICH CLIP — the three questions that carry a layout
//!   regression. Same crate, same feature, same reason.
//! - [`pixel_liveness`] (feature `test-support`) — the RASTERIZED rung: non-golden liveness
//!   floors ([`pixel_liveness::assert_pixels_live`]) over the RGBA readback of the two
//!   `png-export` offscreen harnesses, so a run that saved a fully-blank frame reddens instead of
//!   exiting 0. `frame_sanity` says the geometry is paintable, `frame_record` says where it was
//!   drawn — this is the only rung that sees what a rasterizer actually PAINTED, and it judges
//!   liveness only, never correctness (no goldens, nothing to rebaseline). Same crate, same
//!   feature, same reason.

pub mod fmt;
pub mod font;
pub mod palette;

#[cfg(feature = "test-support")]
pub mod frame_record;
#[cfg(feature = "test-support")]
pub mod frame_sanity;
#[cfg(feature = "test-support")]
pub mod pixel_liveness;
