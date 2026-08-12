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
readme = (ROOT / "README.md").read_text()
for event in ("install_cta_clicked", "github_clicked", "panel_demo_clicked", "local_capacity_cta_clicked", "guides_clicked"):
    assert f'data-track="{event}"' in home, f"missing event marker: {event}"
assert "Keep sensitive routes off unapproved providers" in home
assert "Runaway agents turn retries and fan-out into spend." in home
assert 'allowed_tiers</span> = [<span class="str">"local"' in home
assert "/v1/nodes" in home
assert "Node discovery" in home
assert "GET /v1/nodes" in home
assert "warm state, health, and load" in home
assert "Two machines, one endpoint" in home
assert '<span class="var">type</span> = <span class="str">"stoke"</span>' in home
assert '<span class="var">tier</span> = <span class="str">"remote"</span>' in home
assert "Run the failure proof, not a simulation." in home
assert "scripts/smoke_spend.sh" in home
assert "loop refusal" in home
assert "PII redaction" in home
assert "builtins.pii_redact" in home
assert '"stoke_route"' in home
assert "Three-node topology" in readme
assert "gpu-west" in readme
assert "cpu-east" in readme
assert "studio" in readme
assert "GET /v1/nodes" in readme
assert '"healthy": true' in readme
assert '"warm_models"' in readme
assert '"in_flight"' in readme
tracking = (LANDING / "tracking.js").read_text()
assert 'navigator.sendBeacon("/events"' in tracking
assert 'install_section_viewed' in tracking

handler = ROOT / "functions" / "events.js"
assert handler.is_file(), f"missing event handler: {handler}"
assert "INSERT INTO events" in handler.read_text()
assert "panel_demo_clicked" in handler.read_text()
assert "local_capacity_cta_clicked" in handler.read_text()
assert "guides_clicked" in handler.read_text()

wrangler = (ROOT / "wrangler.toml").read_text()
assert "d1_databases" in wrangler
assert "c04effbf-1293-4b35-84d3-9fb47519ed73" in wrangler

print("landing checks passed")
