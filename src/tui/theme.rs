//! The palette: semantic colour tokens the client's chrome is drawn with.
//!
//! A theme is data, `doc/settings/theme` = `{"name", "palette": {token: colour}}`, written by an
//! extension (D17). The client only resolves tokens. It keeps a built-in default so it looks right
//! with no theme and no extensions; a token a theme leaves out, or gives a colour this client can't
//! read, keeps its default. Unknown tokens are ignored, so themes can carry tokens for other clients.
//!
//! Colours: a name (`cyan`, `darkgray`, `lightblue`, ...), `#RRGGBB`, a 256-colour index (a number
//! or a numeric string), or `default` for the terminal's own colour. Terminal *content* keeps the
//! colours its program chose; only chrome uses the palette.

use ratatui::style::{Color, Modifier, Style};
use serde_json::Value;

/// Where the active theme lives.
pub const ENTRY: &str = "doc/settings/theme";

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Ordinary text.
    pub text: Color,
    /// Secondary text: inactive tabs, labels, the status message, `text` rows.
    pub muted: Color,
    /// Tertiary text: details, descriptions, disabled rows, hints; the `dim` style.
    pub dim: Color,
    /// Highlights: headers, values, buttons, the scrollbar thumb, the `accent` style.
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
    /// Keyboard modes: the prompt line, the open vqro badge, the filter.
    pub prompt: Color,
    /// Borders of unfocused panes, rules, separators, the scrollbar track.
    pub border: Color,
    /// Borders of the focused pane, popups and menus.
    pub border_focus: Color,
    /// Selected row background and text. An unset `selection` means reverse video, which works on
    /// any terminal and any background.
    pub selection: Option<Color>,
    pub selection_text: Option<Color>,
    /// Fill of neutral badges and unchecked chips.
    pub badge: Color,
    /// Text drawn on a filled background: badges, the vqro badge, the active page, checked chips.
    pub badge_text: Color,
    /// Fill of plain (non-toggle) chips: a raised surface.
    pub surface: Color,
    /// Background of card blocks (`message` rows with `look: "card"`): a little lighter than the pane, below
    /// `surface`. A theme without it uses its `surface`.
    pub card: Color,
    /// The active tab in a tab bar.
    pub tab_active: Color,
    pub tab_active_text: Color,
    /// Background of the whole tab bar row: a darker band that sets the tabs apart from panes.
    pub tab_bar: Color,
}

impl Default for Palette {
    /// Today's look, in the 16 ANSI colours so it follows the terminal's own scheme.
    fn default() -> Self {
        Palette {
            text: Color::Reset,
            muted: Color::Gray,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
            prompt: Color::Magenta,
            border: Color::DarkGray,
            border_focus: Color::Cyan,
            selection: None,
            selection_text: None,
            badge: Color::DarkGray,
            badge_text: Color::Black,
            surface: Color::Rgb(40, 40, 48),
            card: Color::Rgb(32, 32, 38),
            tab_active: Color::Blue,
            tab_active_text: Color::White,
            tab_bar: Color::Indexed(235),
        }
    }
}

impl Palette {
    /// The palette for a theme entry's value (or none): the default with the theme's tokens over it.
    pub fn from_theme(theme: Option<&Value>) -> Palette {
        let mut p = Palette::default();
        let Some(tokens) = theme.and_then(|t| t["palette"].as_object()) else { return p };
        for (token, v) in tokens {
            let Some(c) = parse_color(v) else { continue };
            match token.as_str() {
                "selection" => p.selection = Some(c),
                "selection_text" => p.selection_text = Some(c),
                t => {
                    if let Some(slot) = p.slot(t) {
                        *slot = c;
                    }
                }
            }
        }
        // A theme without `card` uses its own raised surface; failing that, a card derived from the base
        // background it declares (`background`, informational); else the built-in card.
        if tokens.get("card").and_then(parse_color).is_none() {
            if tokens.get("surface").and_then(parse_color).is_some() {
                p.card = p.surface;
            } else if let Some(Color::Rgb(r, g, b)) = tokens.get("background").and_then(parse_color) {
                p.card = card_from_background(r, g, b);
            }
        }
        p
    }

