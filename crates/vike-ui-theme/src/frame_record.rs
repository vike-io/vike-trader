//! [`record_frame`] — a canonical TEXT record of one TESSELLATED egui frame, compared against a
//! committed golden by [`assert_frame_golden`].
//!
//! WHY THIS EXISTS — and why it is a rung ABOVE [`crate::frame_sanity`] rather than a replacement.
//! `assert_frame_sane` answers exactly one question: "is anything `NaN`, infinite or absurd?" It
//! cannot answer **where** a thing was drawn, **in what order**, or **under what clip** — the three
//! properties that carry every layout regression a headless test could plausibly catch. A pane that
//! collapses to a quarter of its height, an overlay that sinks behind the candles it is supposed to
//! sit on, a colour token swapped at one paint site, a clip rect narrowed until a label is silently
//! cut: all four emit perfectly finite, perfectly sane coordinates. `assert_frame_sane` stays green
//! through all four. This module is what reddens.
//!
//! It records the TESSELLATED frame (`egui::Context::tessellate` → `Vec<ClippedPrimitive>`) rather
//! than the shape list, for one reason: tessellation is where paint order, clipping and batching
//! become observable as one flat sequence. epaint appends each shape into the last
//! `ClippedPrimitive` while `(clip_rect, texture_id)` is unchanged and starts a new one when either
//! moves (`epaint::Tessellator::tessellate_clipped_shape`), so the primitive sequence IS the
//! renderer's draw-call sequence — the thing a GPU would actually execute. A record over shapes
//! would show the same information only by re-deriving that batching by hand.
//!
//! TEXT, not pixels: the record is platform-independent (see "Stability" below), reviewable as a
//! diff, and needs no GPU — so it gates in CI, where this repo's GUI verification lives. Pixels
//! stay on the dev box (`just qa-shots`).
//!
//! It lives HERE for the same reason `frame_sanity` does: `vike-ui-theme` is layer 70 with `egui`
//! as its only dependency, every GUI crate is already above it, and a law spelled twice is the
//! defect class this crate exists to prevent. Behind the `test-support` feature, so a default build
//! compiles none of it.
//!
//! # The record format, and why each field is in it
//!
//! One line per `ClippedPrimitive`, in paint order, preceded by a totals header. Every coordinate
//! is ROUNDED TO AN INTEGER — sub-pixel jitter is not information a golden should carry, and a
//! record that re-baselines on a 0.0001px difference is a record nobody will keep.
//!
//! ```text
//! # vike frame record v1 (vike_ui_theme::frame_record)
//! # scenario: candles_price_only
//! ppp 1  primitives 42  vertices 5120  triangles 2210
//! ----
//!    0  clip [0 0 960 620]  tex M0  v 4  t 2  solid [0 0 960 620]  glyph -  runs 1  #101418ff*4  d 9e1f...
//! ```
//!
//! | field | why it is recorded |
//! |---|---|
//! | ordinal | **paint order**. A batch that moves earlier or later shows up as a moved line. |
//! | `clip` | **the scissor rect**. The only field that can see a clip narrowed to cut content. |
//! | `tex` | `M<n>` managed / `U<n>` user. `M0` is epaint's font atlas — which also carries the white pixel every solid fill samples, so it is the texture of nearly everything. Recorded because a NEW texture id is a new draw call. |
//! | `v` / `t` | vertex and triangle counts. Content appearing or vanishing moves these even when the bounding box does not. |
//! | `solid` | integer bounding box over vertices at `epaint::WHITE_UV` — i.e. the untextured geometry: rects, strokes, meshes. **Where things were drawn.** |
//! | `glyph` | the same box over textured vertices — the laid-out TEXT. Deliberately a SEPARATE field: it is the one number whose reproducibility depends on font layout rather than on our own arithmetic, so it can be dropped without touching the rest (see "Fonts"). |
//! | `runs` + colour runs | consecutive-equal-colour runs over the VISIBLE (non-zero-alpha) vertex stream, IN ORDER, as `#rrggbbaa*count`. Colours are exact `u8` — perfectly stable — and the ORDER is what makes a z-order inversion inside one batch visible. Capped at [`MAX_COLOR_RUNS`] for readability; the count is always exact. |
//! | `d` | FNV-1a over the whole vertex stream (rounded position + colour, in order) and the index buffer. The backstop for everything the capped run list elides. It hashes the SAME rounded values the visible fields carry, so it is exactly as stable as they are — never more sensitive. |
//!
//! **Deliberately NOT recorded**, each for a reason the next person will want:
//!
//! - **Texture UVs.** Glyph UVs are positions inside a font atlas that GROWS as glyphs are
//!   rasterized; they depend on the order every glyph in the process was first seen, which is a
//!   property of the test binary's scheduling and not of the frame. Recording them would make the
//!   golden depend on which other tests ran first.
//! - **Raw float coordinates.** See the rounding rule above.
//! - **Individual vertex positions.** A 5,000-vertex frame would produce an unreviewable golden,
//!   and the bounding box plus the digest already separate "moved" from "did not move".
//! - **Index buffer contents (visibly).** They are pure topology; they go into the digest, where a
//!   change is caught without costing a reviewer anything.
//!
//! # Stability — the property the whole scheme rests on
//!
//! A golden that is not reproducible is worse than no golden: it trains everyone to re-baseline on
//! red. Three hazards were identified and each is handled explicitly.
//!
//! **1. Time and timezone.** The chart's x-axis labels and hour/day grid marks are wall-clock
//! derived (`vike_chart::tz::to_naive`), and `vike_chart::DisplayTz`'s DEFAULT is `Local` — i.e.
//! `chrono::Local`, i.e. the machine's zone. That is not a hypothetical: it moves both the label
//! TEXT and the grid-line POSITIONS. The fixture side pins it (`vike-chart`'s
//! `tests/common/mod.rs` sets `DisplayTz::Utc`), and [`rounding_margin`] plus the twice-generated
//! diff are what caught it being necessary.
//!
//! **2. Fonts.** `vike-app`'s `install_fonts` reads SYSTEM fonts with a silent fallback, so text
//! metrics under the real app differ per machine. The headless harnesses do NOT call it: they call
//! a `bind_chart_font_families` helper that binds the custom family names to
//! `egui::FontDefinitions::default()`'s Proportional family — and egui's default definitions are
//! fonts EMBEDDED IN THE BINARY (Ubuntu-Light / Hack / emoji), not system lookups. Text layout is
//! therefore the same pure-Rust `ab_glyph` arithmetic everywhere. `glyph` is nonetheless kept as
//! its own field so that if a future toolchain proves otherwise, the fix is deleting one column
//! rather than abandoning the suite.
//!
//! **3. Floating-point drift across platforms.** Almost none is possible, and it is worth being
//! precise about why. Everything from `run_ui` through `tessellate` is `f32` arithmetic that Rust
//! will not reassociate and LLVM will not contract, so the same binary logic executes the same
//! operations in the same order on Linux and Windows alike. The ONE input that can genuinely differ
//! is libm: the chart fixtures compute prices with `f64::sin`/`f64::cos`, and glibc and the MSVC
//! runtime are each permitted their own last-bit result.
//!
//! So the hazard is testable head-on rather than by proxy, and `vike-chart`'s
//! `a_one_ulp_price_perturbation_leaves_every_record_unchanged` does exactly that: it nudges every
//! fixture price by one `f64` ULP — about 10× MORE than the worst real libm disagreement, since
//! `100 + 6·sin(x)` carries a 1-ULP error in `sin` into roughly a tenth of an ULP of the price —
//! and asserts every one of the sixteen records is byte-identical. [`rounding_margin`] remains as a
//! diagnostic; [`NEAR_EDGE_EPS`] records why it is not the gate.

