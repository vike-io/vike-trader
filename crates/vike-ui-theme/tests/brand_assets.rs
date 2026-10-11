//! Every file under `assets/brand/` is what `vike_ui_theme::brand` draws, or what
//! `crates/vike-ui-theme/ui-theme.toml` says — the brand's member of the committed-artifact drift
//! family (`crates/vike-ops/CLAUDE.md`'s "Committed generated artifacts (the drift-gate family)").
//!
//! Each file is re-derived here from its source and compared:
//!
//! - `ui-theme.css` and `brand-book.html`, and the Rust constants the crate compiles (the
//!   `*_tokens.rs` files beside `theme.rs`), from `ui-theme.toml` (`tests/tokens_gen/` draws them), byte
//!   for byte;
//! - the SVGs and the desktop entry, byte for byte;
//! - every PNG — on its own, or inside the `.ico` and the `.icns` — by its DECODED pixels. The PNG
//!   encoder is a lockfile dependency, and its own documentation
//!   (`image::codecs::png::PngEncoder::new_with_quality`) says its exact output is "expressly not
//!   part of the SemVer stability guarantee". A byte gate would redden on a bump that moved no
//!   pixel; a pixel gate reddens on the edit that matters.
//!
//! A PR that touches only `assets/` selects no crate, so `.github/workflows/brand-assets.yml` runs
//! this file on such a PR. Any change to this crate runs it in the roster lane as well.
//!
//! # Regenerating
//!
//! ```sh
//! VIKE_REGEN_BRAND_ASSETS=1 cargo test -p vike-ui-theme --test brand_assets
//! ```
//!
//! …then LOOK at the files. A PNG, `.ico` or `.icns` whose pixels already match is left alone, so a
//! regeneration rewrites only what changed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::Color32;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, ImageFormat};
use vike_ui_theme::brand::{self, Grid, MARK, MARK_SMALL, Mark};
use vike_ui_theme::theme::{Theme, ThemeId};

mod tokens_gen;

/// Regeneration switch — see the module doc. Named so `crates/vike-ops/src/settings/rows/gui.rs`'s
/// `VIKE_REGEN_BRAND_ASSETS` row (`Naming::Konst("REGEN_ENV")`) resolves through this constant.
const REGEN_ENV: &str = "VIKE_REGEN_BRAND_ASSETS";

/// Printed in every failure, so the reader is not left guessing how to rewrite a file.
const REGEN_HINT: &str =
    "VIKE_REGEN_BRAND_ASSETS=1 cargo test -p vike-ui-theme --test brand_assets";

/// The `.ico`'s sizes: Windows' small, taskbar and Explorer sizes from 100% to 200% scaling, and
/// 256 for the large views.
const WINDOWS_ICON_PX: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// The `.icns`'s chunks, `(type, side)`. `ic11`–`ic14` and `ic10` are the 2× images of the point
/// sizes before them, so some sides appear twice.
const ICNS: [([u8; 4], u32); 10] = [
    (*b"icp4", 16),
    (*b"icp5", 32),
    (*b"ic11", 32),
    (*b"ic12", 64),
    (*b"ic07", 128),
    (*b"ic13", 256),
    (*b"ic08", 256),
    (*b"ic14", 512),
    (*b"ic09", 512),
    (*b"ic10", 1024),
];

/// The exact string `"1"`, the workspace's idiom for an opt-in switch — never a fuzzy truthy parse.
fn regen() -> bool {
    std::env::var(REGEN_ENV).as_deref() == Ok("1")
}

fn brand_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand")
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a file under assets/brand")).expect("mkdir");
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn stale(rel: &str) -> ! {
    panic!("assets/brand/{rel} is not what vike_ui_theme::brand draws; run `{REGEN_HINT}` and look")
}

// ---- the text files --------------------------------------------------------------------------

/// A number as the SVGs carry it: Rust's shortest round-trip form (`62`, `74.5`, `48.578125`).
fn num(v: f64) -> String {
    format!("{v}")
}

fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

