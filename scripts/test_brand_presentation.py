#!/usr/bin/env python3
"""Portable contracts for the homepage presentation additions."""
from html.parser import HTMLParser
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
LANDING = ROOT / "landing"


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.tags = []

    def handle_starttag(self, tag, attrs):
        self.tags.append((tag, dict(attrs)))


def parse(path):
    page = Page()
    page.feed(path.read_text())
    return page


home_path = LANDING / "index.html"
cloud_path = LANDING / "cloud" / "index.html"
home = parse(home_path)
cloud = parse(cloud_path)
home_html = home_path.read_text()
cloud_html = cloud_path.read_text()

for path, html, page in ((home_path, home_html, home), (cloud_path, cloud_html, cloud)):
    assert '/product-visual.css' in html and '/brand-presentation.css' in html, path
    assert html.count('class="log-panel-surface"') == 1, f"one log surface required: {path}"
    assert 'M3 3 L25 1 L50 0' in html and 'L100 50' in html and 'L50 100' in html and 'L0 50' in html
    assert 'brand-period' not in html, f"trailing brand period remains: {path}"
    assert 'class="product-visual__note"' not in html, 'removed visible caption returned'
    assert 'class="sr-only" id="visual-context"' in html
    assert 'aria-describedby="visual-context"' in html
    assert 'style=' not in html.lower(), f"inline style violates CSP: {path}"
    brands = [attrs for tag, attrs in page.tags if tag == 'a' and attrs.get('class') == 'brand']
    assert len(brands) == 2, f"header/footer brand count changed: {path}"
    assert html.count('<span class="brand-mark" aria-hidden="true"><img src="/stoke-mark.svg" alt="" width="24" height="24"></span>stoke') == 2

gateway_html = (LANDING / 'gateway' / 'index.html').read_text()
assert 'Release scope:' not in gateway_html and 'Read it in context' not in gateway_html, 'removed editorial boilerplate returned'

assert 'class="provider-strip"' in home_html
assert 'OpenAI API' in home_html and 'Anthropic API' in home_html and 'Ollama' in home_html
assert 'class="provider-strip"' not in cloud_html, 'provider row must stay homepage-only'
assert 'data-ember-flow' in home_html and '/ember-flow.mjs' in home_html
assert '/burning-numbers.mjs' not in home_html and 'class="burning-numbers"' not in home_html
assert '/hero-atmosphere.css' in home_html and '/ember-flow.css' in home_html
assert 'data-burning-numbers' not in cloud_html and '/burning-numbers.mjs' not in cloud_html
assert 'class="burning-toggle"' in home_html

css = (LANDING / 'product-visual.css').read_text()
assert '.product-visual__core::before { content: none; }' in css
assert '.log-panel-surface {' in css and 'pointer-events: none' in css

controller = (LANDING / 'ember-flow.mjs').read_text()
for marker in ('wakeDistances', 'ResizeObserver', 'MutationObserver', 'IntersectionObserver', 'visibilitychange', 'cancelAnimationFrame', 'prefers-reduced-motion', 'data-phase'):
    assert marker in controller, f'missing burning-number behavior: {marker}'
assert "querySelectorAll('[data-route]')" in controller, 'effect must use actual request routes'
assert 'state.visiblePaths.includes(packet.path)' in controller
assert 'window.flowStudy' not in controller and "get('flow')" not in controller
assert 'digits' not in controller, 'only the accepted ember option ships'
assert '@keyframes' not in (LANDING / 'hero-atmosphere.css').read_text(), 'background stays static'
assert 'PHASE_OFFSET' not in controller, 'do not add independent request streams'
assert 'pageshow' in controller, 'restore animation after page-cache return'
assert 'getBoundingClientRect()' in controller
assert re.search(r'function tick\(now\).*?getBoundingClientRect', controller, re.S) is None, 'animation tick must not read layout'
assert 'window.burningStudy' not in controller and 'window.hearthStudy' not in controller

for icon in ('openai.svg', 'anthropic.svg', 'ollama.svg', 'LobeHub-LICENSE.txt', 'sources.json'):
    assert (LANDING / 'brands' / icon).is_file(), f'missing public-safe brand provenance asset: {icon}'

import hashlib
import json
for asset in json.loads((LANDING / 'brands' / 'sources.json').read_text()):
    assert hashlib.sha256((LANDING / 'brands' / asset['file']).read_bytes()).hexdigest() == asset['sha256'], asset['file']

import subprocess
subprocess.run(['node', '--test', str(ROOT / 'scripts/test_burning_numbers.mjs')], check=True)

subprocess.run(['node', '--test', str(ROOT / 'scripts/test_ember_flow.mjs')], check=True)
subprocess.run(['python3', str(ROOT / 'scripts/test_stoke_mark.py')], check=True)
print('brand presentation checks passed: homepage-only providers/ember route, static atmosphere, shared log surface, CSP, lifecycle, and provenance')
