# Bundled fonts

The faces the GUI draws with (design system spec §3.3): **Inter** for words, **JetBrains Mono** for
anything read character by character — and **Phosphor**, the one icon font (spec §9), which draws
from a font family of its own (`crates/vike-ui-theme/src/icons.rs` says why). They are COMPILE
inputs — `include_bytes!` in `crates/vike-ui-theme/src/fonts.rs` — so every build carries the same
faces on every OS. They are committed rather than downloaded at build time, and
`scripts/fonts_vendor_drift.sh` re-derives them from the upstream releases below and compares them
byte for byte.

| File | Upstream | SHA-256 |
|---|---|---|
| `Inter-Regular.ttf` | Inter 4.1, `extras/ttf/Inter-Regular.ttf` | `40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82` |
| `Inter-SemiBold.ttf` | Inter 4.1, `extras/ttf/Inter-SemiBold.ttf` | `78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3` |
| `Inter-Bold.ttf` | Inter 4.1, `extras/ttf/Inter-Bold.ttf` | `288316099b1e0a47a4716d159098005eef7c0066921f34e3200393dbdb01947f` |
| `Inter-OFL.txt` | Inter 4.1, `LICENSE.txt` | `262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a` |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono 2.304, `fonts/ttf/JetBrainsMono-Regular.ttf` | `a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f` |
| `JetBrainsMono-SemiBold.ttf` | JetBrains Mono 2.304, `fonts/ttf/JetBrainsMono-SemiBold.ttf` | `1b3bfa1ed5665a4ce3f9feb68d2d4e40e70bf8b4b7d9a3edd418f321b4e166a0` |
| `JetBrainsMono-OFL.txt` | JetBrains Mono 2.304, `OFL.txt` | `30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad` |
| `Phosphor-Regular.ttf` | Phosphor 2.1.2 (`@phosphor-icons/web`), `package/src/regular/Phosphor.ttf` | `06b91e022b7ee899a63efced879392a74f0bacbda54e4467e9f663220d173a10` |
| `Phosphor-Regular.css` | Phosphor 2.1.2, `package/src/regular/style.css` | `873761b8711147dc516b6102936e9ad005f3a3015349efcde1a496f0326f1051` |
| `Phosphor-MIT.txt` | Phosphor 2.1.2, `package/LICENSE` | `687fbe52d0eb5c2353eca27a4037e889145ee0b584d3b1abf67581c4a4a4e47c` |

Archives:
- `https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip`, SHA-256 `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`;
- `https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip`, SHA-256 `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`;
- `https://registry.npmjs.org/@phosphor-icons/web/-/web-2.1.2.tgz`, SHA-256 `40e3096099ca818c047979cece6a3764944d5200c67b64092f7bcd8bcd1d2b08` (npm is Phosphor's release channel; the GitHub tag carries no assets).

The stylesheet is not compiled in. It is the only record of which name each Phosphor codepoint has,
because the TTF carries no glyph names, and `crates/vike-ui-theme/src/icons.rs`'s tests read it.

Inter and JetBrains Mono are licensed under the SIL Open Font License 1.1; Phosphor under the MIT
licence. Each licence travels beside its files. To re-check: `bash scripts/fonts_vendor_drift.sh`.
