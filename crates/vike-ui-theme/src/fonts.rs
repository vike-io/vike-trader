//! The two faces every GUI surface draws with — Inter for words, JetBrains Mono for anything read
//! character by character (design system spec §3.3) — compiled into the binary, so text is the same
//! on every OS. The one icon face, Phosphor, is bundled beside them and draws from a family of its
//! own (`crate::icons`' module doc says why). The files and their provenance:
//! `assets/fonts/SOURCES.md`.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// The bold-weight family (Inter 600 first).
pub const SEMIBOLD: &str = "semibold";
/// The heaviest family (Inter 700 first).
pub const BOLD: &str = "bold";
/// Heavy numbers (JetBrains Mono 600 first).
pub const MONO_SEMIBOLD: &str = "mono-semibold";

/// egui's own embedded faces, kept BEHIND ours in every family: they draw the symbols and emoji
/// neither Inter nor JetBrains Mono has. Which characters those are is
/// `crates/vike-ops/tests/gui/ui_glyph_coverage.rs`'s to measure, not this comment's to count. The
/// icons are not among them: they draw from `crate::icons::FAMILY`.
const FALLBACKS: [&str; 3] = ["Hack", "NotoEmoji-Regular", "emoji-icon-font"];

/// The one `FontDefinitions` the app, its examples and its tests install.
pub fn definitions() -> FontDefinitions {
    // Starting from egui's defaults keeps its embedded fallback data and their tweaks.
    let mut defs = FontDefinitions::default();
    defs.font_data.remove("Ubuntu-Light");
    for (name, bytes) in FACES {
        defs.font_data.insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    }
    let chain = |lead: [&str; 2]| -> Vec<String> {
        lead.iter().chain(FALLBACKS.iter()).map(|s| s.to_string()).collect()
    };
    defs.families = [
        (FontFamily::Proportional, chain(["inter-400", "jbm-400"])),
        (FontFamily::Monospace, chain(["jbm-400", "inter-400"])),
        (FontFamily::Name(SEMIBOLD.into()), chain(["inter-600", "jbm-600"])),
        (FontFamily::Name(BOLD.into()), chain(["inter-700", "jbm-600"])),
        (FontFamily::Name(MONO_SEMIBOLD.into()), chain(["jbm-600", "inter-600"])),
        // Icons draw from Phosphor ALONE — never behind Inter, which maps 307 of Phosphor's
        // codepoints to glyphs of its own — then Hack, whose replacement glyph makes a missing
        // icon a visible box (`crate::icons`' module doc).
        (crate::icons::family(), vec![ICON_FACE.to_string(), "Hack".to_string()]),
    ]
    .into();
    defs
}

/// The icon face's name in the family lists. It leads the `icons` family and no other
/// (`crate::icons`' module doc says why).
pub(crate) const ICON_FACE: &str = "phosphor";

