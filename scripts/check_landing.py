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
import sys


ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / "landing"
DYNAMIC_PATHS = {"/api/waitlist"}

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
    if path in DYNAMIC_PATHS:
        return None
    if path == "/": return LANDING / "index.html"
    clean = path.lstrip("/")
    return LANDING / clean / "index.html" if path.endswith("/") else LANDING / clean

home_parser, home_text = parse(LANDING / "index.html")
assert home_parser.tags[0][0] == "html" and home_parser.tags[0][1].get("lang") == "en"
assert sum(tag == "h1" for tag, _ in home_parser.tags) == 1
gateway_html = (LANDING / "gateway" / "index.html").read_text()
assert len(re.findall(r'<tr><td>\d{2}</td>', gateway_html)) == 24, "gateway matrix is not 24 rows"
assert "#capabilities" not in (LANDING / "llms.txt").read_text().replace("retired homepage `#capabilities`", "")

# Every same-page anchor resolves, and every first-party href points at a file/route.
parsers = {}
for page_path in sorted(LANDING.rglob("index.html")):
    parsers[page_path] = parse(page_path)[0]
for page_path, parser in parsers.items():
    for href in parser.hrefs:
        target = page_path if not urlparse(href).path and not urlparse(href).scheme and not urlparse(href).netloc else local_target(href)
        if target is not None:
            assert target.is_file(), f"missing local link target: {page_path}: {href} -> {target}"
            fragment = urlparse(href).fragment
            if fragment:
                assert fragment in parsers[target].ids, f"missing anchor: {page_path}: {href}"

# Required evidence and boundary language.
all_text = "\n".join(parse(path)[1] for path in parsers)
for phrase in ("PII redaction", "GET /v1/nodes", "durable", "subscription", "feature-gated", "client paths", "provider", "STOKE_API_KEYS", "POST /v1/responses"):
    assert phrase.lower() in all_text.lower(), f"missing evidence copy: {phrase}"
for stale in ("supports every provider", "supports all providers", "unlimited savings", "guaranteed savings", "zero cost"):
    assert stale not in all_text.lower(), f"stale/overclaiming copy: {stale}"

# Dynamic UI has an honest live region and keyboard/mobile controls.
assert any("data-nav-toggle" in a and a.get("aria-controls") == "nav-menu" and a.get("aria-expanded") == "false" for t, a in home_parser.tags if t == "button")
videos = [a for t, a in home_parser.tags if t == "video"]
assert len(videos) == 1, "exactly one product preview required"
assert all(k in videos[0] for k in ["controls", "playsinline", "poster", "aria-label", "aria-describedby"])
assert "autoplay" not in videos[0], "preview must not force playback"
assert videos[0]["poster"] == "/media/stoke-preview-poster.jpg"
assert "sample data" in home_text.lower() and "not a live" in home_text.lower()
assert not any("data-flow-demo" in a for t, a in home_parser.tags), "retired abstraction must not remain hidden"
for tag, attrs in home_parser.tags:
    if tag == "button": assert attrs.get("type") == "button", "button missing explicit type"
for name in ["stoke-preview.mp4", "stoke-preview-poster.jpg", "stoke-preview-devices.jpg", "stoke-preview-access.jpg", "stoke-preview-activity.jpg"]:
    assert (LANDING / "media" / name).is_file(), "missing product media: " + name
site_css = (LANDING / "site.css").read_text()
site_js = (LANDING / "site.js").read_text()
assert ".site-nav.nav-enhanced .nav-links" in site_css and ".site-nav.nav-enhanced .nav-toggle" in site_css
assert "header.classList.add('nav-enhanced')" in site_js and "event.key === 'Escape'" in site_js
assert "product-preview" in site_js and ".play()" in site_js, "watch CTA must play the actual preview"
assert "prefers-reduced-motion" in site_css