use egui::epaint::{ClippedPrimitive, Color32, Mesh, Primitive, TextureId};
use egui::{Pos2, Rect};
use std::path::Path;

/// Bumped when the record GRAMMAR changes — every golden must then be regenerated. It is in the
/// file so a stale golden fails with "v1 vs v2" instead of a thousand-line diff.
pub const RECORD_VERSION: &str = "v1";

/// How many colour runs a primitive line prints before eliding. The exact run COUNT is always
/// printed, and the digest covers what is elided; this bound only keeps a line reviewable.
///
/// 16 was chosen against the real distribution rather than picked: across the sixteen chart
/// scenarios, 6 of ~96 primitive lines carry more runs than this, and raising the cap to cover them
/// would push the longest line past 400 characters for the benefit of two outliers.
pub const MAX_COLOR_RUNS: usize = 16;

/// How close to a `.5` rounding boundary a coordinate must sit to be COUNTED as near-edge by
/// [`rounding_margin`].
///
/// ⚠ **A reporting threshold, not a gate — and the story of why is worth the eight lines.** This
/// was first written as an assertion ("no recorded coordinate may come within EPS of a rounding
/// boundary"), on the theory that a coordinate on a knife edge is one a platform's float drift
/// could flip. It was set at 1e-3 and fired at once: `candles_with_volume` puts a coordinate at
/// `63.499626`. Lowered to 1e-5, it fired again on a DIFFERENT scenario: `solid_background` puts
/// one at `0.49999997`, which is 3e-8 — exactly one `f32` ULP — below the boundary.
///
/// Neither is a fragile golden, and that is the point. Both values are reached by *deterministic*
/// arithmetic: the second is pure layout (screen size × pane fractions), which contains no libm
/// call at all and is bit-identical on every platform. Proximity to a boundary simply does not
/// distinguish a reproducible coordinate from a fragile one, so any threshold over it is a coin
/// toss between false alarms and vacuity. `vike-chart`'s
/// `a_one_ulp_price_perturbation_leaves_every_record_unchanged` replaced it: it perturbs the only
/// input that CAN differ between platforms — the `f64` prices the fixtures compute with
/// `sin`/`cos` — and asserts the record does not move. That is the hazard itself rather than a
/// proxy for it, and it needs no constant.
///
/// What survives here is the diagnostic: how many coordinates sit near an edge, how many are exact
/// halves, and which one is tightest. Useful when a golden DOES move unexpectedly; not a gate.
pub const NEAR_EDGE_EPS: f32 = 1.0e-5;

