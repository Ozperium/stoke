#!/usr/bin/env python3
"""Static acceptance checks for the public Stoke launch pages."""
from html.parser import HTMLParser
from pathlib import Path
import json
import re
import sys
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[1] / "landing"

class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.scripts = []
        self.jsonld = []
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
    path = href.split("#", 1)[0].split("?", 1)[0]
    if not path or path == "/":
        return ROOT / "index.html"
    candidate = ROOT / path.lstrip("/")
    if candidate.is_dir():
        candidate /= "index.html"
    return candidate


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def parse_sitemap(path):
    """Parse trusted repository XML without allowing DTD/entity expansion."""
    raw = path.read_text()
    check(not re.search(r"<!DOCTYPE\b|<!ENTITY\b", raw, re.I), "sitemap XML must not declare DTDs or entities")
    # The repository sitemap is trusted only after declaration rejection; this
    # stdlib parse therefore cannot expand an attacker-controlled DTD/entity.
    return ET.fromstring(raw)


def main():
    pages = [ROOT / "index.html", ROOT / "cloud/index.html", ROOT / "gateway/index.html", ROOT / "contact/index.html"]
    for page in pages:
        check(page.is_file(), f"missing page: {page}")
        doc = Document()
        doc.feed(page.read_text())
        check("/site.css" in doc.links or "/site.css" in doc.scripts or 'href="/site.css"' in page.read_text(), f"{page} missing shared CSS")
        for href in doc.links:
            target = local_target(href)
            if target:
                check(target.is_file(), f"broken local link in {page}: {href} -> {target}")
        for src in doc.scripts:
            target = local_target(src)
            if target:
                check(target.is_file(), f"missing script in {page}: {src}")
        for raw in doc.jsonld:
            json.loads(raw)

    home = (ROOT / "index.html").read_text()
    cloud = (ROOT / "cloud/index.html").read_text()
    gateway = (ROOT / "gateway/index.html").read_text()
    contact = (ROOT / "contact/index.html").read_text()
    combined = "\n".join([home, cloud, gateway, contact])

    check("Your models." in home and "Wherever" in home, "homepage promise missing")
    check("first 10" in combined.lower() and "30 days" in combined, "pilot offer missing")
    check("not deployed yet" in combined.lower(), "Cloud deployment boundary missing")
    check("macOS Apple Silicon" in cloud, "Apple Silicon preview boundary missing")
    check("not notarized by Apple" in cloud and "Gatekeeper" in cloud and "per-file approval" in cloud, "explicit unnotarized preview boundary missing")
    check("owned compute" in cloud.lower() and "provider bills" in cloud.lower(), "owned-compute/provider-bill boundary missing")
    check("cloud.stokegate.com" not in combined, "dead Cloud domain leaked into public CTA")
    check("data-flow-demo" in home and "illustrative product flow" in home.lower(), "illustrative flow missing")
    check("prefers-reduced-motion" in (ROOT / "site.css").read_text(), "reduced motion CSS missing")
    check("data-nav-toggle" in home and "aria-expanded" in home, "responsive navigation hooks missing")
    check(len(re.findall(r'<tr><td>\d{2}</td>', gateway)) == 24, "gateway matrix is not 24 rows")
    check("/cloud/" in (ROOT / "sitemap.xml").read_text() and "/gateway/" in (ROOT / "sitemap.xml").read_text(), "new routes missing from sitemap")
    check("pilot availability/details" in combined.lower(), "pilot availability CTA missing")
    check("open a public bug report" in contact.lower() and "github.com/Ozperium/stoke/issues/new" in contact, "contact route lacks bug-report GitHub CTA")
    check("no private intake form" in contact.lower() and 'data-track="cloud_invitation_requested"' not in contact, "contact route implies a private invitation intake")
    check("not deployed yet" in combined.lower(), "Cloud deployment boundary missing")
    events = (ROOT.parent / "functions" / "events.js").read_text()
    check("cloud_invitation_requested" in events and "body.event" in events, "invitation event is not allowlisted/validated")

    parse_sitemap(ROOT / "sitemap.xml")
    og = ROOT / "og-image.png"
    check(og.is_file() and og.stat().st_size > 1000, "OG PNG missing or unexpectedly small")
    svg = (ROOT / "og-image.svg").read_text()
    check('width="1200"' in svg and 'height="630"' in svg, "OG SVG dimensions missing")
    print(f"STATIC CHECKS PASSED: {len(pages)} pages, 24 gateway capabilities, sitemap/XML/JSON-LD/links/claims verified")

if __name__ == "__main__":
    try:
        main()
    except (AssertionError, json.JSONDecodeError, ET.ParseError) as exc:
        print(f"STATIC CHECKS FAILED: {exc}", file=sys.stderr)
        raise SystemExit(1)
