#!/usr/bin/env python3
"""Static acceptance checks for the public Stoke launch pages."""
from html.parser import HTMLParser
from pathlib import Path
import json
import re
import sys
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[1] / "landing"
DYNAMIC_PATHS = {"/api/waitlist"}
EXPECTED_MEDIA = {
    "stoke-preview.mp4",
    "stoke-preview-poster.jpg",
    "stoke-preview-devices.jpg",
    "stoke-preview-access.jpg",
    "stoke-preview-activity.jpg",
}


class Document(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.scripts = []
        self.images = []
        self.videos = []
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
    path = href.split("#", 1)[0].split("?", 1)[0]
    if path in DYNAMIC_PATHS:
        return None
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
    return ET.fromstring(raw)


def main():
    pages = [ROOT / "index.html", ROOT / "cloud/index.html", ROOT / "gateway/index.html", ROOT / "contact/index.html"]
    documents = {}
    for page in pages:
        check(page.is_file(), f"missing page: {page}")
        doc = Document()
        doc.feed(page.read_text())
        documents[page] = doc
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
    css = (ROOT / "site.css").read_text()
    combined = "\n".join([home, cloud, gateway, contact])
    home_doc = documents[ROOT / "index.html"]

    check("Your model stays home." in home and "Your work doesn't." in home, "homepage value promise missing")
    check(len(home_doc.videos) == 1, "homepage must contain exactly one product preview video")
    video = home_doc.videos[0]
    check(video.get("poster") == "/media/stoke-preview-poster.jpg", "preview poster missing or incorrect")
    check(video.get("preload") in {"metadata", "none"}, "preview must not eagerly preload the full video")
    check("controls" in video and "playsinline" in video and "autoplay" not in video, "preview video must use native controls, inline playback, and no autoplay")
    check(video.get("aria-label") and video.get("aria-describedby") == "preview-caption", "preview video needs an accessible name and caption relationship")
    check("/media/stoke-preview.mp4" in home, "preview video source missing")
    check("Product preview · sample data" in combined and "not a live Stoke Cloud session" in combined, "preview status labeling missing")
    for filename in EXPECTED_MEDIA:
        check(f"/media/{filename}" in combined, f"expected preview asset not referenced: {filename}")
    check("data-flow-demo" not in combined and "illustrative product flow" not in combined.lower(), "old abstract flow demo remains")
    check("first 10" in combined.lower() and "30 days" in combined, "pilot offer missing")
    check("not deployed yet" in combined.lower(), "Cloud deployment boundary missing")
    check("macOS Apple Silicon" in cloud, "Apple Silicon preview boundary missing")
    check("not notarized by Apple" in cloud and "Gatekeeper" in cloud and "per-file approval" in cloud, "explicit unnotarized preview boundary missing")
    check("owned compute" in cloud.lower() and "provider bills" in cloud.lower(), "owned-compute/provider-bill boundary missing")
    check("cloud.stokegate.com" not in combined, "dead Cloud domain leaked into public CTA")
    check("prefers-reduced-motion" in css, "reduced motion CSS missing")
    check("data-nav-toggle" in home and "aria-expanded" in home, "responsive navigation hooks missing")
    check("public site should" not in combined.lower(), "internal meta-copy leaked into public page")
    check("<form" not in home.lower(), "homepage must not add a fake signup form")
    check("id=\"waitlist\"" in cloud and "action=\"/api/waitlist\"" in cloud and "method=\"post\"" in cloud, "waitlist form contract missing")
    check('name="email"' in cloud and 'type="email"' in cloud and 'name="persona"' in cloud and 'value="solo"' in cloud and 'value="team"' in cloud, "waitlist identity/persona fields missing")
    check('name="use_case"' in cloud and 'maxlength="800"' in cloud and 'name="consent"' in cloud and 'value="yes"' in cloud and 'name="notice_version"' in cloud and 'value="waitlist-v2"' in cloud, "waitlist consent/description contract missing")
    check('name="website"' in cloud and 'class="honeypot"' in cloud and 'href="/api/waitlist"' in cloud and "<noscript>" in cloud, "waitlist privacy/no-JS contract missing")
    waitlist_js = (ROOT / "newwaitlist.js").read_text()
    check("?format=json" in waitlist_js and "config.open === true" in waitlist_js and "fields.disabled = true" in waitlist_js, "waitlist must fail closed while bootstrapping")
    check("new URLSearchParams(new FormData(form))" in waitlist_js and 'response.status !== 202' in waitlist_js and "not acceptance" in waitlist_js, "waitlist pending/approval handling missing")
    check("console." not in waitlist_js and "sendBeacon" not in waitlist_js and "data-track" not in waitlist_js, "waitlist must not add analytics or console logging")
    legal = (ROOT / "legal/index.html").read_text()
    check("email address" in legal and "solo/team persona" in legal and "use-case description" in legal and "consent" in legal, "legal waitlist data disclosure missing")
    check("/api/waitlist" in legal and "90 days" in legal and "daily cleanup" in legal and "Cloudflare" in legal and "hashed anti-abuse" in legal, "server privacy notice link/boundaries missing")
    check("controller_name" not in cloud and "privacy_email" not in waitlist_js and "data-notice-controller" not in cloud and "data-notice-email" not in cloud, "waitlist must not expose identity/contact notice fields")
    check("invitation" not in cloud.lower() and "invitation" not in waitlist_js.lower() and "approval does not automatically grant, start, or email Cloud access" in cloud, "waitlist must not promise automatic access")
    check(local_target("/api/waitlist") is None and local_target("/api/unknown") == ROOT / "api" / "unknown", "dynamic route allowlist is too broad")
    check(len(re.findall(r'<tr><td>\d{2}</td>', gateway)) == 24, "gateway matrix is not 24 rows")
    check("/cloud/" in (ROOT / "sitemap.xml").read_text() and "/gateway/" in (ROOT / "sitemap.xml").read_text(), "new routes missing from sitemap")
    check("pilot availability/details" in combined.lower(), "pilot availability CTA missing")
    check("open a public bug report" in contact.lower() and "github.com/Ozperium/stoke/issues/new" in contact, "contact route lacks bug-report GitHub CTA")
    check("manual review" in contact.lower() and 'data-track="cloud_invitation_requested"' not in contact, "contact route lacks conditional waitlist guidance")
    events = (ROOT.parent / "functions" / "events.js").read_text()
    check("cloud_invitation_requested" in events and "body.event" in events, "invitation event is not allowlisted/validated")

    parse_sitemap(ROOT / "sitemap.xml")
    og = ROOT / "og-image.png"
    check(og.is_file() and og.stat().st_size > 1000, "OG PNG missing or unexpectedly small")
    svg = (ROOT / "og-image.svg").read_text()
    check('width="1200"' in svg and 'height="630"' in svg, "OG SVG dimensions missing")

    missing_media = sorted(filename for filename in EXPECTED_MEDIA if not (ROOT / "media" / filename).is_file())
    if missing_media:
        media_note = "media pending parent copy: " + ", ".join(missing_media)
    else:
        media_note = "media files present and referenced"
    print(f"STATIC CHECKS PASSED: {len(pages)} pages, 24 gateway capabilities, preview/accessibility/claims/sitemap/XML/JSON-LD/links verified; {media_note}")


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, json.JSONDecodeError, ET.ParseError) as exc:
        print(f"STATIC CHECKS FAILED: {exc}", file=sys.stderr)
        raise SystemExit(1)
