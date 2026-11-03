#!/usr/bin/env python3
"""Replay captured wire assets/placements without adding a destination clip.

Input is the test-only VQRO_IMAGE_EVIDENCE_ROOT export from
nested_kitty_placements_clip_to_terminal_body_side_and_stacked. This is an
actual terminal-parser + host-projection capture, not a native GUI screenshot.
Pillow must already be installed. No server, session, network, or credentials.
"""
import argparse
import copy
import hashlib
import io
import json
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont


def rgb(packed, background=False):
    if packed >> 24 == 2:
        return ((packed >> 16) & 255, (packed >> 8) & 255, packed & 255)
    return (24, 24, 32) if background else (205, 214, 244)


def box(rect, cw, ch):
    return (rect["x"] * cw, rect["y"] * ch,
            (rect["x"] + rect["width"]) * cw,
            (rect["y"] + rect["height"]) * ch)


def replay(record, font_path):
    """Paint precisely what the scene says, then independently detect escapes."""
    cw, ch = record["cell_width"], record["cell_height"]
    base = Image.new("RGBA", (record["columns"] * cw, record["rows"] * ch))
    draw = ImageDraw.Draw(base)
    font = ImageFont.truetype(str(font_path), 16)
    for offset, cell in enumerate(record["frame"]["cells"]):
        x, y = offset % record["columns"], offset // record["columns"]
        draw.rectangle((x * cw, y * ch, (x + 1) * cw - 1, (y + 1) * ch - 1),
                       fill=rgb(cell["bg"], True) + (255,))
        if not cell["skip"]:
            draw.text((x * cw, y * ch), cell["symbol"],
                      font=font, fill=rgb(cell["fg"]) + (255,))
    result = base.copy()
    key = lambda value: json.dumps(value, sort_keys=True)
    assets = {key(asset["key"]): asset for asset in record["graphics"]["assets"]}
    assert record["graphics"]["placements"], "empty scene proves nothing"
    for placement in sorted(record["graphics"]["placements"], key=lambda item: item["z"]):
        assert placement["z"] >= 0, "fixture uses foreground placements"
        asset = assets[key(placement["asset"])]
        spec = asset["key"]
        data = bytes(asset["data"])
        assert len(data) == spec["data_len"]
        if spec["format"] == "Png":
            image = Image.open(io.BytesIO(data)).convert("RGBA")
        else:
            mode = {"Rgb": "RGB", "Rgba": "RGBA"}[spec["format"]]
            image = Image.frombytes(mode, (spec["image_width"], spec["image_height"]), data).convert("RGBA")
        sx, sy = placement["source_x"], placement["source_y"]
        sw, sh = placement["source_width"], placement["source_height"]
        assert sw > 0 and sh > 0
        assert sx + sw <= image.width and sy + sh <= image.height
        image = image.crop((sx, sy, sx + sw, sy + sh))
        image = image.resize((placement["cols"] * cw, placement["rows"] * ch), Image.Resampling.NEAREST)
        # Deliberately NO body/view intersection or other security clip here.
        # A bad wire rectangle must paint outside and fail the pixel fence.
        result.alpha_composite(image, (placement["x"] * cw + placement["x_offset"],
                                       placement["y"] * ch + placement["y_offset"]))
    channels = ImageChops.difference(base.convert("RGB"), result.convert("RGB")).split()
    changed = ImageChops.lighter(ImageChops.lighter(channels[0], channels[1]), channels[2]).point(lambda value: 255 if value else 0)
    assert changed.getbbox(), "a fully transparent/missing image proves nothing"
    allowed = Image.new("L", base.size)
    left, top, right, bottom = box(record["body"], cw, ch)
    ImageDraw.Draw(allowed).rectangle((left, top, right - 1, bottom - 1), fill=255)
    escaped = ImageChops.multiply(changed, ImageChops.invert(allowed))
    assert escaped.getbbox() is None, "image pixels escaped into terminal exterior/title/border"
    assert changed.crop(box(record["view"], cw, ch)).getbbox() is None, "image pixels covered the sibling view/title/border"
    return result


def verify(path, font_path):
    record = json.loads(path.read_text())
    image = replay(record, font_path)
    image.save(path.with_suffix(".png"))
    cw, ch = record["cell_width"], record["cell_height"]
    body, view = record["body"], record["view"]
    if view["y"] > body["y"]:
        seam = (0, max(0, body["y"] + body["height"] - 3) * ch, image.width, image.height)
    else:
        seam = (max(0, body["x"] + body["width"] - 3) * cw, 0, image.width, image.height)
    image.crop(seam).save(path.with_name(path.stem + "-seam.png"))
    for mutation in ("oversized", "title"):
        bad = copy.deepcopy(record)
        placement = bad["graphics"]["placements"][0]
        if mutation == "oversized":
            placement["cols"] = record["columns"]
        else:
            placement["x"] = placement["y"] = 0
        try:
            replay(bad, font_path)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"negative control {mutation} failed to detect a leak")
    return {"record": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "png_sha256": hashlib.sha256(path.with_suffix(".png").read_bytes()).hexdigest(),
            "font": str(font_path),
            "placements": len(record["graphics"]["placements"]),
            "assets": len(record["graphics"]["assets"]), "negative_controls": 2,
            "outside_body_changed_pixels": 0, "view_changed_pixels": 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", nargs="+", type=Path)
    parser.add_argument("--font", type=Path, required=True, help="An already-installed monospace font; no downloads")
    args = parser.parse_args()
    print(json.dumps([verify(path, args.font) for path in args.records], indent=2))


if __name__ == "__main__":
    main()
