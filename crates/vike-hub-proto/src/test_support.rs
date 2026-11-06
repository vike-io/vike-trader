//! The committed WIRE-GOLDEN checks the hub protocols' tag suites share, compiled once here behind
//! `test-support` (only their dev edges turn it on): `crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`
//! (the gatewayhub's own protocol), `crates/vike-market-stream/tests/stream_tag_fixtures.rs` (the tube),
//! `crates/vike-datahub-client/tests/strategy_lane_wire.rs` (the datahub's strategy lane), and the subset
//! comparison `crates/vike-datahub-client/tests/wire_tag_fixtures.rs` runs over its frozen captures. They used
//! to carry a copy each.
//!
//! A golden file is one JSON frame BODY per line under `fixtures/`. Every path is repo-relative and is
//! resolved against the root the calling test hands to [`Goldens::at`] (its own manifest directory's
//! `../..`): `src/` may not bake a compile-time path (`crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`).
//! Nothing here spells an expected variant tag; each suite's `SITES` table says only WHERE each enum sits.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::PathBuf;

use serde_json::Value;

use crate::frame::{read_frame_raw, write_frame};

/// Where the value of one wire enum sits inside a golden frame: the fixture file, and the keys from the
/// frame's root down to it (empty = the frame itself IS the enum's value). Frames are externally tagged,
/// so the first key of a path is the root enum's variant tag. A decimal key indexes an array, and `*`
/// walks EVERY element of an array or every value of an object.
pub struct Site {
    pub file: &'static str,
    pub path: &'static [&'static str],
}

/// The golden files of one suite, read under the repository root.
pub struct Goldens {
    root: PathBuf,
}

impl Goldens {
    /// The goldens under `root`, the repository checkout the calling test was built in.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The UTF-8 text of the repo-relative `rel`.
    pub fn read_text(&self, rel: &str) -> String {
        let bytes = std::fs::read(self.root.join(rel)).unwrap_or_else(|e| {
            panic!(
                "{rel} could not be read ({e}).\nThese goldens pin the wire; restore the file from git — \
                 never regenerate it to make a test pass."
            )
        });
        String::from_utf8(bytes).unwrap_or_else(|e| panic!("{rel} is not UTF-8: {e}"))
    }

    /// The golden frame bodies of `rel`, one per non-empty line.
    pub fn lines(&self, rel: &str) -> Vec<String> {
        self.read_text(rel)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Decode every golden line of `rel` as `T`, re-encode it THROUGH THE FRAME CODEC, and require the
    /// golden to survive ([`contains`]). `src` is the source file whose wire tags the goldens pin, named
    /// in every failure.
    pub fn assert_round_trip<T>(&self, rel: &str, expect_frames: usize, src: &str)
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        let goldens = self.lines(rel);
        assert_eq!(
            goldens.len(),
            expect_frames,
            "{rel} should hold {expect_frames} frames, not {} — a frame was added or dropped without \
             updating this assertion, and a shrunken fixture guards less than it claims",
            goldens.len()
        );
        for golden in &goldens {
            let expected: Value = serde_json::from_str(golden)
                .unwrap_or_else(|e| panic!("{rel} holds a line that is not JSON ({e}): {golden}"));
            let decoded: T = serde_json::from_str(golden).unwrap_or_else(|e| {
                panic!(
                    "A GOLDEN FRAME NO LONGER DECODES — a wire tag or field in {src} changed.\n\
                     fixture: {rel}\n  frame: {golden}\n  serde: {e}\nRestore the source; do not touch \
                     the fixture."
                )
            });
            let mut wire = Vec::new();
            write_frame(&mut wire, &decoded).expect("the wire types encode");
            let body = read_frame_raw(&mut Cursor::new(wire)).expect("a frame written reads back");
            let reencoded: Value = serde_json::from_slice(&body).expect("the body is JSON");
            contains(&reencoded, &expected, "", "golden frame").unwrap_or_else(|why| {
                panic!(
                    "THIS BUILD NO LONGER PRODUCES A GOLDEN FRAME'S BYTES.\nfixture: {rel}\n  frame: {golden}\n  \
                     {why}\nA wire tag or field in {src} was renamed. Restore it."
                )
            });
        }
    }

    /// The variant tags found at `site` over the golden frames of its file: a bare string is a unit
    /// variant's tag, an object with one key is a data variant's. A frame without the path is skipped (it
    /// is some other variant of the root enum).
    pub fn tags_at(&self, site: &Site) -> BTreeSet<String> {
        let mut tags = BTreeSet::new();
        for golden in self.lines(site.file) {
            let frame: Value = serde_json::from_str(&golden).unwrap_or_else(|e| {
                panic!("{} holds a line that is not JSON ({e}): {golden}", site.file)
            });
            collect_tags(&frame, site.path, &mut tags);
        }
        tags
    }

    /// The variant tags a golden frame carries for `name`, over every site `sites` gives it.
    pub fn golden_tags_of(&self, sites: &[(&str, &[Site])], name: &str) -> BTreeSet<String> {
        let sites = sites
            .iter()
            .find(|(enum_name, _)| *enum_name == name)
            .map(|(_, sites)| *sites)
            .unwrap_or_else(|| {
                panic!("`{name}` is a wire enum with no golden site in SITES; add its row")
            });
        sites.iter().flat_map(|site| self.tags_at(site)).collect()
    }

