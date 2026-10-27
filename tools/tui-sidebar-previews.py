#!/usr/bin/env python3
"""Render the TestBackend sidebar fixture JSON to PNG; no daemon is started.

First: cargo test -p homeway-cli sidebar_layout_preview_artifacts --offline -- --ignored
Then: python3 tools/tui-sidebar-previews.py
Uses the existing tui-smoke.py Pillow/pyte helpers.
"""

import argparse
import json
from pathlib import Path
import runpy
import sys
from types import SimpleNamespace

sys.dont_write_bytecode = True
helpers = runpy.run_path(str(Path(__file__).with_name("tui-smoke.py")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path("/tmp/homeway-tui-sidebar-previews"))
    args = parser.parse_args()
    fixtures = sorted(args.directory.glob("*.json"))
    assert fixtures, "run the ignored sidebar preview test first"
    for path in fixtures:
        data = json.loads(path.read_text())
        screen = helpers["pyte"].Screen(data["width"], data["height"])
        for y, row in enumerate(data["rows"]):
            for x, cell in enumerate(row):
                screen.buffer[y][x] = screen.buffer[y][x]._replace(**cell)
        # snapshot() writes the already generated text plus paints each physical
        # cell, preserving CJK double-width heads and all actual RGB backgrounds.
        preview = SimpleNamespace(screen=screen, screen_text=lambda: path.with_suffix(".txt").read_text())
        helpers["snapshot"](preview, args.directory, path.stem, light=data["light"])
        print("PREVIEW: " + str(path.with_suffix(".png")))


if __name__ == "__main__":
    main()
