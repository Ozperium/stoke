#!/usr/bin/env python3
"""Static contracts for the visual-first public landing routes."""
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlparse
import json
import runpy
import subprocess
import re
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / "landing"
DYNAMIC_PATHS = {"/api/waitlist"}
NEW_MEDIA = {
    "stoke-drawn-desktop.svg",
    "stoke-drawn-mobile.svg",
}


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.tags, self.hrefs, self.text, self.ids = [], [], [], set()
        self.jsonld = []
        self._json = False
        self._json_chunks = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        self.tags.append((tag, attrs))
        if attrs.get("id"):
            assert attrs["id"] not in self.ids, f"duplicate id {attrs['id']}"
            self.ids.add(attrs["id"])
        if attrs.get("href"):
            self.hrefs.append(attrs["href"])
        if tag == "script" and attrs.get("type") == "application/ld+json":
            self._json = True
            self._json_chunks = []

    def handle_data(self, data):
        self.text.append(data)
        if self._json:
            self._json_chunks.append(data)

    def handle_endtag(self, tag):
        if tag == "script" and self._json:
            self.jsonld.append("".join(self._json_chunks))
            self._json = False


def parse(path):
    document = Page()
    document.feed(path.read_text())
    return document


def local_target(href):
    parsed = urlparse(href)
    if parsed.scheme or parsed.netloc or href.startswith("mailto:"):
        return None
    path = parsed.path or "/"
    if path in DYNAMIC_PATHS:
        return None
    if path == "/":
        return LANDING / "index.html"
    return LANDING / path.lstrip("/") / "index.html" if path.endswith("/") else LANDING / path.lstrip("/")


def tags(document, name):
    return [attrs for tag, attrs in document.tags if tag == name]


pages = {path: parse(path) for path in sorted(LANDING.rglob("index.html"))}
home_path = LANDING / "index.html"
cloud_path = LANDING / "cloud" / "index.html"
join_path = LANDING / "join" / "index.html"
home, cloud, join = pages[home_path], pages[cloud_path], pages[join_path]

assert home.ids and sum(tag == "h1" for tag, _ in home.tags) == 1
assert sum(tag == "h1" for tag, _ in cloud.tags) == 1
assert sum(tag == "h1" for tag, _ in join.tags) == 1
assert "Your models." in " ".join(home.text) and "Your rules." in " ".join(home.text)
assert "Your models." in " ".join(cloud.text) and "Beyond your desk." in " ".join(cloud.text)
main_html = home_path.read_text().split("<main", 1)[1].split("</main>", 1)[0]
main_html = re.sub(r"<svg\b.*?</svg>|<figcaption\b.*?</figcaption>", " ", main_html, flags=re.S)
main_copy = re.sub(r"<[^>]+>", " ", main_html)
assert len(main_copy.split()) < 90, "homepage body copy exceeds the sparse-copy budget"
assert not any(tag == "form" for tag, _ in home.tags + cloud.tags), "marketing pages must not embed the signup form"
assert "preparing" not in (home_path.read_text() + cloud_path.read_text()).lower()
assert "prelaunch" not in (home_path.read_text() + cloud_path.read_text()).lower()

assert not tags(home, "video"), "rejected film must not remain promoted"
assert {a.get("data-layout") for tag, a in home.tags if tag == "svg"} == {"desktop", "mobile"}
assert {a.get("data-scenario") for tag, a in home.tags if a.get("role") == "tab"} == {"routing", "policy", "fallback"}
assert any("data-play-example" in a and a.get("type") == "button" for tag, a in home.tags if tag == "button")
assert "/illustration.mjs" in home_path.read_text() and "/drawn.css" in home_path.read_text()
assert "/media/stoke-drawn-desktop.svg" in cloud_path.read_text() and "/media/stoke-drawn-mobile.svg" in cloud_path.read_text()
controller = (LANDING / "illustration.mjs").read_text()
for marker in ("cancelAnimationFrame", "aria-selected", "visibilitychange", "prefers-reduced-motion", "aria-labelledby"):
    assert marker in controller, f"missing illustration behavior: {marker}"
