#!/usr/bin/env python3
"""Offline UI proof; needs an already running serve.py and Python Playwright."""
import argparse
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from playwright.sync_api import sync_playwright, expect

NAMES = ['control-room', 'route-lanes', 'patch-bay', 'tenant-atlas', 'change-desk']

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='http://127.0.0.1:5280')
    parser.add_argument('--screenshots', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    base = args.base.rstrip('/')
    for forbidden in ['/serve.py', '/test_mockups.py', '/README.md', '/../Cargo.toml', '/src/setup.rs']:
        try:
            urlopen(base + forbidden, timeout=5)
            raise AssertionError('Host exposed a non-public path')
        except HTTPError as error:
            assert error.code == 404
    try:
        urlopen(Request(base + '/control-room.html', data=b'synthetic', method='POST'), timeout=5)
        raise AssertionError('Read-only host accepted a write')
    except HTTPError as error:
        assert error.code == 501
    with urlopen(base + '/', timeout=5) as response:
        assert "connect-src 'none'" in response.headers['Content-Security-Policy']
        assert response.headers['X-Content-Type-Options'] == 'nosniff'
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        for name in NAMES:
            page = browser.new_page(viewport={'width': 1440, 'height': 1320})
            errors, requests = [], []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.on('request', lambda request: requests.append((request.method, request.url, request.resource_type)))
            page.goto(base + '/' + name + '.html')
            page.wait_for_load_state('networkidle')
            page.evaluate('document.fonts.ready')
            expect(page.locator('.node')).to_have_count(9)
            expect(page.locator('.workspace-node')).to_have_count(2)
            expect(page.locator('.wire')).to_have_count(7)
            expect(page.locator('input[data-target="east"]')).to_be_checked()
            expect(page.locator('input[data-target="west"]')).not_to_be_checked()
            expect(page.locator('input[data-target="travel"]')).to_be_disabled()
            for card in page.locator('.node').all():
                rect = card.bounding_box()
                graph = page.locator('#graph').bounding_box()
                assert rect['x'] >= graph['x'] - 1
                assert rect['x'] + rect['width'] <= graph['x'] + graph['width'] + 1
                assert rect['y'] + rect['height'] <= graph['y'] + graph['height'] + 1
            if name == 'tenant-atlas':
                inspector = page.locator('.inspector').bounding_box()
                for provider in page.locator('.node.provider').all():
                    card = provider.bounding_box()
                    assert card['x'] + card['width'] < inspector['x'], 'Inspector covers provider'
            if args.screenshots:
                page.locator('#app').screenshot(path=str(root / 'assets' / (name + '.png')))
            page.locator('[data-action="discard"]').click()
            expect(page.locator('#draft-count')).to_have_text('0')
            page.locator('input[data-target="east"]').uncheck()
            page.locator('.plan-bar [data-action="preview"]').click()
            expect(page.locator('#preview-dialog')).not_to_be_visible()
            expect(page.locator('#toast')).to_contain_text('Select at least one')
            page.locator('input[data-target="east"]').check()
            page.locator('input[data-target="west"]').check()
            for source, provider in [('m1', 'p1'), ('w1', 'p2')]:
                page.locator(f'[data-port="{source}"]').click()
                page.locator(f'[data-port="{provider}"]').click()
            expect(page.locator('#draft-count')).to_have_text('2')
            expect(page.locator('.wire.draft')).to_have_count(2)
            page.locator('.plan-bar [data-action="preview"]').click()
            expect(page.locator('.preview-table tbody tr')).to_have_count(2)
            expect(page.locator('.preview-table')).not_to_contain_text('eavs-travel')
            expect(page.locator('.preview-table')).to_contain_text('mock-r17')
            expect(page.locator('.preview-table')).to_contain_text('mock-r09')
            page.locator('#simulate-apply').click()
            expect(page.locator('#receipts')).to_contain_text('Applied / sample receipt')
            expect(page.locator('#receipts')).to_contain_text('Conflict / refresh required')
            expect(page.locator('#simulate-apply')).to_be_disabled()
            expect(page.locator('#draft-count')).to_have_text('2')
            page.locator('dialog [data-close]').last.click()
            page.locator('[data-port="h1"]').click()
            page.locator('[data-port="p3"]').click()
            expect(page.locator('#draft-count')).to_have_text('3')
            page.locator('.plan-bar [data-action="preview"]').click()
            expect(page.locator('.preview-table')).to_contain_text('Blocked: local provider absent')
            page.locator('#simulate-apply').click()
            expect(page.locator('#receipts')).to_contain_text('Skipped / provider absent')
            page.keyboard.press('Escape')
            expect(page.locator('#preview-dialog')).not_to_be_visible()
            page.locator('[data-action="undo"]').click()
            expect(page.locator('#draft-count')).to_have_text('2')
            page.locator('[data-port="m1"]').click()
            page.locator('[data-port="p1"]').click()
            expect(page.locator('#draft-count')).to_have_text('2')
            expect(page.locator('#toast')).to_contain_text('already connected')
            card = page.locator('[data-node="m1"]')
            before = card.bounding_box()
            page.mouse.move(before['x'] + 45, before['y'] + 35)
            page.mouse.down()
            page.mouse.move(before['x'] + 75, before['y'] + 55, steps=8)
            page.mouse.up()
            after = card.bounding_box()
            assert after['x'] > before['x'] + 20
            page.locator('[data-action="reset-layout"]').click()
            expect(page.locator('#draft-count')).to_have_text('2')
            page.locator('#tenant').select_option('lab')
            expect(page.locator('input[data-target="lab-dev"]')).to_be_checked()
            expect(page.locator('input[data-target="east"]')).to_have_count(0)
            expect(page.locator('#draft-count')).to_have_text('1')
            page.locator('[data-action="discard"]').click()
            page.locator('#tenant').select_option('studio')
            expect(page.locator('#draft-count')).to_have_text('2')
            expect(page.locator('input[data-target="west"]')).to_be_checked()
            # Native button keyboard activation must not be swallowed by the card role.
            page.locator('[data-port="m2"]').focus()
            page.keyboard.press('Enter')
            page.locator('[data-port="p2"]').focus()
            page.keyboard.press('Enter')
            expect(page.locator('#draft-count')).to_have_text('3')
            page.locator('[data-node="h2"]').click()
            if name == 'change-desk':
                page.locator('.composer summary').click()
            page.locator('#provider-choice').select_option('p1')
            expect(page.locator('#model-choice')).to_have_value('claude-haiku-5-5')
            page.locator('[data-action="stage"]').click()
            expect(page.locator('#draft-count')).to_have_text('4')
            assert not errors, errors
            assert all(method == 'GET' and url.startswith(base + '/') and kind not in ('fetch', 'xhr', 'websocket') for method, url, kind in requests), requests
            print(f'PASS {name}: nodes, all source types, target scope, isolated tenants, CAS/missing-provider receipts, keyboard, drag, dedup, undo, no API calls')
            page.close()
        page = browser.new_page(viewport={'width': 390, 'height': 844})
        for name in NAMES:
            page.goto(base + '/' + name + '.html')
            page.wait_for_load_state('networkidle')
            expect(page.locator('.node')).to_have_count(9)
            page.locator('#tenant').select_option('sandbox')
            expect(page.locator('input[data-target="sandbox"]')).to_be_checked()
            page.locator('.plan-bar [data-action="preview"]').click()
            expect(page.locator('#preview-dialog')).to_be_visible()
            expect(page.locator('.preview-table tbody tr')).to_have_count(1)
            page.keyboard.press('Escape')
            print(f'PASS mobile {name}: scrollable graph, tenant switch, single-target preview')
        page.goto(base + '/')
        page.wait_for_load_state('networkidle')
        expect(page.locator('.g-card')).to_have_count(5)
        assert page.locator('.g-card img').evaluate_all('(images) => images.every(image => image.complete && image.naturalWidth > 0)')
        browser.close()
    print('PASS gallery and read-only host: five previews, private source paths denied, write methods denied, self-hosted assets and connect-src none')

if __name__ == '__main__':
    main()
