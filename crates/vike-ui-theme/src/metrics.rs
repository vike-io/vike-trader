//! Sizes that are not text (design system spec §3.4): the corner radius, and the per-density
//! control, row, padding, gap and window-header measures. `preferences.density` picks the density.

/// How tightly the app packs its controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeDensity {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// The measures one density sets, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeDensityMetrics {
    pub control_height: f32,
    pub row_height: f32,
    pub padding: f32,
    pub gap: f32,
    pub header_height: f32,
}

impl ThemeDensity {
    pub const ALL: [ThemeDensity; 3] =
        [ThemeDensity::Compact, ThemeDensity::Normal, ThemeDensity::Comfortable];

    /// The value the `preferences.density` settings row stores: its word in
    /// `vike_model::appearance::DENSITIES`, at its place in [`ThemeDensity::ALL`] (the declaration order).
    pub fn key(self) -> &'static str {
        vike_model::appearance::DENSITIES[self as usize]
    }

    pub fn from_key(key: &str) -> Option<ThemeDensity> {
        Self::ALL.into_iter().find(|d| d.key() == key)
    }

    /// The name the Appearance screen shows for this density.
    pub fn label(self) -> &'static str {
        match self {
            ThemeDensity::Compact => "Compact",
            ThemeDensity::Normal => "Normal",
            ThemeDensity::Comfortable => "Comfortable",
        }
    }

    pub fn metrics(self) -> ThemeDensityMetrics {
        match self {
            ThemeDensity::Compact => COMPACT,
            ThemeDensity::Normal => NORMAL,
            ThemeDensity::Comfortable => COMFORTABLE,
        }
    }
}

// The three densities' measures: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[density.*]`, `[shape]` for the corner radius).
include!("generated/metrics_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_densities_are_the_ruled_table() {
        let row = |d: ThemeDensity| {
            let m = d.metrics();
            [m.control_height, m.row_height, m.padding, m.gap, m.header_height]
        };
        assert_eq!(row(ThemeDensity::Compact), [20.0, 16.0, 6.0, 4.0, 22.0]);
        assert_eq!(row(ThemeDensity::Normal), [24.0, 18.0, 8.0, 6.0, 24.0]);
        assert_eq!(row(ThemeDensity::Comfortable), [28.0, 22.0, 10.0, 8.0, 28.0]);
        assert_eq!(RADIUS, 4);
    }

    #[test]
    fn keys_round_trip() {
        for d in ThemeDensity::ALL {
            assert_eq!(ThemeDensity::from_key(d.key()), Some(d));
        }
        assert_eq!(ThemeDensity::default(), ThemeDensity::Normal);
        let keys: Vec<_> = ThemeDensity::ALL.iter().map(|d| d.key()).collect();
        assert_eq!(keys, ["compact", "normal", "comfortable"]);
    }
}