fn svg_open() -> String {
    let b = num(f64::from(brand::BOX));
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {b} {b}\" width=\"{b}\" height=\"{b}\">\n"
    )
}

fn mark_body(m: &Mark, colour: &str) -> String {
    let f = |v: f32| num(f64::from(v));
    format!(
        "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{colour}\"/>\n\
         <line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{colour}\" stroke-width=\"{}\" \
         stroke-linecap=\"round\"/>\n",
        f(m.centre.0),
        f(m.centre.1),
        f(m.radius),
        f(m.from.0),
        f(m.from.1),
        f(m.to.0),
        f(m.to.1),
        f(m.stroke),
    )
}

/// The bare mark in one colour on a transparent ground.
fn svg_mark(m: &Mark, colour: Color32) -> String {
    format!("{}{}</svg>\n", svg_open(), mark_body(m, &hex(colour)))
}

/// The app icon — the mark on the Graphite tile, placed by `brand::tile_layout`.
fn svg_app_icon(small: bool) -> String {
    let l = brand::tile_layout(Grid::FullBleed, small);
    let t = Theme::of(ThemeId::Graphite);
    let (x0, side) = (num(l.centre - l.half), num(2.0 * l.half));
    let mut s = svg_open();
    let fill = if brand::TILE.gradient {
        s.push_str(&format!(
            "<defs><linearGradient id=\"tile\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
             <stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/>\
             </linearGradient></defs>\n",
            hex(t.grad_top),
            hex(t.bg)
        ));
        "url(#tile)".to_string()
    } else {
        hex(t.bg)
    };
    s.push_str(&format!(
        "<rect x=\"{x0}\" y=\"{x0}\" width=\"{side}\" height=\"{side}\" rx=\"{}\" fill=\"{fill}\"/>\n",
        num(l.radius)
    ));
    if let Some(h) = l.hairline {
        // A stroke is centred on its path: inset by half its width, it covers the tile's outer `h`.
        let (x, w) = (num(l.centre - l.half + h / 2.0), num(2.0 * l.half - h));
        s.push_str(&format!(
            "<rect x=\"{x}\" y=\"{x}\" width=\"{w}\" height=\"{w}\" rx=\"{}\" fill=\"none\" \
             stroke=\"{}\" stroke-width=\"{}\"/>\n",
            num(l.radius - h / 2.0),
            hex(t.border),
            num(h)
        ));
    }
    s.push_str(&format!(
        "<g transform=\"translate({} {}) scale({})\">\n{}</g>\n</svg>\n",
        num(l.offset.0),
        num(l.offset.1),
        num(l.scale),
        mark_body(l.mark, &hex(brand::ORANGE))
    ));
    s
}

/// Every text file: `(path under assets/brand/, contents)`.
fn text_files() -> Vec<(String, String)> {
    let tokens = tokens_gen::Tokens::load();
    vec![
        ("ui-theme.css".into(), tokens_gen::css(&tokens)),
        ("brand-book.html".into(), tokens_gen::book(&tokens)),
        ("mark.svg".into(), svg_mark(&MARK, brand::ORANGE)),
        ("mark-small.svg".into(), svg_mark(&MARK_SMALL, brand::ORANGE)),
        ("mark-on-light.svg".into(), svg_mark(&MARK, brand::ORANGE_ON_LIGHT)),
        ("mark-small-white.svg".into(), svg_mark(&MARK_SMALL, Color32::WHITE)),
        ("mark-small-black.svg".into(), svg_mark(&MARK_SMALL, Color32::BLACK)),
        ("app-icon.svg".into(), svg_app_icon(false)),
        ("app-icon-small.svg".into(), svg_app_icon(true)),
        (brand::desktop_file_name(), brand::desktop_entry("vike-desktop")),
    ]
}

fn check_text(rel: &str, want: &str) {
    let path = brand_dir().join(rel);
    if std::fs::read(&path).ok().as_deref() == Some(want.as_bytes()) {
        return;
    }
    if regen() {
        return write(&path, want.as_bytes());
    }
    stale(rel)
}

// ---- the rasters -----------------------------------------------------------------------------

