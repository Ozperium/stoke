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
    'local-models': ('Developers & small teams', 'Use models across your machines'),
    'private-data': ('Teams handling confidential data', 'Keep private requests private'),
    'agent-spend': ('Agent builders & agencies', 'Put limits around agent spend'),
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
                self.assertEqual(len(re.findall(r'<a\b', menu)), 4)

    def test_index_and_exact_three_detail_routes(self):
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
                for section in ('A day-to-day case', 'With Stoke', 'Know the boundary', 'Try this setup'):
                    self.assertIn(section, raw)
                self.assertIn('href="/use-cases/"', raw)
                self.assertRegex(raw, r'href="/guides/[^\"]+/"')
                canonical = f'https://stokegate.com/use-cases/{slug}/'
                self.assertIn(f'rel="canonical" href="{canonical}"', raw)
                self.assertIn('aria-current="page"', raw)
                schemas = re.findall(r'<script type="application/ld\+json">(.*?)</script>', raw)
                self.assertEqual(len(schemas), 1)
                self.assertEqual(json.loads(schemas[0])['url'], canonical)
                self.assertNotIn('<form', raw)

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


if __name__ == '__main__':
    unittest.main()
