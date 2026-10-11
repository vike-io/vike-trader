//! Sizes that are not text (design system spec §3.4): the corner radius, and the per-density
//! control, row, padding, gap and window-header measures. `preferences.density` picks the density.

/// How tightly the app packs its controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// The measures one density sets, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub control_h: f32,
    pub row_h: f32,
    pub pad: f32,
    pub gap: f32,
    pub header_h: f32,
}

impl Density {
    pub const ALL: [Density; 3] = [Density::Compact, Density::Normal, Density::Comfortable];

    /// The value the `preferences.density` settings row stores.
    pub fn key(self) -> &'static str {
        match self {
            Density::Compact => "compact",
            Density::Normal => "normal",
            Density::Comfortable => "comfortable",
        }
    }

    pub fn from_key(key: &str) -> Option<Density> {
        Self::ALL.into_iter().find(|d| d.key() == key)
    }

    /// The name the Appearance screen shows for this density.
    pub fn label(self) -> &'static str {
        match self {
            Density::Compact => "Compact",
            Density::Normal => "Normal",
            Density::Comfortable => "Comfortable",
        }
    }

    pub fn metrics(self) -> Metrics {
        match self {
            Density::Compact => COMPACT,
            Density::Normal => NORMAL,
            Density::Comfortable => COMFORTABLE,
        }
    }
}

// The three densities' measures: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[density.*]`, `[shape]` for the corner radius).
include!("metrics_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_densities_are_the_ruled_table() {
        let row = |d: Density| {
            let m = d.metrics();
            [m.control_h, m.row_h, m.pad, m.gap, m.header_h]
        };
        assert_eq!(row(Density::Compact), [20.0, 16.0, 6.0, 4.0, 22.0]);
        assert_eq!(row(Density::Normal), [24.0, 18.0, 8.0, 6.0, 24.0]);
        assert_eq!(row(Density::Comfortable), [28.0, 22.0, 10.0, 8.0, 28.0]);
        assert_eq!(RADIUS, 4);
    }

    #[test]
    fn keys_round_trip() {
        for d in Density::ALL {
            assert_eq!(Density::from_key(d.key()), Some(d));
        }
        assert_eq!(Density::default(), Density::Normal);
        let keys: Vec<_> = Density::ALL.iter().map(|d| d.key()).collect();
        assert_eq!(keys, ["compact", "normal", "comfortable"]);
    }
}
