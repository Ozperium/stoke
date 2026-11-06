#!/usr/bin/env python3
"""Exact approved mark and all active derivative provenance."""
import hashlib
import json
from pathlib import Path
from xml.etree import ElementTree as ET
ROOT = Path(__file__).resolve().parents[1]
PATHS = [
    "M4 24 L12 22.5 L20 23 L28 22.4 L29 27 L20 28 L11 27.4 L3 28 Z",
    "M6 20 L11 12 L15 5 L19 4 L20 8 L14 16 L10 22 Z",
    "M20 11 L24 12 L28 19 L26 22 L22 19 L18 14 Z",
]
for name in ("stoke-mark.svg", "favicon.svg"):
    # Locally authored, committed SVG only; reject entity declarations before parsing.
    source = (ROOT / "landing" / name).read_text()
    assert "<!DOCTYPE" not in source and "<!ENTITY" not in source
    svg = ET.fromstring(source)
    assert svg.attrib["viewBox"] == "0 0 32 32"
    assert svg.attrib["color"] == "#F15332"
    assert [n.attrib["d"] for n in svg.iter() if n.tag.endswith("path")] == PATHS
manifest = json.loads((ROOT / "design/use-cases/editorial/manifest.json").read_text())
assert len(manifest["items"]) == 6
for item in manifest["items"]:
    for field in ("source", "hero", "social"):
        assert hashlib.sha256((ROOT / item[field]).read_bytes()).hexdigest() == item[field + "_sha256"]
    svg = (ROOT / item["source"]).read_text()
    assert all(path in svg for path in PATHS)
    assert "M5.229" not in svg
assert all(path in (ROOT / "landing/og-image.svg").read_text() for path in PATHS)
assert "M5.229" not in (ROOT / "landing/og-image.svg").read_text()
digest = hashlib.sha256((ROOT / "landing/favicon.svg").read_bytes()).hexdigest()[:12]
pages = list((ROOT / "landing").rglob("*.html"))
assert len(pages) >= 25
for page in pages:
    text = page.read_text()
    assert '/stoke-mark.svg' in text, page
    assert '/favicon.svg?v=' + digest in text, page
print("selected lean-to mark passed: exact paths, sitewide favicon version, six illustration and social derivatives")