// ============================ the record ============================

/// Render one tessellated frame as the canonical text record.
///
/// `scenario` is written into the header so a golden self-identifies: two goldens swapped by a
/// copy-paste fail on line 2 instead of producing a thousand-line diff nobody reads.
#[must_use]
pub fn record_frame(
    scenario: &str,
    pixels_per_point: f32,
    primitives: &[ClippedPrimitive],
) -> String {
    let mut vertices = 0usize;
    let mut triangles = 0usize;
    for p in primitives {
        if let Primitive::Mesh(m) = &p.primitive {
            vertices += m.vertices.len();
            triangles += m.indices.len() / 3;
        }
    }

    let mut s = String::with_capacity(4096);
    s.push_str(&format!("# vike frame record {RECORD_VERSION} (vike_ui_theme::frame_record)\n"));
    s.push_str(&format!("# scenario: {scenario}\n"));
    s.push_str(&format!(
        "ppp {}  primitives {}  vertices {}  triangles {}\n",
        round(pixels_per_point),
        primitives.len(),
        vertices,
        triangles,
    ));
    s.push_str("----\n");
    for (i, p) in primitives.iter().enumerate() {
        s.push_str(&primitive_line(i, p));
        s.push('\n');
    }
    s
}

fn primitive_line(i: usize, p: &ClippedPrimitive) -> String {
    let clip = fmt_rect(p.clip_rect);
    match &p.primitive {
        Primitive::Callback(cb) => {
            // A GPU callback paints outside epaint entirely; the rect it reserves is all the
            // record can honestly say about it.
            format!("{i:>4}  clip {clip}  callback rect {}", fmt_rect(cb.rect))
        }
        Primitive::Mesh(m) => {
            let (solid, glyph) = mesh_boxes(m);
            let runs = color_runs(m);
            let shown = runs
                .iter()
                .take(MAX_COLOR_RUNS)
                .map(|(c, n)| format!("{}*{n}", fmt_color(*c)))
                .collect::<Vec<_>>()
                .join(" ");
            let elided = if runs.len() > MAX_COLOR_RUNS { "  +.." } else { "" };
            format!(
                "{i:>4}  clip {clip}  tex {}  v {}  t {}  solid {solid}  glyph {glyph}  runs {}  {shown}{elided}  d {:016x}",
                fmt_texture(m.texture_id),
                m.vertices.len(),
                m.indices.len() / 3,
                runs.len(),
                digest(m),
            )
        }
    }
}

/// The two bounding boxes: untextured (`WHITE_UV`) vertices, then textured (glyph) vertices.
///
/// Split on `uv == WHITE_UV` rather than on texture id, because epaint batches TEXT and SOLID
/// geometry into the same mesh — both sample `TextureId::Managed(0)`, the font atlas, whose
/// top-left pixel is the white pixel every fill uses. Texture id alone cannot separate them.
fn mesh_boxes(m: &Mesh) -> (String, String) {
    let mut solid = BoxAcc::default();
    let mut glyph = BoxAcc::default();
    for v in &m.vertices {
        if v.uv == egui::epaint::WHITE_UV {
            solid.add(v.pos);
        } else {
            glyph.add(v.pos);
        }
    }
    (solid.render(), glyph.render())
}