    /// Every variant of every wire enum (`wire_enums`: the declaring file, then its enum names) appears in
    /// a golden frame **at that enum's own position** (`sites`). Keyed on `(enum, variant)`: a same-named
    /// variant of another enum, or the same word in another position, covers nothing. `fixture_dir` is
    /// where a new variant's captured frame belongs, named in the failure.
    pub fn assert_every_variant_is_covered(
        &self,
        wire_enums: &[(&str, &[&str])],
        sites: &[(&str, &[Site])],
        fixture_dir: &str,
    ) {
        let mut missing = Vec::new();
        for (file, enums) in wire_enums {
            let src = self.read_text(file);
            for name in *enums {
                let golden = self.golden_tags_of(sites, name);
                for variant in variants_of(&src, name) {
                    if !golden.contains(&variant) {
                        missing.push(format!("{name}::{variant}"));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "wire variant(s) that no golden frame carries at their enum's position: {missing:?}\nA NEW \
             variant ships with a captured frame in {fixture_dir} — a wire tag nobody captured is a wire \
             tag nobody holds still. (A same-named variant of another enum does not count: SITES says \
             where each enum sits in a frame.)"
        );
    }

    /// `sites` and `wire_enums` name the same enums, so a coverage row cannot be forgotten or left behind,
    /// and every site really finds something: a path that matches no frame covers nothing, silently.
    pub fn assert_every_enum_has_a_golden_site(
        &self,
        wire_enums: &[(&str, &[&str])],
        sites: &[(&str, &[Site])],
    ) {
        let listed: Vec<&str> =
            wire_enums.iter().flat_map(|(_, names)| names.iter().copied()).collect();
        let sited: Vec<&str> = sites.iter().map(|(name, _)| *name).collect();
        let unsited: Vec<&&str> = listed.iter().filter(|n| !sited.contains(n)).collect();
        let orphan: Vec<&&str> = sited.iter().filter(|n| !listed.contains(n)).collect();
        assert!(unsited.is_empty(), "wire enum(s) in WIRE_ENUMS with no row in SITES: {unsited:?}");
        assert!(orphan.is_empty(), "SITES row(s) for an enum WIRE_ENUMS does not list: {orphan:?}");
        for (name, enum_sites) in sites {
            for site in *enum_sites {
                assert!(
                    !self.tags_at(site).is_empty(),
                    "the site {:?} of `{name}` in {} matches no golden frame",
                    site.path,
                    site.file
                );
            }
        }
    }
}

/// `expected` ⊆ `actual`, recursively: every key of `expected` exists in `actual` with a contained value,
/// arrays match in length and elementwise, scalars are equal. The asymmetry is the point: an additive field
/// is allowed and a renamed one never is. `Err` names the path of the first divergence; `frame_word` is
/// what the suite calls the expected document (`"golden frame"`, `"frozen frame"`).
pub fn contains(
    actual: &Value,
    expected: &Value,
    at: &str,
    frame_word: &str,
) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for (k, ev) in e {
                let Some(av) = a.get(k) else {
                    let present: Vec<&String> = a.keys().collect();
                    return Err(format!(
                        "{at}/{k} is in the {frame_word} and NOT in what this build encodes \
                         (present keys: {present:?})"
                    ));
                };
                contains(av, ev, &format!("{at}/{k}"), frame_word)?;
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(e)) if a.len() == e.len() => {
            for (i, (av, ev)) in a.iter().zip(e).enumerate() {
                contains(av, ev, &format!("{at}/{i}"), frame_word)?;
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => Err(format!("{at}: {frame_word} has {expected}, this build encodes {actual}")),
    }
}

/// The variants of `pub enum NAME { … }` in `src`: the identifiers at four-space indent that start with a
/// capital (doc comments, attributes and struct-variant fields are all something else).
pub fn variants_of(src: &str, name: &str) -> Vec<String> {
    let header = format!("pub enum {name} {{");
    let start = src.find(&header).unwrap_or_else(|| panic!("`{header}` is not in the source"));
    let mut out = Vec::new();
    for line in src[start..].lines().skip(1) {
        if line == "}" {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else { continue };
        if rest.starts_with(' ') {
            continue; // a struct variant's field, or a continuation line
        }
        if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
            let ident: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            out.push(ident);
        }
    }
    assert!(
        !out.is_empty(),
        "no variants found for {name} — the scanner stopped matching the source"
    );
    out
}

/// Walk `path` from `at` ([`Site`]'s rules); whatever sits at the end of a path is a tag.
fn collect_tags(at: &Value, path: &[&str], tags: &mut BTreeSet<String>) {
    let Some((key, rest)) = path.split_first() else {
        match at {
            Value::String(tag) => {
                tags.insert(tag.clone());
            }
            Value::Object(map) if map.len() == 1 => {
                tags.extend(map.keys().cloned());
            }
            _ => {}
        }
        return;
    };
    if *key == "*" {
        match at {
            Value::Array(items) => items.iter().for_each(|v| collect_tags(v, rest, tags)),
            Value::Object(map) => map.values().for_each(|v| collect_tags(v, rest, tags)),
            _ => {}
        }
        return;
    }
    let next = match at {
        Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
        other => other.get(*key),
    };
    if let Some(next) = next {
        collect_tags(next, rest, tags);
    }
}
