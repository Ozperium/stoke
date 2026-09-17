#!/usr/bin/env python3
"""Evidence and accessibility checks for the static landing site."""
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlparse
import re


ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / "landing"

class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids, self.hrefs, self.tags, self.text = set(), [], [], []
    def handle_starttag(self, tag, attrs):
        self.tags.append((tag, dict(attrs)))
        attrs = dict(attrs)
        if attrs.get("id"):
            assert attrs["id"] not in self.ids, f"duplicate id {attrs['id']}"
            self.ids.add(attrs["id"])
        if "href" in attrs:
            self.hrefs.append(attrs["href"])
    def handle_data(self, data):
        self.text.append(data)


def parse(path):
    p = Page(); p.feed(path.read_text()); return p, " ".join(p.text)

def local_target(href):
    parsed = urlparse(href)
    if parsed.scheme or parsed.netloc or href.startswith("mailto:"):
        return None
    path = parsed.path or "/"
    if path == "/": return LANDING / "index.html"
    clean = path.lstrip("/")
    return LANDING / clean / "index.html" if path.endswith("/") else LANDING / clean

home_parser, home_text = parse(LANDING / "index.html")
assert home_parser.tags[0][0] == "html" and home_parser.tags[0][1].get("lang") == "en"
assert sum(tag == "h1" for tag, _ in home_parser.tags) == 1
assert "id=\"capabilities\"" in (LANDING / "index.html").read_text()
assert len(re.findall(r'data-feature="\d+"', (LANDING / "index.html").read_text())) == 24
assert "One gateway, 24 evidenced capabilities." in home_text
for number in range(1, 25):
    assert f'data-feature="{number}"' in (LANDING / "index.html").read_text(), f"missing capability {number}"

# Every same-page anchor resolves, and every first-party href points at a file/route.
for href in home_parser.hrefs:
    if href.startswith("#"):
        assert href[1:] in home_parser.ids, f"missing anchor: {href}"
    else:
        target = local_target(href)
        if target is not None:
            assert target.is_file(), f"missing local link target: {href} -> {target}"

# Required evidence and boundary language.
for phrase in (
    "Run the failure proof, not a simulation.", "scripts/smoke_spend.sh", "PII redaction",
    "builtins.pii_redact", '"stoke_route"', "GET /v1/nodes", "Two machines, one endpoint",
    "durable", "subscription", "feature-gated", "stream can still overshoot",
    "client protocol", "provider", "STOKE_API_KEYS", "POST /ui/demo",
):
    assert phrase.lower() in home_text.lower(), f"missing evidence copy: {phrase}"
for stale in ("supports every provider", "all provider", "unlimited savings", "guaranteed savings", "zero cost"):
    assert stale not in home_text.lower(), f"stale/overclaiming copy: {stale}"

# Dynamic UI has an honest live region and keyboard/mobile controls.
assert any(a.get("id") == "nav-toggle" and a.get("aria-controls") == "nav-links" and a.get("aria-expanded") == "false" for t, a in home_parser.tags if t == "button")
assert any(a.get("id") == "panel-demo-feed" and a.get("role") == "log" and a.get("aria-live") == "polite" for t, a in home_parser.tags)
assert any(a.get("id") == "term-body" and a.get("aria-live") == "polite" for t, a in home_parser.tags)
for tag, attrs in home_parser.tags:
    if tag == "button": assert attrs.get("type") == "button", "button missing explicit type"
assert "prefers-reduced-motion" in (LANDING / "index.html").read_text()

# All rendered routes must have titles and resolve their first-party links.
for page_path in sorted(LANDING.rglob("index.html")):
    parser, _ = parse(page_path)
    assert any(t == "title" for t, _ in parser.tags), f"missing title: {page_path}"
    for href in parser.hrefs:
        target = local_target(href)
        if target is not None: assert target.is_file(), f"missing local link: {page_path}: {href}"

# Sitemap entries must be real routes and must include every published HTML route.
sitemap_text = (LANDING / "sitemap.xml").read_text()
urls = {urlparse(match).path for match in re.findall(r"<loc>([^<]+)</loc>", sitemap_text)}
route_paths = {"/" if p == LANDING / "index.html" else "/" + str(p.relative_to(LANDING).parent).replace("\\", "/") + "/" for p in LANDING.rglob("index.html")}
assert route_paths <= urls, f"routes missing from sitemap: {sorted(route_paths - urls)}"
assert all(url.startswith("/") for url in urls)

# Cross-check the machine-readable capability inventory.
llms = (LANDING / "llms.txt").read_text()
assert len(re.findall(r"^\d+\. ", llms, re.M)) >= 24
assert "Complete shipped capability matrix (24)" in llms

for route in ("security", "legal", "contact"):
    page = LANDING / route / "index.html"
    assert page.is_file(), f"missing route: {page}"
    assert f"https://stokegate.com/{route}/" in page.read_text()

tracking = (LANDING / "tracking.js").read_text()
for event in ("install_cta_clicked", "github_clicked", "panel_demo_clicked", "local_capacity_cta_clicked", "guides_clicked"):
    assert f'data-track="{event}"' in (LANDING / "index.html").read_text()
assert 'navigator.sendBeacon("/events"' in tracking and "install_section_viewed" in tracking
handler = (ROOT / "functions" / "events.js").read_text()
assert "INSERT INTO events" in handler and "panel_demo_clicked" in handler
wrangler = (ROOT / "wrangler.toml").read_text()
assert "d1_databases" in wrangler and "c04effbf-1293-4b35-84d3-9fb47519ed73" in wrangler
print(f"landing checks passed: {len(route_paths)} routes, 24 capabilities, {len(urls)} sitemap URLs")