#[derive(Default)]
struct BoxAcc {
    r: Option<Rect>,
}

impl BoxAcc {
    fn add(&mut self, p: Pos2) {
        self.r = Some(match self.r {
            None => Rect::from_min_max(p, p),
            Some(r) => r.union(Rect::from_min_max(p, p)),
        });
    }
    fn render(&self) -> String {
        self.r.map_or_else(|| "-".to_owned(), fmt_rect)
    }
}

/// Consecutive-equal-colour runs over the VISIBLE vertex stream, in order. Order is the point: two
/// shapes of different colours swapped inside one batch produce the same colour SET and the same
/// counts, and a different SEQUENCE.
///
/// ⚠ Fully TRANSPARENT vertices are skipped, and that is not a rounding-off: epaint's
/// anti-aliasing emits a zero-alpha skirt vertex alongside each edge vertex, so an unfiltered run
/// list is an unreadable `#00000000*1 #0a0b0d48*2 #00000000*2 …` alternation — measured at 1,177
/// runs for one chart pane, of which every second one paints nothing. Filtering lets the real
/// colours collapse into runs a reviewer can read, and costs no coverage: the skirt still goes
/// through [`digest`] with everything else, and a shape recoloured to transparent still moves both
/// the run list and the run count.
fn color_runs(m: &Mesh) -> Vec<(Color32, usize)> {
    let mut runs: Vec<(Color32, usize)> = Vec::new();
    for v in m.vertices.iter().filter(|v| v.color.a() != 0) {
        match runs.last_mut() {
            Some((c, n)) if *c == v.color => *n += 1,
            _ => runs.push((v.color, 1)),
        }
    }
    runs
}

/// FNV-1a 64 over the ROUNDED vertex stream (position + colour, in order) and the index buffer.
///
/// It hashes the same integers the visible fields print, deliberately: a digest more precise than
/// the record would re-introduce the sub-pixel sensitivity the rounding rule exists to remove, and
/// would fail on frames whose printed record is identical — the worst possible failure mode for a
/// golden.
fn digest(m: &Mesh) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    };
    for v in &m.vertices {
        eat(&round(v.pos.x).to_le_bytes());
        eat(&round(v.pos.y).to_le_bytes());
        eat(&v.color.to_array());
    }
    for i in &m.indices {
        eat(&i.to_le_bytes());
    }
    h
}

// ============================ rounding-margin measurement ============================

/// How close this frame's coordinates come to a rounding boundary — a DIAGNOSTIC, not a gate. See
/// [`NEAR_EDGE_EPS`] for the two measurements that demoted it from one.
#[derive(Debug, Default, Clone)]
pub struct RoundingMargin {
    /// Coordinates measured (every vertex x/y and every clip-rect edge).
    pub samples: usize,
    /// Coordinates sitting EXACTLY on a `.5` boundary. Expected and unremarkable: egui rounds
    /// layout to whole pixels and epaint's anti-alias feathering expands by exactly 0.5, so roughly
    /// a third of a chart frame's coordinates are exact halves.
    pub exact_halves: usize,
    /// Coordinates within [`NEAR_EDGE_EPS`] of a boundary without sitting exactly on one.
    pub near_edge: usize,
    /// The smallest NON-ZERO distance from a `.5` boundary; `f32::MAX` when every sample was an
    /// exact half.
    pub min_margin: f32,
    /// The coordinate that produced [`Self::min_margin`].
    pub worst: f32,
}

impl std::fmt::Display for RoundingMargin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} coords | {} exact halves | {} within {NEAR_EDGE_EPS:e} of a boundary | closest non-exact approach {:e} (at {})",
            self.samples, self.exact_halves, self.near_edge, self.min_margin, self.worst
        )
    }
}