# Waitlist markup is a minimal, no-JS-capable form; JS only opens it after a fail-closed config check.
cloud_page = LANDING / "cloud" / "index.html"
cloud_parser = parsers[cloud_page]
forms = [attrs for tag, attrs in cloud_parser.tags if tag == "form" and attrs.get("id") == "waitlist-form"]
assert len(forms) == 1, "waitlist form missing or duplicated"
waitlist_form = forms[0]
assert waitlist_form.get("action") == "/api/waitlist" and waitlist_form.get("method") == "post"
fields = [attrs for tag, attrs in cloud_parser.tags if tag in {"input", "textarea"} and attrs.get("name")]
field_names = {attrs["name"] for attrs in fields}
assert field_names == {"email", "persona", "use_case", "consent", "notice_version", "website"}
assert any(attrs.get("name") == "email" and attrs.get("type") == "email" and "required" in attrs for attrs in fields)
assert sum(attrs.get("name") == "persona" and attrs.get("type") == "radio" for attrs in fields) == 2
assert all("required" in attrs for attrs in fields if attrs.get("name") == "persona")
assert any(attrs.get("name") == "consent" and attrs.get("value") == "yes" and "required" in attrs for attrs in fields)
assert any(attrs.get("name") == "notice_version" and attrs.get("value") == "waitlist-v1" for attrs in fields)
assert any(attrs.get("name") == "website" and attrs.get("value") == "" for attrs in fields)
assert 'href="/api/waitlist"' in cloud_page.read_text() and "<noscript>" in cloud_page.read_text()
waitlist_js = (LANDING / "newwaitlist.js").read_text()
assert "?format=json" in waitlist_js and "config.open === true" in waitlist_js
assert "fields.disabled = true" in waitlist_js and "closeWaitlist" in waitlist_js
assert "new URLSearchParams(new FormData(form))" in waitlist_js
assert 'response.status !== 202' in waitlist_js and "not acceptance" in waitlist_js
assert "role=\"alert\"" in cloud_page.read_text() and "aria-live=\"polite\"" in cloud_page.read_text()
assert "console." not in waitlist_js and "sendBeacon" not in waitlist_js and "data-track" not in waitlist_js
assert local_target("/api/waitlist") is None and local_target("/api/unknown") == LANDING / "api" / "unknown"
legal_text = (LANDING / "legal" / "index.html").read_text()
assert "email address" in legal_text and "solo/team persona" in legal_text and "use-case description" in legal_text and "consent" in legal_text
assert "/api/waitlist" in legal_text and "90-day expiry" in legal_text and "daily cleanup" in legal_text and "Cloudflare" in legal_text

# All rendered routes must have titles and resolve their first-party links.
for page_path in sorted(LANDING.rglob("index.html")):
    parser, _ = parse(page_path)
    assert any(t == "title" for t, _ in parser.tags), f"missing title: {page_path}"
    for href in parser.hrefs:
        target = page_path if not urlparse(href).path and not urlparse(href).scheme and not urlparse(href).netloc else local_target(href)
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
handler = (ROOT / "functions" / "events.js").read_text()
allowed_events = set(re.findall(r"'([a-z0-9_]+)'", handler.split("]);", 1)[0]))
tracked_events = {attrs["data-track"] for parser in parsers.values() for tag, attrs in parser.tags if "data-track" in attrs}
assert tracked_events <= allowed_events, f"unallowlisted tracking event: {sorted(tracked_events - allowed_events)}"
assert 'navigator.sendBeacon("/events"' in tracking and "install_section_viewed" in tracking
assert "INSERT INTO events" in handler and "cloud_invitation_requested" in handler
assert "EVENTS.has(body.event)" in handler and "body.event" in handler
assert "document.cookie" not in tracking and "localStorage" not in tracking
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

# Social previews are required on the homepage, Cloud, gateway, and every generated guide route.
indexable = [
    LANDING / "index.html",
    LANDING / "cloud" / "index.html",
    LANDING / "gateway" / "index.html",
    LANDING / "guides" / "index.html",
]
indexable.extend(sorted((LANDING / "guides").glob("*/index.html")))
website_pages = {
    LANDING / "index.html",
    LANDING / "cloud" / "index.html",
    LANDING / "gateway" / "index.html",
    LANDING / "guides" / "index.html",
}
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
    assert values("meta", "property", "og:type", "content") == ["website" if page_path in website_pages else "article"]
    for key, property_name in (("property", "og:title"), ("property", "og:description"), ("property", "og:image:alt"), ("name", "twitter:title"), ("name", "twitter:description"), ("name", "twitter:image:alt")):
        found = values("meta", key, property_name, "content")
        assert len(found) == 1 and bool(found[0]), f"missing or duplicate {property_name}: {page_path}"
    schemas = re.findall(r'<script type="application/ld\+json">(.*?)</script>', page_html)
    assert len(schemas) == 1, f"JSON-LD count: {page_path}"
    schema = json.loads(schemas[0])
    assert schema["@context"] == "https://schema.org" and schema["url"] == canonical
    expected_type = (
        "SoftwareApplication" if page_path == LANDING / "index.html"
        else "WebPage" if page_path in {LANDING / "cloud" / "index.html", LANDING / "gateway" / "index.html"}
        else "CollectionPage" if page_path == LANDING / "guides" / "index.html"
        else "TechArticle"
    )
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
