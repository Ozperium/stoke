#!/usr/bin/env python3
"""Static checks for the landing page routes and first-party event funnel."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / "landing"

for route in ("security", "legal", "contact"):
    page = LANDING / route / "index.html"
    assert page.is_file(), f"missing route: {page}"
    html = page.read_text()
    assert f"https://stokegate.com/{route}/" in html
    assert "<title>" in html

home = (LANDING / "index.html").read_text()
for event in ("install_cta_clicked", "github_clicked"):
    assert f'data-track="{event}"' in home, f"missing event marker: {event}"
tracking = (LANDING / "tracking.js").read_text()
assert 'navigator.sendBeacon("/events"' in tracking
assert 'install_section_viewed' in tracking

handler = ROOT / "functions" / "events.js"
assert handler.is_file(), f"missing event handler: {handler}"
assert "INSERT INTO events" in handler.read_text()

wrangler = (ROOT / "wrangler.toml").read_text()
assert "d1_databases" in wrangler
assert "c04effbf-1293-4b35-84d3-9fb47519ed73" in wrangler

print("landing checks passed")
