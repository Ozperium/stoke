#!/usr/bin/env python3
"""Evidence and accessibility checks for the static landing site."""
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlparse
import json
import os
import struct
import runpy
import re
import subprocess
import tempfile


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

# A failed second backup must restore the first installed binary.
installer = (LANDING / "install.sh").read_text()
replace_fn = installer[installer.index("replace_binaries() {"):installer.index("\nrollback_binaries()")]
with tempfile.TemporaryDirectory() as tmp:
    root = Path(tmp)
    staged, installed, fake_bin = root / "staged", root / "installed", root / "bin"
    staged.mkdir(); installed.mkdir(); fake_bin.mkdir()
    for name, value in (("stoke", "new-stoke"), ("stoke-cli", "new-cli")):
        (staged / name).write_text(value)
        (installed / name).write_text(value.replace("new", "old"))
    fake_mv = fake_bin / "mv"
    fake_mv.write_text("#!/bin/sh\ncase \"$1:$2\" in *'/installed/stoke-cli:'*'/installed/stoke-cli.stoke-backup') exit 1;; esac\nexec /bin/mv \"$@\"\n")
    fake_mv.chmod(0o755)
    probe = root / "probe.sh"
    probe.write_text(f"{replace_fn}\nreplace_binaries \"$1\" || true\ntest \"$(cat \"$INSTALL_DIR/stoke\")\" = old-stoke\ntest \"$(cat \"$INSTALL_DIR/stoke-cli\")\" = old-cli\n")
    env = os.environ.copy()
    env.update({"PATH": f"{fake_bin}:{env['PATH']}", "INSTALL_DIR": str(installed)})
    subprocess.run(["sh", str(probe), str(staged)], env=env, check=True)

# Exercise metadata escaping with quotes and closing-script text, not only today's guides.
generator = runpy.run_path(str(ROOT / "scripts" / "build_guides.py"))
quoted = 'Gateway "safe" & local <models> </script>'
attribute_escape = generator.get("attr", generator["esc"])
probe = Page()
probe.feed(generator["PAGE"].format(title=attribute_escape(quoted), description=attribute_escape(quoted), canonical="https://stokegate.com/guides/fixture/", site="https://stokegate.com", og_type="article", jsonld=generator["jsonld"]({"title": quoted, "description": quoted}, "https://stokegate.com/guides/fixture/"), css="", body=""))
for key, name in (("name", "description"), ("property", "og:title"), ("property", "og:description"), ("name", "twitter:title"), ("name", "twitter:description")):
    assert [attrs.get("content") for tag, attrs in probe.tags if tag == "meta" and attrs.get(key) == name] == [quoted], f"quoted metadata roundtrip failed: {name}"
json_probe = generator["jsonld"]({"title": quoted, "description": quoted}, "https://stokegate.com/guides/fixture/")
assert "</script>" not in json_probe and json.loads(json_probe)["headline"] == quoted
assert generator["guide_url"]("cache-identity") == "https://stokegate.com/guides/cache-identity/"
for invalid_slug in ("../escape", 'bad"slug', "two/parts", "", "UpperCase", "space slug"):
    try:
        generator["guide_url"](invalid_slug)
    except ValueError:
        pass
    else:
        raise AssertionError(f"invalid guide slug accepted: {invalid_slug!r}")

# Social previews are required on the homepage and every generated guide route.
indexable = [LANDING / "index.html", LANDING / "guides" / "index.html"]
indexable.extend(sorted((LANDING / "guides").glob("*/index.html")))
for page_path in indexable:
    parser, _ = parse(page_path)
    page_html = page_path.read_text()
    canonical = "https://stokegate.com/" + ("" if page_path == LANDING / "index.html" else str(page_path.parent.relative_to(LANDING)) + "/")
    def values(tag, key, value, result):
        return [attrs.get(result) for name, attrs in parser.tags if name == tag and attrs.get(key) == value]
    assert values("link", "rel", "canonical", "href") == [canonical], f"canonical: {page_path}"
    assert values("meta", "property", "og:url", "content") == [canonical], f"OG URL: {page_path}"
    assert values("meta", "property", "og:image", "content") == ["https://stokegate.com/og-image.png"], f"OG image: {page_path}"
    assert values("meta", "property", "og:image:width", "content") == ["1200"]
    assert values("meta", "property", "og:image:height", "content") == ["630"]
    assert values("meta", "name", "twitter:card", "content") == ["summary_large_image"]
    assert values("meta", "name", "twitter:image", "content") == ["https://stokegate.com/og-image.png"]
    assert values("meta", "property", "og:type", "content") == ["website" if page_path in indexable[:2] else "article"]
    for key, property_name in (("property", "og:title"), ("property", "og:description"), ("property", "og:image:alt"), ("name", "twitter:title"), ("name", "twitter:description"), ("name", "twitter:image:alt")):
        found = values("meta", key, property_name, "content")
        assert len(found) == 1 and bool(found[0]), f"missing or duplicate {property_name}: {page_path}"
    schemas = re.findall(r'<script type="application/ld\+json">(.*?)</script>', page_html)
    assert len(schemas) == 1, f"JSON-LD count: {page_path}"
    schema = json.loads(schemas[0])
    assert schema["@context"] == "https://schema.org" and schema["url"] == canonical
    expected_type = "SoftwareApplication" if page_path == LANDING / "index.html" else "CollectionPage" if page_path == LANDING / "guides" / "index.html" else "TechArticle"
    assert schema["@type"] == expected_type, f"JSON-LD type: {page_path}"
    if expected_type == "SoftwareApplication":
        package_text = (ROOT / "Cargo.toml").read_text().split("[package]", 1)[1].split("\n[", 1)[0]
        version = re.search(r'^version\s*=\s*"([^"]+)"', package_text, re.M)
        assert version and schema["softwareVersion"] == version.group(1)
        assert schema["codeRepository"] == "https://github.com/Ozperium/stoke"

image = (LANDING / "og-image.png").read_bytes()
assert image[:8] == b"\x89PNG\r\n\x1a\n" and image[12:16] == b"IHDR"
assert struct.unpack(">II", image[16:24]) == (1200, 630), "actual OG image dimensions"
image_headers = (LANDING / "_headers").read_text().split("/og-image.png", 1)[1]
assert "immutable" not in image_headers, "stable OG filename must be revalidatable"

print(f"landing checks passed: {len(route_paths)} routes, 24 capabilities, {len(urls)} sitemap URLs, {len(indexable)} social-preview pages")
