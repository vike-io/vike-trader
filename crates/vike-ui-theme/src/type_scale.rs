//! The seven text ROLES and the two scales behind them (design system spec §3.3). Code names a
//! role; the `preferences.text_size` setting picks which scale answers.

/// What a piece of text IS, from which its size follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeTextRole {
    /// Axis labels, column headers, units, timestamps.
    Caption,
    /// Tables, ladders, most text.
    Body,
    /// Buttons, tabs, menus.
    Strong,
    /// Section and panel headings, window titles.
    Title,
    /// Dialog titles.
    Heading,
    /// Balance, day PnL.
    Display,
    /// The Polymarket cockpit countdown.
    Hero,
}

impl ThemeTextRole {
    /// Every role, smallest first.
    pub const ALL: [ThemeTextRole; 7] = [
        ThemeTextRole::Caption,
        ThemeTextRole::Body,
        ThemeTextRole::Strong,
        ThemeTextRole::Title,
        ThemeTextRole::Heading,
        ThemeTextRole::Display,
        ThemeTextRole::Hero,
    ];
}

/// Which scale answers the roles. Three, since 2026-10-05: Small is what Standard was and Standard
/// is what Large was (the owner found both too small on a 2560 x 1600 screen), and Large is a new,
/// bigger scale. Layout thresholds are calibrated at Small, the smallest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeTextSize {
    Small,
    #[default]
    Standard,
    Large,
}

impl ThemeTextSize {
    pub const ALL: [ThemeTextSize; 3] =
        [ThemeTextSize::Small, ThemeTextSize::Standard, ThemeTextSize::Large];

    /// The value the `preferences.text_size` settings row stores: its word in
    /// `vike_model::appearance::TEXT_SIZES`, at its place in [`ThemeTextSize::ALL`] (the declaration order).
    pub fn key(self) -> &'static str {
        vike_model::appearance::TEXT_SIZES[self as usize]
    }

    pub fn from_key(key: &str) -> Option<ThemeTextSize> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// The name the Appearance screen shows for this text size.
    pub fn label(self) -> &'static str {
        match self {
            ThemeTextSize::Small => "Small",
            ThemeTextSize::Standard => "Standard",
            ThemeTextSize::Large => "Large",
        }
    }

    /// The size, in points, of `role` on this scale.
    pub fn px(self, role: ThemeTextRole) -> f32 {
        let table = match self {
            ThemeTextSize::Small => SMALL,
            ThemeTextSize::Standard => STANDARD,
            ThemeTextSize::Large => LARGE,
        };
        table[role as usize]
    }
}

// The three scales: GENERATED from `crates/vike-ui-theme/ui-theme.toml` (`[type.small]`, `[type.standard]`,
// `[type.large]`).
include!("generated/type_scale_tokens.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scales_are_the_ruled_tables() {
        let of = |size: ThemeTextSize| -> Vec<f32> {
            ThemeTextRole::ALL.iter().map(|r| size.px(*r)).collect()
        };
        assert_eq!(of(ThemeTextSize::Small), [10.0, 11.0, 12.0, 14.0, 18.0, 24.0, 40.0]);
        assert_eq!(of(ThemeTextSize::Standard), [11.0, 12.0, 13.0, 15.0, 19.0, 26.0, 40.0]);
        assert_eq!(of(ThemeTextSize::Large), [13.0, 14.0, 15.0, 17.0, 22.0, 30.0, 44.0]);
    }

    #[test]
    fn each_scale_grows_role_by_role() {
        for size in ThemeTextSize::ALL {
            let px: Vec<f32> = ThemeTextRole::ALL.iter().map(|r| size.px(*r)).collect();
            assert!(px.windows(2).all(|w| w[0] < w[1]), "{size:?}: {px:?}");
        }
    }

    #[test]
    fn keys_round_trip() {
        for s in ThemeTextSize::ALL {
            assert_eq!(ThemeTextSize::from_key(s.key()), Some(s));
        }
        assert_eq!(ThemeTextSize::default(), ThemeTextSize::Standard);
        let keys: Vec<_> = ThemeTextSize::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(keys, ["small", "standard", "large"]);
    }
}