    fn slot(&mut self, token: &str) -> Option<&mut Color> {
        Some(match token {
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "error" => &mut self.error,
            "prompt" => &mut self.prompt,
            "border" => &mut self.border,
            "border_focus" => &mut self.border_focus,
            "badge" => &mut self.badge,
            "badge_text" => &mut self.badge_text,
            "surface" => &mut self.surface,
            "card" => &mut self.card,
            "tab_active" => &mut self.tab_active,
            "tab_active_text" => &mut self.tab_active_text,
            "tab_bar" => &mut self.tab_bar,
            _ => return None,
        })
    }

    /// A text style by its semantic name, as extensions write it. Unknown names are plain text.
    pub fn style(&self, name: Option<&str>) -> Style {
        let fg = match name {
            Some("dim") => self.dim,
            Some("muted") => self.muted,
            Some("warn") => self.warn,
            Some("error") => self.error,
            Some("ok") => self.ok,
            Some("accent") => self.accent,
            Some("bold") => return Style::new().fg(self.text).add_modifier(Modifier::BOLD),
            _ => self.text,
        };
        Style::new().fg(fg)
    }

    /// A filled badge by semantic name; neutral when the name has no colour.
    pub fn badge(&self, name: Option<&str>) -> Style {
        let bg = match name {
            Some("warn") => self.warn,
            Some("error") => self.error,
            Some("ok") => self.ok,
            Some("accent") => self.accent,
            _ => self.badge,
        };
        let s = Style::new().bg(bg).fg(self.badge_text);
        if name == Some("bold") { s.add_modifier(Modifier::BOLD) } else { s }
    }