/// Every bundled face — the five text faces and the icon face — by the name the family lists use.
/// egui has no weight axis, so each weight is its own file.
pub(crate) const FACES: [(&str, &[u8]); 6] = [
    ("inter-400", include_bytes!("../../../assets/fonts/Inter-Regular.ttf")),
    ("inter-600", include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf")),
    ("inter-700", include_bytes!("../../../assets/fonts/Inter-Bold.ttf")),
    ("jbm-400", include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")),
    ("jbm-600", include_bytes!("../../../assets/fonts/JetBrainsMono-SemiBold.ttf")),
    (ICON_FACE, include_bytes!("../../../assets/fonts/Phosphor-Regular.ttf")),
];

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Each face, pinned to the exact upstream file (`assets/fonts/SOURCES.md`). A new font version
    /// is a deliberate edit of this table AND of the drift script, never a silent replacement.
    const PINS: [(&str, &str); 6] = [
        ("inter-400", "40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82"),
        ("inter-600", "78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3"),
        ("inter-700", "288316099b1e0a47a4716d159098005eef7c0066921f34e3200393dbdb01947f"),
        ("jbm-400", "a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f"),
        ("jbm-600", "1b3bfa1ed5665a4ce3f9feb68d2d4e40e70bf8b4b7d9a3edd418f321b4e166a0"),
        ("phosphor", "06b91e022b7ee899a63efced879392a74f0bacbda54e4467e9f663220d173a10"),
    ];

    #[test]
    fn every_bundled_face_is_the_pinned_file() {
        let got: Vec<(&str, String)> = FACES.iter().map(|(n, b)| (*n, sha256(b))).collect();
        let want: Vec<(&str, String)> = PINS.iter().map(|(n, h)| (*n, h.to_string())).collect();
        assert_eq!(got, want);
    }

    use egui::epaint::text::{Fonts, TextOptions};
    use egui::{FontFamily, FontId};

    fn fonts() -> Fonts {
        Fonts::new(TextOptions::default(), definitions())
    }

    fn text_families() -> [FontFamily; 5] {
        [
            FontFamily::Proportional,
            FontFamily::Monospace,
            FontFamily::Name(SEMIBOLD.into()),
            FontFamily::Name(BOLD.into()),
            FontFamily::Name(MONO_SEMIBOLD.into()),
        ]
    }

    /// Every family a call site can name draws real glyphs. An UNBOUND family panics in epaint's
    /// first layout — the crash that once kept the desktop from starting on Linux.
    #[test]
    fn every_family_is_bound_and_draws() {
        let mut f = fonts();
        let mut v = f.with_pixels_per_point(1.0);
        for fam in text_families() {
            assert!(v.glyph_width(&FontId::new(12.0, fam.clone()), 'W') > 0.0, "{fam:?}");
        }
    }

    /// Monospace IS monospaced (JetBrains Mono leads it) and Proportional is not (Inter leads it).
    #[test]
    fn monospace_is_monospaced_and_proportional_is_not() {
        let mut f = fonts();
        let mut v = f.with_pixels_per_point(1.0);
        let mono_i = v.glyph_width(&FontId::monospace(12.0), 'i');
        let mono_w = v.glyph_width(&FontId::monospace(12.0), 'W');
        let prop_i = v.glyph_width(&FontId::proportional(12.0), 'i');
        let prop_w = v.glyph_width(&FontId::proportional(12.0), 'W');
        assert_eq!(mono_i, mono_w);
        assert_ne!(prop_i, prop_w);
    }

    /// The five text families and the one icon family — nothing else is nameable.
    #[test]
    fn the_families_are_the_ruled_weights_and_the_icons() {
        let keys: Vec<FontFamily> = definitions().families.keys().cloned().collect();
        assert_eq!(keys.len(), 6, "{keys:?}");
        for fam in text_families() {
            assert!(keys.contains(&fam), "{fam:?} missing from {keys:?}");
        }
        assert!(keys.contains(&crate::icons::family()), "the icon family is missing: {keys:?}");
    }

    /// Each TEXT family leads with a bundled text face and ends with egui's embedded fallbacks,
    /// which draw the symbols and emoji our two faces lack. Ubuntu-Light is not used at all.
    #[test]
    fn every_text_family_leads_with_a_bundled_face_and_keeps_the_fallbacks() {
        let defs = definitions();
        let ours: Vec<&str> = FACES.iter().map(|(n, _)| *n).filter(|n| *n != ICON_FACE).collect();
        for fam in text_families() {
            let chain = &defs.families[&fam];
            assert!(ours.contains(&chain[0].as_str()), "{fam:?} leads with {}", chain[0]);
            assert_eq!(
                chain[chain.len() - 3..].to_vec(),
                FALLBACKS.map(String::from).to_vec(),
                "{fam:?}"
            );
            assert!(!chain.iter().any(|n| n == "Ubuntu-Light"), "{fam:?}");
        }
        assert!(!defs.font_data.contains_key("Ubuntu-Light"));
    }

    /// Icons draw from Phosphor ALONE, then Hack for the replacement glyph (a missing icon is a
    /// visible box, not nothing). Behind Inter, 307 of Phosphor's codepoints would draw Inter's own
    /// glyphs; in front of it, Phosphor's blank `a`–`z` would erase every word.
    #[test]
    fn the_icon_family_is_the_icon_face_then_the_replacement() {
        let chain = &definitions().families[&crate::icons::family()];
        assert_eq!(chain, &vec![ICON_FACE.to_string(), "Hack".to_string()]);
    }

    /// ...and no text family carries the icon face, so an icon cannot half-work inside a sentence.
    #[test]
    fn no_text_family_carries_the_icon_face() {
        let defs = definitions();
        for fam in text_families() {
            assert!(!defs.families[&fam].iter().any(|n| n == ICON_FACE), "{fam:?}");
        }
    }

    /// The OFL and MIT licences travel with the faces (§3.3), and so does the icon stylesheet the
    /// icon tests read. Read from the checkout, not compiled in.
    #[test]
    fn the_licences_and_the_icon_stylesheet_ship_beside_the_faces() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
        for (file, want) in [
            ("Inter-OFL.txt", "262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a"),
            (
                "JetBrainsMono-OFL.txt",
                "30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad",
            ),
            (
                "Phosphor-MIT.txt",
                "687fbe52d0eb5c2353eca27a4037e889145ee0b584d3b1abf67581c4a4a4e47c",
            ),
            (
                "Phosphor-Regular.css",
                "873761b8711147dc516b6102936e9ad005f3a3015349efcde1a496f0326f1051",
            ),
        ] {
            let bytes = std::fs::read(dir.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(sha256(&bytes), want, "{file}");
        }
    }
}
