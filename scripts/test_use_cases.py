#!/usr/bin/env python3
"""Persona → use case → detail contracts, including the no-JavaScript path."""
from html.parser import HTMLParser
from pathlib import Path
import json
import re
import unittest
from urllib.parse import urlparse
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / 'landing'
CASES = {
    'local-models': ('Developers & small teams', 'Give your idle computer a job'),
    'private-data': ('Teams handling confidential data', 'Keep client documents off the cloud'),
    'agent-spend': ('Agent builders & agencies', 'Stop funding an agent’s retry loop'),
    'private-model-access': ('IT & internal AI teams', 'Connect apps without copying provider keys'),
    'node-failover': ('Platform & DevEx teams', 'A model host went offline. Now what?'),
    'secret-redaction': ('Engineering & support teams', 'The error log came with an API key'),
}


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.tags = []
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        self.tags.append((tag, dict(attrs)))


class UseCasesTests(unittest.TestCase):
    def test_every_page_has_static_persona_case_links(self):
        for path in sorted(LANDING.rglob('*.html')):
            with self.subTest(page=str(path.relative_to(LANDING))):
                raw = path.read_text()
                match = re.search(r'<details\b[^>]*data-use-cases[^>]*>.*?</details>', raw, re.S)
                if match is None:
                    self.fail('Use cases dropdown missing')
                menu = match.group()
                self.assertIn('<summary', menu)
                self.assertIn('Use cases', menu)
                for slug, (persona, title) in CASES.items():
                    self.assertIn(f'href="/use-cases/{slug}/"', menu)
                    self.assertIn(persona.replace('&', '&amp;'), menu)
                    self.assertIn(title, menu)
                self.assertIn('href="/use-cases/"', menu)
                self.assertEqual(len(re.findall(r'<a\b', menu)), 7)

    def test_index_and_exact_six_detail_routes(self):
        directory = LANDING / 'use-cases'
        self.assertTrue((directory / 'index.html').is_file())
        self.assertEqual({p.parent.name for p in directory.glob('*/index.html')}, set(CASES))
        index = (directory / 'index.html').read_text()
        for slug in CASES:
            self.assertIn(f'href="/use-cases/{slug}/"', index)

    def test_detail_pages_have_scenarios_boundaries_and_next_action(self):
        for slug, (persona, title) in CASES.items():
            with self.subTest(case=slug):
                path = LANDING / 'use-cases' / slug / 'index.html'
                self.assertTrue(path.is_file(), 'Detail route missing')
                raw = path.read_text()
                page = Page(raw)
                self.assertEqual(sum(tag == 'h1' for tag, _ in page.tags), 1)
                self.assertIn(title, raw)
                self.assertIn(persona.replace('&', '&amp;'), raw)
                self.assertGreaterEqual(sum(tag == 'h2' for tag, _ in page.tags), 3)
                for filler in ('A day-to-day case', 'With Stoke', 'Know the boundary', 'Try this setup'):
                    self.assertNotIn(f'>{filler}</h2>', raw)
                self.assertIn('href="/use-cases/"', raw)
                self.assertRegex(raw, r'href="/guides/[^\"]+/"')
                canonical = f'https://stokegate.com/use-cases/{slug}/'
                self.assertIn(f'rel="canonical" href="{canonical}"', raw)
                self.assertIn('aria-current="page"', raw)
                schemas = re.findall(r'<script type="application/ld\+json">(.*?)</script>', raw)
                self.assertEqual(len(schemas), 1)
                self.assertEqual(json.loads(schemas[0])['url'], canonical)
                self.assertNotIn('<form', raw)

    def test_unique_illustrations_and_social_metadata(self):
        import hashlib
        import struct
        social_hashes = set()
        for slug in CASES:
            with self.subTest(case=slug):
                raw = (LANDING / 'use-cases' / slug / 'index.html').read_text()
                page = Page(raw)
                images = [attrs for tag, attrs in page.tags if tag == 'img' and 'data-use-case-image' in attrs]
                self.assertEqual(len(images), 1, 'One real use-case illustration required')
                image = images[0]
                self.assertEqual((image['width'], image['height']), ('1152', '720'))
                self.assertTrue(image.get('alt'))
                self.assertIn(slug, image['src'])
                hero_path = LANDING / image['src'].lstrip('/')
                hero = hero_path.read_bytes()
                self.assertEqual(hero[:4], b'RIFF')
                self.assertEqual(hero[8:12], b'WEBP')
                metas = [(attrs.get('property') or attrs.get('name'), attrs.get('content')) for tag, attrs in page.tags if tag == 'meta']
                def value(key):
                    values = [content for name, content in metas if name == key]
                    self.assertEqual(len(values), 1, key)
                    return values[0]
                social = value('og:image')
                self.assertTrue(social.startswith('https://stokegate.com/media/use-cases/'))
                self.assertIn(slug, social)
                self.assertEqual(value('twitter:image'), social)
                self.assertEqual(value('og:image:type'), 'image/png')
                self.assertEqual((value('og:image:width'), value('og:image:height')), ('1200', '630'))
                self.assertEqual(value('twitter:card'), 'summary_large_image')
                self.assertEqual(value('og:image:alt'), value('twitter:image:alt'))
                self.assertIn(CASES[slug][1], value('og:image:alt'))
                png = (LANDING / urlparse(social).path.lstrip('/')).read_bytes()
                self.assertEqual(png[:8], b'\x89PNG\r\n\x1a\n')
                self.assertEqual(struct.unpack('>II', png[16:24]), (1200, 630))
                social_hashes.add(hashlib.sha256(png).hexdigest())
                schema = json.loads(re.findall(r'<script type="application/ld\+json">(.*?)</script>', raw)[0])
                self.assertEqual(schema['image'], social)
                self.assertNotIn('Concept illustration', raw)
                self.assertNotIn('not a product screenshot', raw)
                self.assertNotIn('<figcaption>', raw)
        self.assertEqual(len(social_hashes), 6, 'Every use case needs its own social card')

    def test_every_use_case_uses_editorial_art(self):
        for slug in CASES:
            with self.subTest(case=slug):
                raw = (LANDING / 'use-cases' / slug / 'index.html').read_text()
                self.assertIn('use-case-hero--editorial', raw)
                images = [attrs for tag, attrs in Page(raw).tags if tag == 'img' and 'data-use-case-image' in attrs]
                self.assertEqual(len(images), 1)
                self.assertNotIn('-v1.webp', images[0]['src'])

    def test_new_routes_are_in_generated_sitemap(self):
        # Parse only our generated XML; reject declarations before stdlib parsing.
        raw = (LANDING / 'sitemap.xml').read_text()
        self.assertNotRegex(raw, r'<!DOCTYPE\b|<!ENTITY\b')
        sitemap = ET.fromstring(raw)
        paths = {urlparse(n.text).path for n in sitemap.findall('.//{*}loc')}
        expected = {'/use-cases/'} | {f'/use-cases/{slug}/' for slug in CASES}
        self.assertTrue(expected <= paths)

    def test_security_and_cost_claims_keep_their_scope(self):
        private = (LANDING / 'use-cases/private-data/index.html').read_text()
        self.assertIn('pattern-based', private)
        self.assertIn('not complete DLP', private)
        self.assertIn('refused', private)
        cost = (LANDING / 'use-cases/agent-spend/index.html').read_text()
        self.assertIn('metered API', cost)
        self.assertIn('not subscription', cost)
        local = (LANDING / 'use-cases/local-models/index.html').read_text()
        self.assertIn('not combined GPU memory', local)
        self.assertIn('hardware and electricity', local)
        access = (LANDING / 'use-cases/private-model-access/index.html').read_text()
        self.assertIn('not per-user RBAC', access)
        self.assertIn('not subscription lending', access)
        failover = (LANDING / 'use-cases/node-failover/index.html').read_text()
        self.assertIn('before streaming begins', failover)
        self.assertIn('not seamless mid-stream recovery', failover)
        redaction = (LANDING / 'use-cases/secret-redaction/index.html').read_text()
        self.assertIn('not complete DLP', redaction)
        self.assertIn('synthetic', redaction)


if __name__ == '__main__':
    unittest.main()
