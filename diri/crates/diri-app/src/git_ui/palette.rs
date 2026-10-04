// Adapted from Ely GPUI Components (MIT OR Apache-2.0), https://github.com/ZacharyZhang-NY/Ely-GPUI-Components
//
//! Colors the review paints with.
//!
//! Ely washes a changed line in its success or danger tone at a low alpha and
//! its changed words at a stronger one. Diri takes those tones from the
//! terminal theme's own ANSI green and red, so a diff reads like the agent's
//! terminal beside it and keeps working when the panel sits directly on the
//! terminal background. Every wash is translucent: it adapts to whatever
//! surface the panel is painted on, light or dark.

use diri_term::theme::{TermTheme, ThemeAppearance};
use diri_ui::SemanticColors;
use gpui::Rgba;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DiffPalette {
    /// Code and primary labels.
    pub text: Rgba,
    /// Paths, subjects, secondary labels.
    pub secondary: Rgba,
    /// Line numbers, metadata, `@@` ranges.
    pub muted: Rgba,
    /// Hairlines between files and around controls.
    pub rule: Rgba,
    /// Signs and counts for added lines.
    pub added: Rgba,
    /// Signs and counts for removed lines.
    pub removed: Rgba,
    /// Renames, selection edges, the Ask action.
    pub accent: Rgba,
    /// Modified files.
    pub modified: Rgba,
    pub added_line: Rgba,
    pub added_word: Rgba,
    pub added_gutter: Rgba,
    pub removed_line: Rgba,
    pub removed_word: Rgba,
    pub removed_gutter: Rgba,
    /// Hunk header band.
    pub hunk: Rgba,
    /// File header band.
    pub file: Rgba,
    /// The missing half of a split row (a pure addition or removal).
    pub empty_side: Rgba,
    pub selection: Rgba,
    pub selection_edge: Rgba,
    pub hover: Rgba,
    /// Subtle control fill (segmented tracks, buttons).
    pub control: Rgba,
    /// The raised segment of a segmented control.
    pub surface_lift: Rgba,
    /// The surface the review is painted on (the terminal background).
    pub surface: Rgba,
    /// Commit graph lane colors.
    pub lanes: [Rgba; 6],
}

impl DiffPalette {
    #[must_use]
    pub(crate) fn new(colors: SemanticColors, theme: &TermTheme) -> Self {
        let light = theme.appearance == ThemeAppearance::Light;
        let green = theme.ansi[2];
        let red = theme.ansi[1];
        let blue = theme.ansi[4];
        let (line, word, gutter) = if light {
            (0.11, 0.26, 0.16)
        } else {
            (0.12, 0.30, 0.17)
        };
        let selection = theme.selection.alpha(theme.selection.a.clamp(0.24, 0.40));
        let hunk = blue.alpha(if light { 0.07 } else { 0.09 });
        // Code must stay readable under every wash it can sit on, on both the
        // terminal background and the panel's own surface. Themes whose
        // foreground is already soft (Solarized) are nudged toward black or
        // white just enough to get there.
        let bases = [
            opaque(theme.background),
            composite(colors.sidebar_surface(), opaque(colors.background)),
        ];
        let surfaces: Vec<Rgba> = bases
            .iter()
            .flat_map(|base| {
                [
                    *base,
                    composite(green.alpha(word), *base),
                    composite(red.alpha(word), *base),
                    composite(selection, *base),
                    composite(hunk, *base),
                ]
            })
            .collect();
        let text = readable(opaque(colors.primary), &surfaces, light);
        let secondary = readable(mix(text, theme.background, 0.24), &surfaces, light);
        // Bright ANSI hues are tuned for text on the terminal background; on a
        // light theme they are pulled toward the foreground so +/− counts and
        // signs stay legible at the review's small sizes.
        let ink = |hue: Rgba| if light { mix(hue, text, 0.35) } else { hue };
        Self {
            text,
            secondary,
            muted: mix(text, theme.background, if light { 0.40 } else { 0.50 }),
            rule: text.alpha(if light { 0.10 } else { 0.08 }),
            added: ink(green),
            removed: ink(red),
            accent: ink(blue),
            modified: ink(theme.ansi[3]),
            added_line: green.alpha(line),
            added_word: green.alpha(word),
            added_gutter: green.alpha(gutter),
            removed_line: red.alpha(line),
            removed_word: red.alpha(word),
            removed_gutter: red.alpha(gutter),
            hunk,
            file: text.alpha(if light { 0.035 } else { 0.04 }),
            empty_side: text.alpha(if light { 0.03 } else { 0.025 }),
            selection,
            selection_edge: ink(blue),
            hover: text.alpha(if light { 0.045 } else { 0.05 }),
            control: text.alpha(if light { 0.05 } else { 0.06 }),
            surface_lift: if light {
                mix(theme.background, WHITE, 0.6)
            } else {
                text.alpha(0.13)
            },
            surface: opaque(theme.background),
            lanes: [
                ink(theme.ansi[12]),
                ink(theme.ansi[13]),
                ink(theme.ansi[14]),
                ink(theme.ansi[11]),
                ink(theme.ansi[10]),
                ink(theme.ansi[9]),
            ],
        }
    }
}

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const BLACK: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
/// Body text contrast the review holds on every surface it paints.
const READABLE: f32 = 4.6;