/// Measure [`RoundingMargin`] over a tessellated frame.
#[must_use]
pub fn rounding_margin(primitives: &[ClippedPrimitive]) -> RoundingMargin {
    let mut out = RoundingMargin { min_margin: f32::MAX, ..Default::default() };
    let mut take = |v: f32| {
        if !v.is_finite() {
            return; // frame_sanity's job, not this one
        }
        out.samples += 1;
        // Distance to the nearest `.5` boundary: 0 means the value IS a half-integer (the knife
        // edge for round-half-away-from-zero), 0.5 means it is an exact integer (maximally safe).
        let m = ((v - v.floor()) - 0.5).abs();
        if m == 0.0 {
            out.exact_halves += 1;
            return;
        }
        if m < NEAR_EDGE_EPS {
            out.near_edge += 1;
        }
        if m < out.min_margin {
            out.min_margin = m;
            out.worst = v;
        }
    };
    for p in primitives {
        for e in [p.clip_rect.min.x, p.clip_rect.min.y, p.clip_rect.max.x, p.clip_rect.max.y] {
            take(e);
        }
        if let Primitive::Mesh(m) = &p.primitive {
            for v in &m.vertices {
                take(v.pos.x);
                take(v.pos.y);
            }
        }
    }
    out
}

// ============================ golden compare ============================

/// Compare `record` against the golden at `path`, or rewrite it when `update` is set.
///
/// `update` and `regen_hint` are PARAMETERS, never an environment read: this is a library, and the
/// workspace rule (`vike_ops::settings`) is that libraries take configuration as parameters and
/// only binaries — here, the test — read the process environment. `regen_hint` is the command line
/// that rewrites the golden, printed in the failure so the reader is not left guessing; the calling
/// test owns both it and the switch it names.
///
/// Line endings are normalised on BOTH sides before comparing. `.gitattributes` pins the golden
/// directory to `eol=lf`, but a checkout made with `core.autocrlf=true` before that rule existed
/// would otherwise redden every golden at once, with a diff in which no line looks different.
///
/// # Panics
///
/// If the golden is missing, or differs. The message names the first differing lines and
/// `regen_hint`.
#[track_caller]
pub fn assert_frame_golden(record: &str, path: &Path, update: bool, regen_hint: &str) {
    if update {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .unwrap_or_else(|e| panic!("cannot create golden dir {}: {e}", dir.display()));
        }
        std::fs::write(path, record)
            .unwrap_or_else(|e| panic!("cannot write golden {}: {e}", path.display()));
        return;
    }

    let Ok(golden) = std::fs::read_to_string(path) else {
        panic!(
            "missing golden {}\n  generate it with:  {regen_hint}\n  \
             …then READ the diff before committing it — a golden accepted without being read is \
             not a gate.",
            path.display()
        );
    };

    let want: Vec<&str> = golden.lines().map(str::trim_end).collect();
    let got: Vec<&str> = record.lines().map(str::trim_end).collect();
    if want == got {
        return;
    }
    panic!(
        "frame record does not match {}\n{}  rewrite with:  {regen_hint}\n  \
         a changed record means the frame's geometry, paint order, clipping or colours moved — \
         confirm that was intended before accepting the new bytes.",
        path.display(),
        line_diff(&want, &got),
    );
}

/// The first differing lines, golden-vs-actual, capped. Not a real diff algorithm on purpose: a
/// record is a positional list, so an inserted primitive shifts everything after it and the FIRST
/// difference is the one that explains the change.
fn line_diff(want: &[&str], got: &[&str]) -> String {
    const MAX: usize = 10;
    let mut out = format!("  golden {} lines, actual {} lines\n", want.len(), got.len());
    let mut shown = 0;
    for i in 0..want.len().max(got.len()) {
        let (w, g) = (want.get(i).copied(), got.get(i).copied());
        if w == g {
            continue;
        }
        out.push_str(&format!(
            "  line {}:\n    - {}\n    + {}\n",
            i + 1,
            w.unwrap_or("<missing>"),
            g.unwrap_or("<missing>"),
        ));
        shown += 1;
        if shown == MAX {
            out.push_str("  … (further differences elided)\n");
            break;
        }
    }
    out
}

// ============================ formatting internals ============================

/// Round-half-away-from-zero to an integer. Saturating (`as i64` saturates in Rust, `NaN` → 0) so
/// the recorder can never panic on a frame — reporting a bad coordinate is
/// [`crate::frame_sanity`]'s job, and a recorder that aborts first would hide it.
fn round(v: f32) -> i64 {
    v.round() as i64
}

fn fmt_rect(r: Rect) -> String {
    format!("[{} {} {} {}]", round(r.min.x), round(r.min.y), round(r.max.x), round(r.max.y))
}

fn fmt_color(c: Color32) -> String {
    let [r, g, b, a] = c.to_array();
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn fmt_texture(t: TextureId) -> String {
    match t {
        TextureId::Managed(n) => format!("M{n}"),
        TextureId::User(n) => format!("U{n}"),
    }
}