assert "fetch(" not in controller and "setTimeout(" not in controller

forms = [attrs for tag, attrs in join.tags if tag == "form"]
assert len(forms) == 1 and forms[0].get("action") == "/api/waitlist" and forms[0].get("method") == "post"
fields = [attrs for tag, attrs in join.tags if tag in {"input", "textarea"} and attrs.get("name")]
assert {attrs["name"] for attrs in fields} == {"email", "persona", "use_case", "consent", "notice_version", "website"}
assert any(a.get("name") == "email" and a.get("type") == "email" and "required" in a for a in fields)
assert sum(a.get("name") == "persona" and a.get("type") == "radio" for a in fields) == 2
assert all("required" in a for a in fields if a.get("name") == "persona")
assert any(a.get("name") == "consent" and a.get("value") == "yes" and "required" in a for a in fields)
assert any(a.get("name") == "notice_version" and a.get("value") == "waitlist-v2" for a in fields)
assert any(a.get("name") == "website" and a.get("value") == "" for a in fields)
assert 'href="/api/waitlist"' in join_path.read_text() and "<noscript>" in join_path.read_text()
assert 'meta name="robots" content="noindex, follow"' in join_path.read_text()

waitlist_js = (LANDING / "newwaitlist.js").read_text()
assert "?format=json" in waitlist_js and "config.open === true" in waitlist_js
assert "fields.disabled = true" in waitlist_js and "new URLSearchParams(new FormData(form))" in waitlist_js
assert "response.status !== 202" in waitlist_js and "details are still here" in waitlist_js
assert "console." not in waitlist_js and "sendBeacon" not in waitlist_js and "data-track" not in waitlist_js
assert "Requests are reviewed manually." in waitlist_js
assert "invitation" not in waitlist_js.lower()

legal = (LANDING / "legal" / "index.html").read_text()
for phrase in ("email address", "solo/team persona", "use-case description", "consent", "90 days", "daily cleanup", "Cloudflare", "hashed anti-abuse"):
    assert phrase.lower() in legal.lower(), f"missing legal waitlist disclosure: {phrase}"
assert "controller_name" not in join_path.read_text() and "privacy_email" not in waitlist_js

for page_path, document in pages.items():
    assert tags(document, "title"), f"missing title: {page_path}"
    for href in document.hrefs:
        target = local_target(href)
        if target is not None and not href.startswith("/media/"):
            assert target.is_file(), f"missing local link target: {page_path}: {href} -> {target}"
    for raw in document.jsonld:
        json.loads(raw)

sitemap = LANDING / "sitemap.xml"
sitemap_root = ET.fromstring(sitemap.read_text())
urls = {urlparse(child.text).path for node in sitemap_root if node.tag.endswith("url") for child in node if child.tag.endswith("loc")}
route_paths = {"/" if path == home_path else "/" + str(path.relative_to(LANDING).parent).replace("\\", "/") + "/" for path in pages}
assert route_paths - {"/join/"} == urls, "sitemap must contain every indexable route"
assert len(urls) == 16 and "/join/" not in urls, "noindex signup must stay out of sitemap"

missing_media = sorted(name for name in NEW_MEDIA if not (LANDING / "media" / name).is_file())
assert not missing_media, "missing presentation media: " + ", ".join(missing_media)
media_note = "new presentation media present"
runpy.run_path(str(ROOT / "scripts/check_landing_regressions.py"))
subprocess.run(["node", "--test", str(ROOT / "scripts/test_illustration_model.mjs")], check=True, cwd=ROOT)
print(f"landing checks passed: {len(route_paths)} routes, sparse marketing copy, join form semantics, metadata, links, and sitemap verified; {media_note}")
