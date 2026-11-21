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
    /// Focus and selection: the focused border, the active page or chip, the scrollbar thumb, the `accent` style.
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
    /// Meaning roles (core-protocol.md, Themes): one meaning each, named by extensions as styles. A theme that
    /// leaves one out but sets its fallback role (`ROLES`) uses that; otherwise the built-in value below.
    /// Structure: section headers, markdown headings, popup titles.
    pub heading: Color,
    /// You can act on it: buttons, chips, selects, ▸ expanders, row actions.
    pub control: Color,
    /// It opens something: paths, URLs; always drawn underlined.
    pub link: Color,
    /// In progress, neutral state: a working agent, a running tool.
    pub info: Color,
    /// Written by you.
    pub you: Color,
    /// The agent of this pane.
    pub agent: Color,
    /// Another agent.
    pub other_agent: Color,
    /// Uncommitted work (git).
    pub changed: Color,
}

/// The meaning roles and the role each falls back to when a theme leaves it out.
pub const ROLES: [(&str, &str); 8] = [("heading", "accent"), ("control", "accent"), ("link", "accent"), ("info", "accent"),
    ("you", "accent"), ("agent", "muted"), ("other_agent", "accent"), ("changed", "warn")];

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
            heading: Color::Cyan,
            control: Color::LightBlue,
            link: Color::LightBlue,
            info: Color::LightCyan,
            you: Color::LightYellow,
            agent: Color::LightMagenta,
            other_agent: Color::LightGreen,
            changed: Color::Yellow,
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
        // A meaning role the theme leaves out follows the theme's own fallback role when it sets that one, so a
        // theme written before the roles (or one that only sets `accent`) stays one coherent palette.
        for (role, fallback) in ROLES {
            if tokens.get(role).and_then(parse_color).is_none() && tokens.get(fallback).and_then(parse_color).is_some() {
                let c = *p.slot(fallback).expect("fallback is a palette token");
                *p.slot(role).expect("role is a palette token") = c;
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
            "heading" => &mut self.heading,
            "control" => &mut self.control,
            "link" => &mut self.link,
            "info" => &mut self.info,
            "you" => &mut self.you,
            "agent" => &mut self.agent,
            "other_agent" => &mut self.other_agent,
            "changed" => &mut self.changed,
            _ => return None,
        })
    }

    /// The colour of a semantic style name that has one (not `bold`, `plain` or an unknown name).
    fn named(&self, name: &str) -> Option<Color> {
        Some(match name {
            "dim" => self.dim,
            "muted" => self.muted,
            "warn" => self.warn,
            "error" => self.error,
            "ok" => self.ok,
            "accent" => self.accent,
            "heading" => self.heading,
            "control" => self.control,
            "link" => self.link,
            "info" => self.info,
            "you" => self.you,
            "agent" => self.agent,
            "other_agent" => self.other_agent,
            "changed" => self.changed,
            _ => return None,
        })
    }

    /// A text style by its semantic name, as extensions write it. Unknown names are plain text; `link` is also
    /// underlined, so it reads as a link without colour.
    pub fn style(&self, name: Option<&str>) -> Style {
        match name {
            Some("bold") => Style::new().fg(self.text).add_modifier(Modifier::BOLD),
            Some("link") => Style::new().fg(self.link).add_modifier(Modifier::UNDERLINED),
            Some(n) => Style::new().fg(self.named(n).unwrap_or(self.text)),
            None => Style::new().fg(self.text),
        }
    }

    /// A filled badge by semantic name; neutral when the name has no colour.
    pub fn badge(&self, name: Option<&str>) -> Style {
        let bg = match name.and_then(|n| self.named(n)) {
            Some(c) if !matches!(name, Some("dim" | "muted")) => c,
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
        let tokens = ["text", "muted", "dim", "accent", "ok", "warn", "error", "prompt", "border", "border_focus", "badge", "badge_text", "surface", "tab_active", "tab_active_text", "tab_bar",
            "heading", "control", "link", "info", "you", "agent", "other_agent", "changed"];
        let palette: serde_json::Map<String, Value> = tokens.iter().map(|t| (t.to_string(), json!(1))).collect();
        let mut p = Palette::from_theme(Some(&json!({"palette": palette})));
        for t in tokens {
            assert_eq!(*p.slot(t).unwrap(), Color::Indexed(1), "{t}");
        }
    }

    #[test]
    fn meaning_roles_fall_back_to_the_themes_own_role_else_the_built_in() {
        let d = Palette::default();
        // An older theme (no roles) is one coherent palette: each role follows its fallback role.
        let p = Palette::from_theme(Some(&json!({"palette": {"accent": "#ff0000", "muted": "#00ff00", "warn": "#0000ff"}})));
        for (role, fallback) in ROLES {
            let (mut q, mut r) = (p.clone(), p.clone());
            assert_eq!(*q.slot(role).unwrap(), *r.slot(fallback).unwrap(), "{role} -> {fallback}");
        }
        // A role the theme sets wins; a fallback the theme leaves out keeps the built-in role.
        let p = Palette::from_theme(Some(&json!({"palette": {"accent": "#ff0000", "heading": "#123456"}})));
        assert_eq!(p.heading, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(p.control, Color::Rgb(255, 0, 0));
        assert_eq!(p.agent, d.agent, "muted not set: built-in agent");
        assert_eq!(p.changed, d.changed, "warn not set: built-in changed");
        // An empty palette (the bundled `default` theme) is the built-in look, roles included.
        assert_eq!(Palette::from_theme(Some(&json!({"name": "default", "palette": {}}))), d);
        // The built-in roles are distinct where meaning depends on it.
        let ids = [d.you, d.agent, d.other_agent, d.accent];
        for (i, a) in ids.iter().enumerate() { for b in &ids[i + 1..] { assert_ne!(a, b); } }
    }

    #[test]
    fn shared_role_cases() {
        let f: Value = serde_json::from_str(include_str!("../../tests/fixtures/client/roles.json")).unwrap();
        let roles: serde_json::Map<String, Value> = ROLES.iter().map(|(r, b)| (r.to_string(), json!(b))).collect();
        assert_eq!(f["roles"], Value::Object(roles));
        let mut d = Palette::default();
        for (role, want) in f["builtin"].as_object().unwrap() {
            assert_eq!(*d.slot(role).unwrap(), parse_color(want).unwrap(), "built-in {role}");
        }
        for case in f["cases"].as_array().unwrap() {
            let mut p = Palette::from_theme(Some(&json!({"palette": case["palette"]})));
            for (role, want) in case["expect"].as_object().unwrap() {
                assert_eq!(*p.slot(role).unwrap(), parse_color(want).unwrap(), "{}: {role}", case["name"]);
            }
        }
    }

    #[test]
    fn meaning_roles_are_style_and_badge_names_and_links_are_underlined() {
        let d = Palette::default();
        for (role, _) in ROLES {
            let mut q = d.clone();
            let c = *q.slot(role).unwrap();
            assert_eq!(d.style(Some(role)).fg, Some(c), "{role}");
            assert_eq!(d.badge(Some(role)).bg, Some(c), "{role}");
        }
        assert!(d.style(Some("link")).add_modifier.contains(Modifier::UNDERLINED));
        assert!(!d.style(Some("control")).add_modifier.contains(Modifier::UNDERLINED));
        // Unknown names stay plain; dim and muted badges stay neutral, as before.
        assert_eq!(d.style(Some("sparkly")).fg, Some(d.text));
        assert_eq!(d.badge(Some("dim")).bg, Some(d.badge));
        assert_eq!(d.badge(Some("sparkly")).bg, Some(d.badge));
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
