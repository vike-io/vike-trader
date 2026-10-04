//! The seven text ROLES and the two scales behind them (design system spec §3.3). Code names a
//! role; the `preferences.text_size` setting picks which scale answers.

/// What a piece of text IS, from which its size follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRole {
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

impl TextRole {
    /// Every role, smallest first.
    pub const ALL: [TextRole; 7] = [
        TextRole::Caption,
        TextRole::Body,
        TextRole::Strong,
        TextRole::Title,
        TextRole::Heading,
        TextRole::Display,
        TextRole::Hero,
    ];
}

/// Which scale answers the roles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextSize {
    #[default]
    Standard,
    Large,
}

impl TextSize {
    pub const ALL: [TextSize; 2] = [TextSize::Standard, TextSize::Large];

    /// The value the `preferences.text_size` settings row stores.
    pub fn key(self) -> &'static str {
        match self {
            TextSize::Standard => "standard",
            TextSize::Large => "large",
        }
    }

    pub fn from_key(key: &str) -> Option<TextSize> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// The name the Appearance screen shows for this text size.
    pub fn label(self) -> &'static str {
        match self {
            TextSize::Standard => "Standard",
            TextSize::Large => "Large",
        }
    }

    /// The size, in points, of `role` on this scale.
    pub fn px(self, role: TextRole) -> f32 {
        let table = match self {
            TextSize::Standard => STANDARD,
            TextSize::Large => LARGE,
        };
        table[role as usize]
    }
}

const STANDARD: [f32; 7] = [10.0, 11.0, 12.0, 14.0, 18.0, 24.0, 40.0];
const LARGE: [f32; 7] = [11.0, 12.0, 13.0, 15.0, 19.0, 26.0, 40.0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scales_are_the_ruled_tables() {
        let s: Vec<f32> = TextRole::ALL.iter().map(|r| TextSize::Standard.px(*r)).collect();
        let l: Vec<f32> = TextRole::ALL.iter().map(|r| TextSize::Large.px(*r)).collect();
        assert_eq!(s, [10.0, 11.0, 12.0, 14.0, 18.0, 24.0, 40.0]);
        assert_eq!(l, [11.0, 12.0, 13.0, 15.0, 19.0, 26.0, 40.0]);
    }

    #[test]
    fn each_scale_grows_role_by_role() {
        for size in TextSize::ALL {
            let px: Vec<f32> = TextRole::ALL.iter().map(|r| size.px(*r)).collect();
            assert!(px.windows(2).all(|w| w[0] < w[1]), "{size:?}: {px:?}");
        }
    }

    #[test]
    fn keys_round_trip() {
        for s in TextSize::ALL {
            assert_eq!(TextSize::from_key(s.key()), Some(s));
        }
        assert_eq!(TextSize::default(), TextSize::Standard);
        let keys: Vec<_> = TextSize::ALL.iter().map(|s| s.key()).collect();
        assert_eq!(keys, ["standard", "large"]);
    }
}