fn encode_png(side: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, FilterType::Adaptive)
        .write_image(rgba, side, side, ExtendedColorType::Rgba8)
        .expect("encode a PNG");
    out
}

/// Whether a PNG stream decodes to exactly `rgba` at `side` × `side`.
fn png_is(bytes: &[u8], side: u32, rgba: &[u8]) -> bool {
    image::load_from_memory_with_format(bytes, ImageFormat::Png)
        .map(|img| img.to_rgba8())
        .is_ok_and(|img| img.width() == side && img.height() == side && img.as_raw() == rgba)
}

fn check_png(rel: &str, side: u32, rgba: &[u8]) {
    let path = brand_dir().join(rel);
    if std::fs::read(&path).is_ok_and(|b| png_is(&b, side, rgba)) {
        return;
    }
    if regen() {
        return write(&path, &encode_png(side, rgba));
    }
    stale(rel)
}

/// An `.ico` holding each image as a PNG stream (Windows reads PNG entries at every size since
/// Vista).
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0]; // reserved; type 1 = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (side, png) in images {
        let s = if *side >= 256 { 0 } else { *side as u8 }; // 0 means 256
        out.extend_from_slice(&[s, s, 0, 0]); // width, height, palette size, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

/// `(side, png)` for every image of an `.ico`, or `None` when it is not one.
fn ico_images(bytes: &[u8]) -> Option<Vec<(u32, Vec<u8>)>> {
    if !bytes.starts_with(&[0, 0, 1, 0]) {
        return None;
    }
    let count = usize::from(u16::from_le_bytes(bytes.get(4..6)?.try_into().ok()?));
    (0..count)
        .map(|i| {
            let e = bytes.get(6 + 16 * i..22 + 16 * i)?;
            let side = if e[0] == 0 { 256 } else { u32::from(e[0]) };
            let len = u32::from_le_bytes(e[8..12].try_into().ok()?) as usize;
            let at = u32::from_le_bytes(e[12..16].try_into().ok()?) as usize;
            Some((side, bytes.get(at..at + len)?.to_vec()))
        })
        .collect()
}

/// An `.icns`: its magic, its total length, then one `(type, length, PNG)` chunk per image.
fn icns(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total = 8 + chunks.iter().map(|(_, png)| 8 + png.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(total as u32).to_be_bytes());
    for (ty, png) in chunks {
        out.extend_from_slice(ty);
        out.extend_from_slice(&((8 + png.len()) as u32).to_be_bytes());
        out.extend_from_slice(png);
    }
    out
}

/// `(type, png)` for every chunk of an `.icns`, or `None` when it is not one.
fn icns_images(bytes: &[u8]) -> Option<Vec<([u8; 4], Vec<u8>)>> {
    let total = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    if !bytes.starts_with(b"icns") || total != bytes.len() {
        return None;
    }
    let (mut at, mut out) = (8, Vec::new());
    while at < total {
        let ty: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        let len = u32::from_be_bytes(bytes.get(at + 4..at + 8)?.try_into().ok()?) as usize;
        if len < 8 {
            return None;
        }
        out.push((ty, bytes.get(at + 8..at + len)?.to_vec()));
        at += len;
    }
    Some(out)
}

// ---- the gate --------------------------------------------------------------------------------

/// Every file the gate derives, by its path under `assets/brand/`.
fn derived_files() -> BTreeSet<String> {
    let mut all: BTreeSet<String> = text_files().into_iter().map(|(name, _)| name).collect();
    all.extend(brand::LINUX_ICON_PX.iter().map(|side| format!("png/app-icon-{side}.png")));
    all.insert("vike-desktop.ico".into());
    all.insert("vike-desktop.icns".into());
    all
}

#[test]
fn every_text_file_is_the_geometry() {
    for (rel, text) in text_files() {
        check_text(&rel, &text);
    }
}