    /// The selected row: the theme's selection colours, or reverse video.
    pub fn selected(&self) -> Style {
        match (self.selection, self.selection_text) {
            (Some(bg), Some(fg)) => Style::new().bg(bg).fg(fg),
            (Some(bg), None) => Style::new().bg(bg),
            // `selection_text` alone means nothing under reverse video.
            (None, _) => Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    /// A pane or popup border.
    pub fn border(&self, focused: bool) -> Style {
        Style::new().fg(if focused { self.border_focus } else { self.border })
    }
}

/// A card for a theme that declares only its base background: 5% of the way toward white on a dark background,
/// toward black on a light one (core-protocol.md, Themes).
pub fn card_from_background(r: u8, g: u8, b: u8) -> Color {
    // Integer weights and half-up rounding are identical in both clients, including threshold ties.
    let light = 2126 * r as u32 + 7152 * g as u32 + 722 * b as u32 >= 1_275_000;
    let step = |c: u8| ((19 * c as u32 + if light { 0 } else { 255 } + 10) / 20) as u8;
    Color::Rgb(step(r), step(g), step(b))
}

/// A colour value: name, `#RRGGBB`, 256-colour index (number or numeric string), or `default`.
pub fn parse_color(v: &Value) -> Option<Color> {
    if let Some(n) = v.as_u64() {
        return u8::try_from(n).ok().map(Color::Indexed);
    }
    let s = v.as_str()?.trim();
    if s.eq_ignore_ascii_case("default") {
        return Some(Color::Reset);
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colours_in_every_form() {
        assert_eq!(parse_color(&json!("cyan")), Some(Color::Cyan));
        assert_eq!(parse_color(&json!("Dark-Gray")), Some(Color::DarkGray));
        assert_eq!(parse_color(&json!("#1a2B3c")), Some(Color::Rgb(0x1a, 0x2b, 0x3c)));
        assert_eq!(parse_color(&json!(208)), Some(Color::Indexed(208)));
        assert_eq!(parse_color(&json!("208")), Some(Color::Indexed(208)));
        assert_eq!(parse_color(&json!("default")), Some(Color::Reset));
        assert_eq!(parse_color(&json!(256)), None);
        assert_eq!(parse_color(&json!("#12345")), None);
        assert_eq!(parse_color(&json!("chartreuse")), None);
        assert_eq!(parse_color(&json!({"r": 1})), None);
    }

    #[test]
    fn shared_card_background_cases() {
        let cases: Value = serde_json::from_str(include_str!("../../tests/fixtures/client/cardcolor.json")).unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let Color::Rgb(r, g, b) = parse_color(&case["background"]).unwrap() else { panic!("RGB fixture required") };
            assert_eq!(card_from_background(r, g, b), parse_color(&case["expect"]).unwrap(), "{}", case["name"]);
        }
    }

    #[test]
    fn card_is_a_token_and_falls_back_to_the_themes_surface() {
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"card": "#101010", "surface": "#202020"}}))).card, Color::Rgb(16, 16, 16));
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"surface": "#202020"}}))).card, Color::Rgb(32, 32, 32));
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"accent": "red"}}))).card, Palette::default().card);
        // `default` is the terminal's own colour, for `card` and for a `surface` that cards follow.
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"card": "default"}}))).card, Color::Reset);
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"surface": "default"}}))).card, Color::Reset);
        // An unreadable card counts as missing.
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"card": "chartreuse", "surface": "#202020"}}))).card, Color::Rgb(32, 32, 32));
        // Neither card nor surface: derived from a declared background (lighter on dark, darker on light), else built in.
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"background": "#000000"}}))).card, Color::Rgb(13, 13, 13));
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"background": "#ffffff"}}))).card, Color::Rgb(242, 242, 242));
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"background": "default"}}))).card, Palette::default().card);
        assert_eq!(Palette::from_theme(Some(&json!({"palette": {"background": "#000000", "surface": "#303030"}}))).card, Color::Rgb(48, 48, 48));
        assert_eq!(card_from_background(0x1e, 0x1e, 0x2e), Color::Rgb(0x29, 0x29, 0x38), "the same as OpenTUI's cardcolor.test.ts");
    }

    #[test]
    fn no_theme_is_the_default() {
        assert_eq!(Palette::from_theme(None), Palette::default());
        assert_eq!(Palette::from_theme(Some(&json!("nonsense"))), Palette::default());
        assert_eq!(Palette::from_theme(Some(&json!({"name": "x"}))), Palette::default());
    }

    #[test]
    fn theme_tokens_override_and_bad_ones_fall_back() {
        let p = Palette::from_theme(Some(&json!({"name": "t", "palette": {
            "accent": "#ff0000", "ok": 34, "warn": "not a colour", "future_token": "red", "selection": "blue"
        }})));
        let d = Palette::default();
        assert_eq!(p.accent, Color::Rgb(255, 0, 0));
        assert_eq!(p.ok, Color::Indexed(34));
        assert_eq!(p.warn, d.warn);
        assert_eq!(p.error, d.error);
        assert_eq!(p.selection, Some(Color::Blue));
        assert_eq!(p.style(Some("accent")).fg, Some(Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn every_token_is_themable() {
        let tokens = ["text", "muted", "dim", "accent", "ok", "warn", "error", "prompt", "border", "border_focus", "badge", "badge_text", "surface", "tab_active", "tab_active_text", "tab_bar"];
        let palette: serde_json::Map<String, Value> = tokens.iter().map(|t| (t.to_string(), json!(1))).collect();
        let mut p = Palette::from_theme(Some(&json!({"palette": palette})));
        for t in tokens {
            assert_eq!(*p.slot(t).unwrap(), Color::Indexed(1), "{t}");
        }
    }

    #[test]
    fn selection_defaults_to_reverse_video() {
        let d = Palette::default();
        assert!(d.selected().add_modifier.contains(Modifier::REVERSED));
        let p = Palette::from_theme(Some(&json!({"palette": {"selection": "blue", "selection_text": "white"}})));
        assert_eq!((p.selected().bg, p.selected().fg), (Some(Color::Blue), Some(Color::White)));
        assert!(!p.selected().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn style_names() {
        let d = Palette::default();
        assert_eq!(d.style(Some("dim")).fg, Some(Color::DarkGray));
        assert_eq!(d.style(Some("unknown")), Style::new().fg(Color::Reset));
        assert!(d.style(Some("bold")).add_modifier.contains(Modifier::BOLD));
        assert_eq!(d.badge(Some("error")).bg, Some(Color::Red));
        assert_eq!(d.badge(None).bg, Some(Color::DarkGray));
    }
}
