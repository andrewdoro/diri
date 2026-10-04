//! Syntax and editor colors drawn from the terminal theme, so code in the
//! Files surface reads in the same palette as the agent's terminal beside it.

use diri_term::theme::{TermTheme, ThemeAppearance};
use gpui::Rgba;

use super::syntax::TokenKind;

/// Every color the editor paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorPalette {
    pub background: Rgba,
    pub foreground: Rgba,
    pub comment: Rgba,
    pub string: Rgba,
    pub number: Rgba,
    pub keyword: Rgba,
    pub function: Rgba,
    pub type_: Rgba,
    pub constant: Rgba,
    pub attribute: Rgba,
    pub property: Rgba,
    pub tag: Rgba,
    pub punctuation: Rgba,
    /// Bracket colors by depth, cycling.
    pub rainbow: [Rgba; 3],
    pub cursor: Rgba,
    pub selection: Rgba,
    pub selection_unfocused: Rgba,
    pub find_match: Rgba,
    pub find_current: Rgba,
    pub occurrence: Rgba,
    pub current_line: Rgba,
    pub line_number: Rgba,
    pub line_number_current: Rgba,
    pub guide: Rgba,
    pub guide_active: Rgba,
    pub ruler: Rgba,
    pub bracket_match: Rgba,
    pub added: Rgba,
    pub modified: Rgba,
    pub deleted: Rgba,
    pub warning: Rgba,
    pub light: bool,
}

/// Body text holds this contrast against the background at least.
const TEXT_CONTRAST: f32 = 4.5;

impl EditorPalette {
    pub fn from_theme(theme: &TermTheme) -> Self {
        let background = opaque(theme.background);
        let foreground = opaque(theme.foreground);
        let light = theme.appearance == ThemeAppearance::Light;
        let ansi = theme.ansi;
        let readable = |color: Rgba| readable(color, foreground, background, TEXT_CONTRAST);
        // ANSI slots: 1 red, 2 green, 3 yellow, 4 blue, 5 magenta, 6 cyan,
        // 8 bright black; the bright set reads better on dark backgrounds.
        let slot = |normal: usize| {
            if light {
                ansi[normal]
            } else {
                ansi[normal + 8]
            }
        };
        let comment = readable(mix(ansi[8], foreground, 0.15));
        Self {
            background,
            foreground,
            comment,
            string: readable(slot(2)),
            number: readable(slot(3)),
            keyword: readable(slot(5)),
            function: readable(slot(4)),
            type_: readable(slot(6)),
            constant: readable(slot(3)),
            attribute: readable(mix(slot(6), foreground, 0.25)),
            property: readable(mix(slot(4), foreground, 0.35)),
            tag: readable(slot(1)),
            punctuation: with_alpha(foreground, 0.72),
            rainbow: [readable(slot(3)), readable(slot(5)), readable(slot(4))],
            cursor: opaque(theme.cursor),
            selection: with_alpha(theme.selection, theme.selection.a.max(0.32)),
            selection_unfocused: with_alpha(theme.selection, theme.selection.a.max(0.32) * 0.55),
            find_match: with_alpha(theme.find_match, theme.find_match.a.clamp(0.22, 0.40)),
            find_current: with_alpha(
                theme.find_match_current,
                theme.find_match_current.a.clamp(0.40, 0.65),
            ),
            occurrence: with_alpha(foreground, if light { 0.08 } else { 0.10 }),
            current_line: with_alpha(foreground, if light { 0.035 } else { 0.045 }),
            line_number: with_alpha(foreground, 0.30),
            line_number_current: with_alpha(foreground, 0.78),
            guide: with_alpha(foreground, 0.08),
            guide_active: with_alpha(foreground, 0.22),
            ruler: with_alpha(foreground, 0.07),
            bracket_match: with_alpha(foreground, 0.16),
            added: opaque(slot(2)),
            modified: opaque(slot(4)),
            deleted: opaque(slot(1)),
            warning: opaque(slot(3)),
            light,
        }
    }

    pub fn token(&self, kind: TokenKind) -> Rgba {
        match kind {
            TokenKind::Comment => self.comment,
            TokenKind::String => self.string,
            TokenKind::Number => self.number,
            TokenKind::Keyword => self.keyword,
            TokenKind::Type => self.type_,
            TokenKind::Function => self.function,
            TokenKind::Constant => self.constant,
            TokenKind::Attribute => self.attribute,
            TokenKind::Property => self.property,
            TokenKind::Tag => self.tag,
            TokenKind::Bracket | TokenKind::Punctuation => self.punctuation,
        }
    }

    pub fn rainbow(&self, depth: usize) -> Rgba {
        self.rainbow[depth % self.rainbow.len()]
    }
}

fn opaque(color: Rgba) -> Rgba {
    Rgba { a: 1.0, ..color }
}

fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

pub(crate) fn mix(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let keep = 1.0 - amount;
    Rgba {
        r: from.r * keep + to.r * amount,
        g: from.g * keep + to.g * amount,
        b: from.b * keep + to.b * amount,
        a: 1.0,
    }
}

fn luminance(color: Rgba) -> f32 {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.039_28 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

/// WCAG contrast between two opaque colors.
pub(crate) fn contrast(left: Rgba, right: Rgba) -> f32 {
    let (left, right) = (luminance(left), luminance(right));
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

/// `color`, drawn toward the foreground just far enough to read.
fn readable(color: Rgba, foreground: Rgba, background: Rgba, target: f32) -> Rgba {
    let color = opaque(color);
    let goal = target.min(contrast(foreground, background));
    for step in 0..=20 {
        let candidate = mix(color, foreground, step as f32 / 20.0);
        if contrast(candidate, background) >= goal {
            return candidate;
        }
    }
    foreground
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_theme_keeps_tokens_readable() {
        for theme in TermTheme::CATALOG {
            let palette = EditorPalette::from_theme(&theme);
            let floor = TEXT_CONTRAST.min(contrast(palette.foreground, palette.background)) - 0.01;
            for (role, color) in [
                ("comment", palette.comment),
                ("string", palette.string),
                ("number", palette.number),
                ("keyword", palette.keyword),
                ("function", palette.function),
                ("type", palette.type_),
                ("tag", palette.tag),
                ("rainbow", palette.rainbow[0]),
                ("rainbow", palette.rainbow[1]),
                ("rainbow", palette.rainbow[2]),
            ] {
                assert!(
                    contrast(color, palette.background) >= floor,
                    "{role} in {} reads at {:.2}",
                    theme.id,
                    contrast(color, palette.background)
                );
            }
        }
    }

    #[test]
    fn the_palette_follows_the_terminal_theme() {
        let dracula = EditorPalette::from_theme(&TermTheme::DRACULA);
        let nord = EditorPalette::from_theme(&TermTheme::NORD);
        assert_eq!(dracula.background, opaque(TermTheme::DRACULA.background));
        assert_ne!(dracula.keyword, nord.keyword);
        assert!(EditorPalette::from_theme(&TermTheme::GITHUB_LIGHT).light);
        assert_ne!(dracula.rainbow(0), dracula.rainbow(1));
        assert_eq!(dracula.rainbow(3), dracula.rainbow(0));
    }
}