/// Nothing sits under `assets/brand/` that this gate does not derive: a hand-made file there would
/// look generated and be checked by nothing. (A MISSING file fails its own check above.)
#[test]
fn nothing_under_assets_brand_is_unaccounted_for() {
    let (root, derived) = (brand_dir(), derived_files());
    let (mut extra, mut stack) = (Vec::new(), vec![brand_dir()]);
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(&root).expect("under the root");
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel != "SOURCES.md" && !derived.contains(&rel) {
                extra.push(rel);
            }
        }
    }
    assert!(extra.is_empty(), "files under assets/brand/ that nothing derives: {extra:?}");
}

/// `SOURCES.md` says what every file is and who reads it; a new file cannot join without a row.
#[test]
fn the_sources_page_names_every_file() {
    let page = std::fs::read_to_string(brand_dir().join("SOURCES.md")).expect("SOURCES.md");
    let missing: Vec<String> =
        derived_files().into_iter().filter(|f| !page.contains(&format!("`{f}`"))).collect();
    assert!(missing.is_empty(), "assets/brand/SOURCES.md has no row for {missing:?}");
}

/// The Rust constants the crate compiles are what `ui-theme.toml` says: the TOML is the source and
/// the `*_tokens.rs` files in `src/` are drawn from it, so a value edited by hand in a generated file — or in the TOML
/// without regenerating — fails here by file name.
#[test]
fn the_generated_rust_is_the_tokens() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for (name, want) in tokens_gen::rust_files(&tokens_gen::Tokens::load()) {
        let path = src.join(name);
        if std::fs::read(&path).ok().as_deref() == Some(want.as_bytes()) {
            continue;
        }
        if regen() {
            write(&path, want.as_bytes());
            continue;
        }
        panic!(
            "crates/vike-ui-theme/src/{name} is not what ui-theme.toml says; run `{REGEN_HINT}` and look"
        );
    }
}

/// A typo in `ui-theme.toml` stops the generator and NAMES the key: a half-filled token file must fail
/// loudly here and never become half a theme in the app.
#[test]
#[should_panic(expected = "themes.graphite.bg = \"#12345\" is not `#rrggbb`")]
fn a_malformed_colour_stops_the_generator_naming_the_key() {
    let bad: toml::Table = "bg = \"#12345\"".parse().expect("toml");
    let _ = tokens_gen::colour(&bad, "bg", "themes.graphite");
}

#[test]
#[should_panic(expected = "themes.graphite.card is missing")]
fn a_missing_colour_stops_the_generator_naming_the_key() {
    let partial: toml::Table = "bg = \"#000000\"".parse().expect("toml");
    let _ = tokens_gen::colour(&partial, "card", "themes.graphite");
}

/// The page shows every value: each theme's colours, each status colour, each role's two sizes and
/// each chrome measure appear in it, so a token the generator forgot to draw is a failure.
#[test]
fn the_brand_book_shows_every_value() {
    let t = tokens_gen::Tokens::load();
    let page = tokens_gen::book(&t);
    for id in tokens_gen::THEMES {
        for field in tokens_gen::THEME_FIELDS {
            let c = Theme::of(match id {
                "graphite" => ThemeId::Graphite,
                "midnight" => ThemeId::Midnight,
                "dusk" => ThemeId::Dusk,
                _ => ThemeId::Carbon,
            });
            let colour = match field {
                "bg" => c.bg,
                "grad_top" => c.grad_top,
                "surface" => c.surface,
                "card" => c.card,
                "hover" => c.hover,
                "border" => c.border,
                "text" => c.text,
                "text_ui" => c.text_ui,
                "text2" => c.text2,
                "text3" => c.text3,
                "analysis_line" => c.analysis_line,
                _ => c.accent,
            };
            assert!(
                page.contains(&hex(colour).to_lowercase()),
                "{id}.{field} {} is not on the page",
                hex(colour)
            );
        }
    }
}

