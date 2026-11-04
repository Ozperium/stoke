#!/usr/bin/env python3
"""Focused structural contracts for Stoke's public presentation routes."""
from html.parser import HTMLParser
from pathlib import Path
import json
import re
import sys
from urllib.parse import urlparse
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[1] / "landing"
DYNAMIC_PATHS = {"/api/waitlist"}
EXPECTED_MEDIA = {"stoke-drawn-desktop.svg", "stoke-drawn-mobile.svg"}


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links, self.scripts, self.images, self.videos, self.jsonld = [], [], [], [], []
        self._json = False
        self._chunks = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "a" and attrs.get("href"):
            self.links.append(attrs["href"])
        if tag == "script":
            if attrs.get("src"):
                self.scripts.append(attrs["src"])
            self._json = attrs.get("type") == "application/ld+json"
            self._chunks = []
        if tag == "img" and attrs.get("src"):
            self.images.append(attrs)
        if tag == "video":
            self.videos.append(attrs)

    def handle_data(self, data):
        if self._json:
            self._chunks.append(data)

    def handle_endtag(self, tag):
        if tag == "script" and self._json:
            self.jsonld.append("".join(self._chunks))
            self._json = False


def local_target(href):
    if not href.startswith("/") or href.startswith("//"):
        return None
    path = href.split("#", 1)[0].split("?", 1)[0] or "/"
    if path in DYNAMIC_PATHS:
        return None
    candidate = ROOT / path.lstrip("/")
    if path == "/":
        return ROOT / "index.html"
    if candidate.is_dir():
        candidate /= "index.html"
    return candidate


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def parse_sitemap(path):
    raw = path.read_text()
    check(not re.search(r"<!DOCTYPE\b|<!ENTITY\b", raw, re.I), "sitemap XML must not declare DTDs or entities")
    return ET.fromstring(raw)


def main():
    pages = [ROOT / "index.html", ROOT / "cloud/index.html", ROOT / "join/index.html", ROOT / "gateway/index.html", ROOT / "contact/index.html"]
    documents = {}
    for page in pages:
        check(page.is_file(), f"missing page: {page}")
        document = Document(); document.feed(page.read_text()); documents[page] = document
        check(any(src == "/presentation.css" for src in re.findall(r'href="([^"]+)"', page.read_text())) or page not in pages[:3], f"{page} missing presentation CSS")
        for href in document.links:
            target = local_target(href)
            if target and not href.startswith("/media/"):
                check(target.is_file(), f"broken local link in {page}: {href} -> {target}")
        for src in document.scripts:
            target = local_target(src)
            if target:
                check(target.is_file(), f"missing script in {page}: {src}")
        for raw in document.jsonld:
            json.loads(raw)

    home = (ROOT / "index.html").read_text()
    cloud = (ROOT / "cloud/index.html").read_text()
    join = (ROOT / "join/index.html").read_text()
    home_doc = documents[ROOT / "index.html"]
    cloud_doc = documents[ROOT / "cloud/index.html"]
    join_doc = documents[ROOT / "join/index.html"]

    check("Your models." in home and "Your rules." in home, "homepage promise missing")
    check("Your models." in cloud and "Beyond your desk." in cloud, "Cloud promise missing")
    check(not home_doc.videos, "rejected film must not be promoted")
    check('data-illustration' in home and 'data-layout="desktop"' in home and 'data-layout="mobile"' in home, "drawn responsive illustration missing")
    check('/illustration.mjs' in home and '/drawn.css' in home, "illustration runtime/style missing")
    check(all(f'data-scenario="{name}"' in home for name in ["routing", "policy", "fallback"]), "scenario controls missing")
    check('/media/stoke-drawn-desktop.svg' in cloud and '/media/stoke-drawn-mobile.svg' in cloud, "Cloud drawing assets missing")
    check("<form" not in home.lower() and "<form" not in cloud.lower(), "marketing pages must not add a signup form")
    check("preparing" not in (home + cloud).lower() and "prelaunch" not in (home + cloud).lower(), "marketing readiness wall remains")
    check("id=\"waitlist-form\"" in join and "action=\"/api/waitlist\"" in join and "method=\"post\"" in join, "join form contract missing")
    check('name="email"' in join and 'type="email"' in join and 'name="persona"' in join and 'value="solo"' in join and 'value="team"' in join, "join identity/persona fields missing")
    check('name="use_case"' in join and 'maxlength="800"' in join and 'name="consent"' in join and 'value="yes"' in join and 'name="notice_version"' in join and 'value="waitlist-v2"' in join, "join consent/use-case contract missing")
    check('name="website"' in join and 'class="honeypot"' in join and 'href="/api/waitlist"' in join and "<noscript>" in join, "join privacy/no-JS contract missing")
    check('meta name="robots" content="noindex, follow"' in join, "join must be noindex")
    waitlist_js = (ROOT / "newwaitlist.js").read_text()
    check("?format=json" in waitlist_js and "config.open === true" in waitlist_js and "fields.disabled = true" in waitlist_js, "waitlist must fail closed")
    check("new URLSearchParams(new FormData(form))" in waitlist_js and 'response.status !== 202' in waitlist_js and "details are still here" in waitlist_js, "waitlist submit/error handling missing")
    check("console." not in waitlist_js and "sendBeacon" not in waitlist_js and "data-track" not in waitlist_js, "waitlist must not add analytics")
    check("invitation" not in join.lower() and "invitation" not in waitlist_js.lower(), "automatic invitation claim leaked")
    check("role=\"alert\"" in join and "aria-live=\"polite\"" in join and "aria-live=\"assertive\"" in join, "join live regions missing")
    check("/join/" not in (ROOT / "sitemap.xml").read_text(), "noindex join route must not be in sitemap")
    check("prefers-reduced-motion" in (ROOT / "presentation.css").read_text(), "reduced motion CSS missing")
    check("data-nav-toggle" in home and "aria-expanded" in home, "responsive navigation hooks missing")

    sitemap = parse_sitemap(ROOT / "sitemap.xml")
    sitemap_paths = {urlparse(child.text).path for node in sitemap if node.tag.endswith("url") for child in node if child.tag.endswith("loc")}
    route_paths = {"/" if p == ROOT / "index.html" else "/" + str(p.relative_to(ROOT).parent).replace("\\", "/") + "/" for p in ROOT.rglob("index.html")}
    check(route_paths - {"/join/"} == sitemap_paths and len(sitemap_paths) == 16, "sitemap must include every indexable route")

    missing_media = sorted(name for name in EXPECTED_MEDIA if not (ROOT / "media" / name).is_file())
    check(not missing_media, "missing drawing assets: " + ", ".join(missing_media))
    media_note = "drawing assets present and referenced"
    print(f"STATIC CHECKS PASSED: {len(pages)} selected pages, join semantics, visual routes, accessibility hooks, sitemap, XML, JSON-LD, and links verified; {media_note}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, json.JSONDecodeError, ET.ParseError) as exc:
        print(f"STATIC CHECKS FAILED: {exc}", file=sys.stderr)
        raise SystemExit(1)