/// `ink`, moved toward black (light themes) or white (dark themes) in small
/// steps until it reaches [`READABLE`] on every one of `surfaces`.
fn readable(ink: Rgba, surfaces: &[Rgba], light: bool) -> Rgba {
    let toward = if light { BLACK } else { WHITE };
    (0..=12)
        .map(|step| mix(ink, toward, step as f32 * 0.07))
        .find(|candidate| {
            surfaces
                .iter()
                .all(|surface| contrast(*candidate, *surface) >= READABLE)
        })
        .unwrap_or_else(|| mix(ink, toward, 0.84))
}

fn opaque(color: Rgba) -> Rgba {
    Rgba { a: 1.0, ..color }
}

fn mix(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let keep = 1.0 - amount;
    Rgba {
        r: from.r * keep + to.r * amount,
        g: from.g * keep + to.g * amount,
        b: from.b * keep + to.b * amount,
        a: from.a,
    }
}

/// `foreground` painted over `background`.
pub(crate) fn composite(foreground: Rgba, background: Rgba) -> Rgba {
    let alpha = foreground.a + background.a * (1.0 - foreground.a);
    if alpha == 0.0 {
        return Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    Rgba {
        r: (foreground.r * foreground.a + background.r * background.a * (1.0 - foreground.a))
            / alpha,
        g: (foreground.g * foreground.a + background.g * background.a * (1.0 - foreground.a))
            / alpha,
        b: (foreground.b * foreground.a + background.b * background.a * (1.0 - foreground.a))
            / alpha,
        a: alpha,
    }
}

/// WCAG contrast ratio of two opaque colors.
pub(crate) fn contrast(left: Rgba, right: Rgba) -> f32 {
    fn luminance(color: Rgba) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.03928 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }
    let (left, right) = (luminance(left), luminance(right));
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Code on a changed line, even under its changed-word wash, keeps nearly
    /// the contrast the theme's own text has on the terminal background, on
    /// every catalog theme. The panel is expected to sit on that background.
    #[test]
    fn changed_lines_keep_the_themes_own_text_contrast() {
        for theme in TermTheme::CATALOG {
            let colors = crate::app_theme::sidebar_colors(theme.id);
            let palette = DiffPalette::new(colors, &theme);
            let surface = theme.background;
            let plain = contrast(composite(palette.text, surface), surface);
            for wash in [
                palette.added_line,
                palette.removed_line,
                palette.added_word,
                palette.removed_word,
                palette.hunk,
                palette.selection,
            ] {
                let behind = composite(wash, surface);
                let ratio = contrast(composite(palette.text, behind), behind);
                assert!(
                    ratio >= (plain * 0.62).min(4.5),
                    "{}: {ratio:.2} under a wash vs {plain:.2} plain",
                    theme.id
                );
            }
            for sign in [palette.added, palette.removed] {
                let ratio = contrast(composite(sign, surface), surface);
                assert!(ratio >= 2.6, "{}: sign contrast {ratio:.2}", theme.id);
            }
        }
    }

    #[test]
    fn washes_are_translucent_so_they_follow_the_panel_surface() {
        let theme = TermTheme::CATALOG[0];
        let palette = DiffPalette::new(crate::app_theme::sidebar_colors(theme.id), &theme);
        for wash in [
            palette.added_line,
            palette.removed_line,
            palette.hunk,
            palette.file,
            palette.empty_side,
        ] {
            assert!(wash.a < 0.5);
        }
        assert!(palette.added_word.a > palette.added_line.a);
    }
}
