"""Bundled semantic colours stay readable on their published base backgrounds."""
from pathlib import Path
import runpy
import sys
import types
import unittest
from unittest import mock


ROLES = ("heading", "control", "link", "info", "you", "agent", "other_agent", "changed")
# Published base background and ordinary foreground, not the terminal's configurable defaults.
BASE = {
    # Primer light: white / black. https://github.com/primer/primitives/blob/v7.15.0/src/tokens/base/color/light/light.json5
    "light": ("#ffffff", "#1f2328"),
    # Nord: nord0 / nord4. https://www.nordtheme.com/docs/colors-and-palettes
    "nord": ("#2e3440", "#d8dee9"),
    # Gruvbox dark: bg0 / fg1. https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim
    "gruvbox": ("#282828", "#ebdbb2"),
    # Catppuccin: base / text, all four flavours. https://catppuccin.com/palette
    "catppuccin-mocha": ("#1e1e2e", "#cdd6f4"),
    "catppuccin-macchiato": ("#24273a", "#cad3f5"),
    "catppuccin-frappe": ("#303446", "#c6d0f5"),
    "catppuccin-latte": ("#eff1f5", "#4c4f69"),
    # Dracula: background / foreground. https://draculatheme.com/contribute
    "dracula": ("#282a36", "#f8f8f2"),
    # Tokyo Night: night bg / storm fg. https://github.com/folke/tokyonight.nvim/tree/main/lua/tokyonight/colors
    "tokyo-night": ("#1a1b26", "#c0caf5"),
    # Solarized dark: base03 / base0. https://ethanschoonover.com/solarized/#the-values
    "solarized-dark": ("#002b36", "#839496"),
    # One Dark: syntax-bg / mono-1. https://github.com/atom/one-dark-syntax/blob/master/styles/colors.less
    "one-dark": ("#282c34", "#abb2bf"),
    # Rose Pine: base / text. https://rosepinetheme.com/palette/
    "rose-pine": ("#191724", "#e0def4"),
    # Kanagawa wave: sumiInk3 / fujiWhite. https://github.com/rebelot/kanagawa.nvim/blob/master/lua/kanagawa/colors.lua
    "kanagawa": ("#1f1f28", "#dcd7ba"),
}


def load():
    """Run the real palette construction without a core connection or extension loop."""
    ext = mock.Mock()
    ext.command.side_effect = lambda *_: lambda f: f
    sdk = types.SimpleNamespace(Extension=mock.Mock(return_value=ext), VqroError=RuntimeError)
    with mock.patch.dict(sys.modules, {"vqro_ext": sdk}), mock.patch.object(sys, "path", list(sys.path)):
        return runpy.run_path(str(Path(__file__).resolve().parents[1] / "extensions/themes.py"))


def luminance(colour):
    # WCAG 2.2 relative luminance: https://www.w3.org/TR/WCAG22/#dfn-relative-luminance
    rgb = [int(colour[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    linear = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in rgb]
    return sum(v * weight for v, weight in zip(linear, (0.2126, 0.7152, 0.0722)))


def contrast(a, b):
    low, high = sorted((luminance(a), luminance(b)))
    return (high + 0.05) / (low + 0.05)


class ThemeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.module = load()
        cls.themes = cls.module["BY_NAME"]

    def test_default_keeps_client_palette_and_preview(self):
        self.assertEqual(self.themes["default"]["palette"], {})
        self.assertTrue(set(self.module["PREVIEW"]) <= self.themes["default"]["preview"].keys())

    def test_base_covers_every_non_default_theme(self):
        self.assertEqual(set(BASE), set(self.themes) - {"default"})

    def test_every_role_is_rgb(self):
        for name, theme in self.themes.items():
            if name == "default":
                continue
            for role in ROLES:
                with self.subTest(theme=name, role=role):
                    self.assertIn(role, theme["palette"])
                    self.assertRegex(theme["palette"][role], r"^#[0-9a-f]{6}$")

    def test_every_role_has_at_least_three_to_one_base_contrast(self):
        for name, (background, _) in BASE.items():
            for role in ROLES:
                with self.subTest(theme=name, role=role):
                    palette = self.themes[name]["palette"]
                    self.assertIn(role, palette)
                    ratio = contrast(palette[role], background)
                    self.assertGreaterEqual(ratio, 3.0, f"{palette[role]} on {background}: {ratio:.3f}:1")

    def test_identities_are_distinct_from_each_other_and_default_accent(self):
        for name in BASE:
            with self.subTest(theme=name):
                palette = self.themes[name]["palette"]
                for role in ("you", "agent", "other_agent"):
                    self.assertIn(role, palette)
                self.assertEqual(len({palette[k] for k in ("you", "agent", "other_agent", "accent")}), 4)

    def test_link_is_distinct_from_published_text(self):
        for name, (_, foreground) in BASE.items():
            with self.subTest(theme=name):
                palette = self.themes[name]["palette"]
                self.assertIn("link", palette)
                self.assertNotEqual(palette["link"], foreground)
                self.assertNotEqual(palette["link"], palette["text"])

    def test_contrast_reference_values(self):
        self.assertAlmostEqual(contrast("#000000", "#ffffff"), 21.0)
        self.assertAlmostEqual(contrast("#123456", "#123456"), 1.0)
        self.assertAlmostEqual(contrast("#777777", "#ffffff"), 4.4781, places=4)
        self.assertAlmostEqual(contrast("#ffffff", "#777777"), 4.4781, places=4)


if __name__ == "__main__":
    unittest.main()