/// A rule on the page says how it is enforced: the tests that fail when the code breaks it, or, for a
/// rule with none, that it is words only. And every place a heavier weight is ruled is shown. A page
/// that listed rules without saying which can fail would read as enforced all of them.
/// Every market set, the heat ramp's two ends, both oranges and the radius are on the page, and the TOML's
/// `[market.*]` keys and labels are the ones `MarketId` stores and shows: the label is the one value
/// of a set the TOML and the code both spell, so it is the one that could drift apart.
#[test]
fn the_brand_book_shows_the_market_sets_the_heat_the_brand_and_the_shape() {
    let toml = std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads");
    let table: toml::Table = toml.parse().expect("ui-theme.toml parses");
    let page = tokens_gen::book(&tokens_gen::Tokens::load());
    for id in tokens_gen::MARKETS {
        let set = table["market"][id].as_table().expect("a market set is a table");
        let market = vike_ui_theme::market::MarketId::from_key(id)
            .unwrap_or_else(|| panic!("ui-theme.toml: market.{id} is not a stored market key"));
        assert_eq!(set["label"].as_str(), Some(market.label()), "market.{id}.label");
        for field in tokens_gen::MARKET_FIELDS {
            let hex = set[field].as_str().expect("a market colour is a string").to_lowercase();
            assert!(page.contains(&hex), "market.{id}.{field} {hex} is not on the page");
        }
    }
    assert_eq!(
        tokens_gen::MARKETS.len(),
        vike_ui_theme::market::MarketId::ALL.len(),
        "a market set has no TOML table, or a table has no set"
    );
    for key in ["cool", "warm"] {
        let hex = table["heat"][key].as_str().expect("a heat end").to_lowercase();
        assert!(page.contains(&hex), "heat.{key} {hex} is not on the page");
    }
    for entry in table["brand"].as_array().expect("[[brand]]") {
        let hex = entry["hex"].as_str().expect("a brand colour").to_lowercase();
        assert!(page.contains(&hex), "brand {hex} is not on the page");
    }
    let radius = table["shape"]["radius"].as_integer().expect("shape.radius");
    assert!(page.contains(&format!("{radius} pt")), "the radius is not on the page");
}

