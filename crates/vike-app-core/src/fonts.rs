//! Where the desktop shell looks for the fonts egui does not bundle.
//!
//! # The defect this module exists because of
//!
//! `vike-app`'s `install_fonts` registers system faces so the title-bar controls (`✕ ─ □ ❐ ●`) and
//! the chart-pane buttons render at all — egui's bundled face has none of those glyphs. Every
//! candidate path it tried was `C:\Windows\Fonts\…`.
//!
//! On Linux all of them failed. Not loudly: a missing font file is a `false` from the loader, the
//! family list is built from whatever succeeded, and the app carries on drawing text in egui's own
//! face. Only the SYMBOLS go missing, as tofu boxes — so a Linux user and the container image both
//! got a GUI whose window controls and chart toolbar were blank squares, and the first report of it
//! was a screenshot of the shipped `0.1.16` thin client.
//!
//! # Why the tables live HERE and not beside the loader
//!
//! Two reasons, and the second is the one that matters. The shallow one is
//! `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs`: `vike-app` is compiled by CI and run by
//! almost nothing, so data and pure logic belong one crate down. The real one is that a test in
//! `vike-app` is a test nobody executes — which is exactly how a font list could name only Windows
//! paths for as long as it did. Down here, [`some_symbol_font_exists`] runs on every PR, on Linux,
//! which is the platform that was broken.

/// A face the shell wants, as the ordered paths that might hold it.
///
/// Order is by what is INSTALLED rather than by preference: DejaVu ships with almost every
/// distribution and with this project's own container base, so it is last in each list — a
/// deliberate "always something" floor rather than a first choice.
pub struct FaceCandidates {
    /// The name the shell registers the loaded face under.
    pub name: &'static str,
    /// Paths to try, in order. The first that reads wins.
    pub paths: &'static [&'static str],
}

/// The UI face at weight 400 — the app's default proportional text.
pub const PROPORTIONAL_400: FaceCandidates = FaceCandidates {
    name: "segoe_400",
    paths: &[
        r"C:\Windows\Fonts\segoeui.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
    ],
};

/// **The one the reported defect was about**: the symbol face behind `✕ ─ □ ❐ ●` and the chart
/// pane's own buttons.
///
/// Noto Symbols 2 first because it also covers `❐` (U+2750); DejaVu last because it is the one
/// that is always present, and covering four of the five glyphs beats covering none.
pub const SYMBOLS: FaceCandidates = FaceCandidates {
    name: "seguisym",
    paths: &[
        r"C:\Windows\Fonts\seguisym.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ],
};

/// Colour emoji, for the launcher icons. Absent on most Linux boxes, and that is survivable — the
/// launchers fall back to the proportional face rather than vanishing.
pub const EMOJI: FaceCandidates = FaceCandidates {
    name: "seguiemj",
    paths: &[r"C:\Windows\Fonts\seguiemj.ttf", "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"],
};

/// The monospace face every price and table figure is drawn in.
pub const MONOSPACE: FaceCandidates = FaceCandidates {
    name: "cascadia",
    paths: &[
        r"C:\Windows\Fonts\CascadiaCode.ttf",
        r"C:\Windows\Fonts\CascadiaMono.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    ],
};

/// Bold, and the semibold/light weights the theme asks for. egui has no numeric weight axis, so
/// each is a separately-registered face.
pub const PROPORTIONAL_600: FaceCandidates = FaceCandidates {
    name: "segoe_600",
    paths: &[
        r"C:\Windows\Fonts\seguisb.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    ],
};
/// See [`PROPORTIONAL_600`].
pub const PROPORTIONAL_700: FaceCandidates = FaceCandidates {
    name: "segoe_700",
    paths: &[
        r"C:\Windows\Fonts\segoeuib.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    ],
};
/// See [`PROPORTIONAL_600`].
pub const PROPORTIONAL_300: FaceCandidates = FaceCandidates {
    name: "segoe_300",
    paths: &[r"C:\Windows\Fonts\segoeuil.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
};
/// See [`PROPORTIONAL_600`].
pub const PROPORTIONAL_350: FaceCandidates = FaceCandidates {
    name: "segoe_350",
    paths: &[r"C:\Windows\Fonts\segoeuisl.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"],
};

/// The first path in `face` that exists on this box, if any. PURE apart from the `is_file` probe,
/// which is the whole question being asked.
#[must_use]
pub fn first_existing(face: &FaceCandidates) -> Option<&'static str> {
    face.paths.iter().copied().find(|p| std::path::Path::new(p).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE guard. On the platform CI runs there must be a symbol-capable font, or the title-bar
    /// controls render as tofu boxes — silently, because a failed font load is not an error.
    ///
    /// It asserts a PATH EXISTS rather than that a glyph rasterises: reading a font file is what
    /// the shell's loader actually does, so that is the failure this can honestly reproduce.
    /// Judging the pixels needs a GPU and a human; this needs to run in the fast lane, on every PR,
    /// on the platform that was broken.
    #[test]
    fn some_symbol_font_exists() {
        assert!(
            first_existing(&SYMBOLS).is_some(),
            "no symbol-capable font on this platform. The shell will load none, and `✕ ─ □ ❐ ●` \
             plus the chart-pane buttons will render as tofu boxes. Candidates:\n  {}\n\nInstall \
             one (DejaVu is in almost every distribution and in this project's container base) or \
             add this platform's path to SYMBOLS.",
            SYMBOLS.paths.join("\n  ")
        );
    }

    /// Every face must offer a non-Windows candidate. The defect was not "a path was wrong" — it
    /// was that EVERY path named one operating system, so the whole mechanism was inert everywhere
    /// else. A new face added with only a `C:\…` path repeats it exactly.
    #[test]
    fn every_face_offers_a_path_off_windows() {
        for face in [
            &PROPORTIONAL_400,
            &PROPORTIONAL_600,
            &PROPORTIONAL_700,
            &PROPORTIONAL_300,
            &PROPORTIONAL_350,
            &SYMBOLS,
            &EMOJI,
            &MONOSPACE,
        ] {
            assert!(
                face.paths.iter().any(|p| !p.starts_with("C:")),
                "face {:?} names only Windows paths — it is inert on Linux and macOS, which is \
                 the defect this module was written for",
                face.name
            );
            assert!(!face.paths.is_empty(), "face {:?} has no candidates at all", face.name);
        }
    }

    /// A face's registered name is what the family lists reference; two faces sharing one would
    /// silently overwrite each other in egui's font map.
    #[test]
    fn face_names_are_unique() {
        let names = [
            PROPORTIONAL_400.name,
            PROPORTIONAL_600.name,
            PROPORTIONAL_700.name,
            PROPORTIONAL_300.name,
            PROPORTIONAL_350.name,
            SYMBOLS.name,
            EMOJI.name,
            MONOSPACE.name,
        ];
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "two faces share a registered name: {names:?}");
    }
}