/// Every step of the spacing, stroke and strength scales is on the page, with its value and its Rust name:
/// the constants the kit reads are the ones the page draws.
#[test]
fn the_brand_book_shows_every_space_stroke_and_strength() {
    use vike_ui_theme::metrics::{alpha, space, stroke};
    let toml = std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads");
    let table: toml::Table = toml.parse().expect("ui-theme.toml parses");
    let page = tokens_gen::book(&tokens_gen::Tokens::load());
    let ruled: [(&str, &[(&str, f32)]); 3] = [
        (
            "space",
            &[
                ("NONE", space::NONE),
                ("HAIR", space::HAIR),
                ("XS", space::XS),
                ("SM", space::SM),
                ("MD", space::MD),
                ("LG", space::LG),
                ("XL", space::XL),
                ("XL2", space::XL2),
                ("XL3", space::XL3),
            ],
        ),
        (
            "stroke",
            &[("HAIRLINE", stroke::HAIRLINE), ("LINE", stroke::LINE), ("EDGE", stroke::EDGE)],
        ),
        ("alpha", &[("LIFT", alpha::LIFT), ("WASH", alpha::WASH)]),
    ];
    for (key, consts) in ruled {
        let rows = table[key].as_array().unwrap_or_else(|| panic!("[[{key}]]"));
        assert_eq!(
            rows.len(),
            consts.len(),
            "[[{key}]] has a row the kit has no constant for, or the reverse"
        );
        for (name, value) in consts {
            let row = rows
                .iter()
                .find(|r| r["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("[[{key}]] has no {name}"));
            let toml_value =
                row["value"].as_float().or_else(|| row["value"].as_integer().map(|i| i as f64));
            assert_eq!(toml_value.map(|v| v as f32), Some(*value), "{key}::{name}");
            assert!(
                page.contains(&format!("<code>{name}</code>")),
                "{key}::{name} is not on the page"
            );
        }
    }
}

#[test]
fn the_brand_book_shows_how_each_rule_is_enforced_and_where_weight_is_ruled() {
    let toml = std::fs::read_to_string(tokens_gen::toml_path()).expect("ui-theme.toml reads");
    let table: toml::Table = toml.parse().expect("ui-theme.toml parses");
    let t = tokens_gen::Tokens::load();
    let page = tokens_gen::book(&t);
    let rules = table["rule"].as_array().expect("[[rule]]");
    assert!(rules.len() >= 8, "the rules of use shrank to {}", rules.len());
    for rule in rules {
        let rule = rule.as_table().expect("a rule is a table");
        let id = rule["id"].as_str().expect("rule.id");
        let by = tokens_gen::checked_by(rule, id);
        assert!(page.contains(&format!("<li id=\"{id}\">")), "rule {id} is not on the page");
        if by.is_empty() {
            assert!(
                page.contains("Words only"),
                "rule {id} has no test and the page does not say so"
            );
        }
        for name in by {
            assert!(page.contains(name), "rule {id}: {name} is not on the page");
        }
    }
    let weights = table["weight_use"].as_array().expect("[[weight_use]]");
    assert!(!weights.is_empty(), "no weight is ruled, so the pin has nothing to name");
    for w in weights {
        let id = w.as_table().and_then(|w| w["id"].as_str()).expect("weight_use.id");
        assert!(page.contains(&format!("<code>{id}</code>")), "weight use {id} is not on the page");
    }
}

#[test]
fn every_linux_png_is_the_raster() {
    for side in brand::LINUX_ICON_PX {
        let rgba = brand::app_icon_rgba(side, Grid::FullBleed);
        check_png(&format!("png/app-icon-{side}.png"), side, &rgba);
    }
}

#[test]
fn the_windows_ico_is_the_raster() {
    let rel = "vike-desktop.ico";
    let want: Vec<(u32, Vec<u8>)> =
        WINDOWS_ICON_PX.iter().map(|&s| (s, brand::app_icon_rgba(s, Grid::FullBleed))).collect();
    let have = std::fs::read(brand_dir().join(rel)).ok().and_then(|b| ico_images(&b));
    let same = have.is_some_and(|have| {
        have.len() == want.len()
            && have
                .iter()
                .zip(&want)
                .all(|((hs, png), (ws, rgba))| hs == ws && png_is(png, *ws, rgba))
    });
    if same {
        return;
    }
    if regen() {
        let images: Vec<(u32, Vec<u8>)> =
            want.iter().map(|(s, rgba)| (*s, encode_png(*s, rgba))).collect();
        return write(&brand_dir().join(rel), &ico(&images));
    }
    stale(rel)
}

#[test]
fn the_macos_icns_is_the_raster() {
    let rel = "vike-desktop.icns";
    let want: Vec<([u8; 4], u32, Vec<u8>)> =
        ICNS.iter().map(|&(ty, s)| (ty, s, brand::app_icon_rgba(s, Grid::AppleInset))).collect();
    let have = std::fs::read(brand_dir().join(rel)).ok().and_then(|b| icns_images(&b));
    let same = have.is_some_and(|have| {
        have.len() == want.len()
            && have
                .iter()
                .zip(&want)
                .all(|((ht, png), (wt, s, rgba))| ht == wt && png_is(png, *s, rgba))
    });
    if same {
        return;
    }
    if regen() {
        let chunks: Vec<([u8; 4], Vec<u8>)> =
            want.iter().map(|(ty, s, rgba)| (*ty, encode_png(*s, rgba))).collect();
        return write(&brand_dir().join(rel), &icns(&chunks));
    }
    stale(rel)
}

/// The container writers and readers agree, so a regenerated `.ico` or `.icns` reads back as
/// written — and a 256-px entry's size byte of 0 reads back as 256.
#[test]
fn the_containers_round_trip() {
    let a = encode_png(16, &brand::app_icon_rgba(16, Grid::FullBleed));
    let b = encode_png(20, &brand::app_icon_rgba(20, Grid::FullBleed));
    let images = vec![(16, a.clone()), (256, b.clone())];
    assert_eq!(ico_images(&ico(&images)), Some(images));
    let chunks = vec![(*b"icp4", a), (*b"ic07", b)];
    assert_eq!(icns_images(&icns(&chunks)), Some(chunks));
}
