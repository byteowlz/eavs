#!/usr/bin/env python3
"""Public mount/update/destroy seam, actual shared-engine themes, and async safety proof."""
import argparse
from urllib.request import urlopen, Request
from urllib.error import HTTPError
from playwright.sync_api import sync_playwright, expect


def stage(root, source='pi', provider='openai'):
    root.locator(f'[data-port="{source}"]').click()
    root.locator(f'[data-port="{provider}"]').click()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', default='http://127.0.0.1:5281')
    parser.add_argument('--react-proof', action='store_true', help='Require the optional built proof using existing React packages')
    args = parser.parse_args()
    base = args.base.rstrip('/')
    for private in ['/serve.py', '/src/control-room.js', '/index.d.ts', '/../Cargo.toml']:
        try:
            urlopen(base + private, timeout=5)
            raise AssertionError('Private path exposed')
        except HTTPError as error:
            assert error.code == 404
    try:
        urlopen(Request(base + '/', data=b'fixture', method='POST'), timeout=5)
        raise AssertionError('Write method accepted')
    except HTTPError as error:
        assert error.code == 501
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={'width': 1440, 'height': 1200})
        errors, requests = [], []
        page.add_init_script('''const NativeObserver=window.ResizeObserver;window.activeRoomObservers=new Set();window.ResizeObserver=class extends NativeObserver {observe(element){window.activeRoomObservers.add(this);return super.observe(element);}disconnect(){window.activeRoomObservers.delete(this);return super.disconnect();}};''')
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.on('request', lambda request: requests.append((request.method, request.url, request.resource_type)))
        page.goto(base + '/')
        page.wait_for_load_state('networkidle')
        root = page.locator('[data-mount]')
        expect(root.locator('.node')).to_have_count(9)
        expect(root.locator('[data-target-count]')).to_contain_text('0 explicitly')
        expect(root.locator('[data-action="preview"]')).to_be_disabled()
        expect(root.locator('[data-target="travel"]')).to_be_disabled()
        stage(root)
        root.locator('[data-target="east"]').check()
        root.locator('[data-target="west"]').check()
        expect(root.locator('[data-draft-count]')).to_have_text('1')
        root.locator('[data-action="preview"]').click()
        expect(root.locator('dialog')).to_be_visible()
        expect(root.locator('.table tbody tr')).to_have_count(2)
        expect(root.locator('.table')).not_to_contain_text('eavs-travel')
        expect(root.locator('.table')).to_contain_text('sample-r17')
        root.locator('[data-action="simulate"]').click()
        expect(root.locator('[data-receipts]')).to_contain_text('Simulated / applied')
        expect(root.locator('[data-receipts]')).to_contain_text('Simulated / conflict')
        expect(root.locator('[data-action="simulate"]')).to_be_disabled()
        page.keyboard.press('Escape')
        expect(root.locator('dialog')).not_to_be_visible()
        stage(root, 'govnr', 'local')
        root.locator('[data-action="preview"]').click()
        expect(root.locator('.table')).to_contain_text('Provider absent')
        root.locator('[data-action="simulate"]').click()
        expect(root.locator('[data-receipts]')).to_contain_text('Simulated / skipped')
        page.keyboard.press('Escape')
        root.locator('[data-action="undo"]').click()
        expect(root.locator('[data-draft-count]')).to_have_text('1')
        root.locator('[data-tenant]').select_option('sandbox')
        expect(root.locator('[data-target-count]')).to_contain_text('0 explicitly')
        expect(root.locator('[data-draft-count]')).to_have_text('0')
        stage(root, 'explorer')
        root.locator('[data-target="sandbox"]').check()
        root.locator('[data-tenant]').select_option('studio')
        expect(root.locator('[data-draft-count]')).to_have_text('1')
        expect(root.locator('[data-target="west"]')).to_be_checked()
        root.locator('[data-port="mac"]').focus()
        page.keyboard.press('Enter')
        root.locator('[data-port="openai"]').focus()
        page.keyboard.press('Enter')
        expect(root.locator('[data-draft-count]')).to_have_text('2')
        card = root.locator('[data-node="mac"]')
        before = card.bounding_box()
        page.mouse.move(before['x'] + 40, before['y'] + 35)
        page.mouse.down()
        page.mouse.move(before['x'] + 65, before['y'] + 50, steps=8)
        page.mouse.up()
        assert card.bounding_box()['x'] > before['x'] + 20
        root.locator('[data-action="fit"]').click()
        print('PASS standalone: explicit targets, typed scopes, isolated tenants, snapshot pins, independent simulation receipts, undo/keyboard/drag')

        # Two simultaneous modules: independent DOM queries, dialogs, intent state and cleanup.
        page.evaluate('''() => {
          const demo=window.controlRoomDemo;
          window.bodyBefore=document.body.getAttribute('style');window.htmlBefore=document.documentElement.getAttribute('style');
          const second=document.createElement('div');second.id='second-room';demo.shell.append(second);
          window.secondModule=demo.mountControlRoom(second,{inventory:demo.inventory});
        }''')
        second = page.locator('#second-room')
        expect(second.locator('.node')).to_have_count(9)
        assert page.evaluate('window.activeRoomObservers.size===2')
        expect(second.locator('[data-draft-count]')).to_have_text('0')
        stage(second, 'govnr', 'openai')
        second.locator('[data-target="east"]').check()
        expect(root.locator('[data-draft-count]')).to_have_text('2')
        second.locator('[data-action="preview"]').click()
        expect(second.locator('dialog')).to_be_visible()
        expect(second.locator('[data-action="simulate"]')).to_be_disabled()
        page.keyboard.press('Escape')
        page.evaluate('window.secondModule.destroy();window.secondModule.destroy()')
        expect(second.locator('.node')).to_have_count(0)
        assert page.evaluate('window.activeRoomObservers.size===1')
        expect(root.locator('.node')).to_have_count(9)
        assert page.evaluate("document.body.getAttribute('style')===window.bodyBefore && document.documentElement.getAttribute('style')===window.htmlBefore")
        print('PASS multiple mounts: isolated state and queries; idempotent destroy; read-only no apply; body/root styles unchanged')

        # Uncooperative async adapters deliberately ignore cancellation; late results must still be discarded.
        page.evaluate('''() => {
          const demo=window.controlRoomDemo;
          window.originalClient=demo.createSimulationClient(demo.inventory);
          demo.module.update({client:{mode:'read-only',preview:(plan,{signal})=>new Promise(resolve=>{
            window.pendingPlan=plan;window.pendingSignal=signal;window.resolveLate=resolve;
          })}});
        }''')
        root.locator('[data-action="preview"]').click()
        expect(root.locator('[data-message]')).to_contain_text('Checking')
        root.locator('[data-target="west"]').uncheck()
        assert page.evaluate('window.pendingSignal.aborted')
        page.evaluate('''async () => window.resolveLate(await window.originalClient.preview(window.pendingPlan))''')
        expect(root.locator('dialog')).not_to_be_visible()
        page.evaluate('''() => controlRoomDemo.module.update({client:{mode:'read-only',preview:async()=>{throw new Error('SYNTHETIC-PRIVATE-ERROR')}}})''')
        root.locator('[data-action="preview"]').click()
        expect(root.locator('[data-message]')).to_contain_text('Preview rejected')
        expect(root).not_to_contain_text('SYNTHETIC-PRIVATE-ERROR')
        # Inventory replacement invalidates all tenants, even when a provider revision changes under the same version.
        page.evaluate('''() => {
          const next=structuredClone(controlRoomDemo.inventory);next.tenants[0].instances[0].revision='sample-new-revision';
          controlRoomDemo.module.update({inventory:next});
        }''')
        expect(root.locator('[data-draft-count]')).to_have_text('0')
        expect(root.locator('[data-target-count]')).to_contain_text('0 explicitly')
        print('PASS async/inventory safety: stale promise discarded, abort observed, error bodies private, changed snapshot clears stale intents/targets')

        # A late simulated receipt cannot land in a different tenant, even from an adapter ignoring abort.
        page.evaluate('''() => {
          const demo=controlRoomDemo;const actual=demo.createSimulationClient(demo.inventory);window.receiptClient=actual;
          demo.module.update({inventory:demo.inventory,client:{mode:'simulation',preview:actual.preview,simulate:(plan,{signal})=>new Promise(resolve=>{window.receiptPlan=plan;window.receiptSignal=signal;window.resolveReceipt=resolve;})}});
        }''')
        stage(root)
        root.locator('[data-target="east"]').check()
        root.locator('[data-action="preview"]').click()
        expect(root.locator('dialog')).to_be_visible()
        root.locator('[data-action="simulate"]').click()
        page.evaluate("controlRoomDemo.module.update({tenantId:'sandbox'})")
        assert page.evaluate('window.receiptSignal.aborted')
        page.evaluate('''async () => window.resolveReceipt(await window.receiptClient.simulate(window.receiptPlan))''')
        expect(root.locator('dialog')).not_to_be_visible()
        expect(root).not_to_contain_text('Simulated / applied')
        expect(root.locator('[data-tenant]')).to_have_value('sandbox')
        # Even an otherwise well-shaped client verdict cannot upgrade a locally missing declaration.
        page.evaluate('''() => controlRoomDemo.module.update({tenantId:'studio',inventory:controlRoomDemo.inventory,client:{mode:'read-only',preview:async(plan)=>({kind:'preview',inventoryVersion:plan.inventoryVersion,tenantId:plan.tenantId,plan,rows:plan.targets.map(target=>({id:target.id,status:'inventory-ready'}))})}})''')
        root.locator('[data-action="discard"]').click()
        stage(root, 'govnr', 'local')
        root.locator('[data-target="east"]').uncheck()
        root.locator('[data-target="west"]').check()
        root.locator('[data-action="preview"]').click()
        expect(root.locator('[data-message]')).to_contain_text('Preview rejected')
        expect(root.locator('dialog')).not_to_be_visible()
        print('PASS stale receipts and dishonest verdict: no cross-tenant completion; absent provider cannot become eligible')

        # Hostile labels are text, never markup; control-room public API accepts no live client mode.
        page.evaluate('''() => {
          const next=structuredClone(controlRoomDemo.inventory);
          next.tenants[0].resources[0].name='<img src=x onerror="window.__xss=1">';
          next.tenants[0].resources[0].apiKey='SYNTHETIC-PRIVATE-KEY';
          controlRoomDemo.module.update({inventory:next});
        }''')
        expect(root.locator('[data-node="mac"]')).to_contain_text('<img src=x')
        expect(root.locator('img')).to_have_count(0)
        expect(root).not_to_contain_text('SYNTHETIC-PRIVATE-KEY')
        assert page.evaluate('!window.__xss')
        assert page.evaluate("(() => {try {controlRoomDemo.module.update({client:{mode:'live',preview:()=>{}}});return false;}catch{return true;}})()")
        print('PASS input boundaries: escaped labels, ignored credential fields, unsupported live-write mode rejected')

        # Clean up in-flight work and detached event handlers; remount with no duplicated roots.
        page.evaluate('''() => {
          const demo=controlRoomDemo;window.intentCalls=0;
          demo.module.update({inventory:demo.inventory,onIntent:()=>window.intentCalls++,client:{mode:'read-only',preview:(plan,{signal})=>new Promise(resolve=>{window.pendingPlan=plan;window.pendingSignal=signal;window.resolveLate=resolve;})}});
        }''')
        stage(root)
        root.locator('[data-target="east"]').check()
        root.locator('[data-action="preview"]').click()
        page.evaluate('''() => {
          window.detachedPort=controlRoomDemo.root.querySelector('.eavs-control-room').shadowRoot.querySelector('[data-port="mac"]');
          window.beforeDestroyCalls=window.intentCalls;
          controlRoomDemo.module.destroy();window.detachedPort.click();
        }''')
        assert page.evaluate('window.pendingSignal.aborted && window.intentCalls===window.beforeDestroyCalls && window.activeRoomObservers.size===0')
        page.evaluate('''async () => window.resolveLate(await window.originalClient.preview(window.pendingPlan))''')
        expect(root.locator('.node')).to_have_count(0)
        page.locator('[data-lifecycle]').click()
        page.locator('[data-lifecycle]').click()
        expect(root.locator('.node')).to_have_count(9)
        print('PASS lifecycle: in-flight cancellation and late response guard, detached handlers removed, destroy/remount no duplicate UI')

        page.goto(base + '/embedded.html')
        page.wait_for_load_state('networkidle')
        root = page.locator('[data-mount]')
        navigation = page.locator('.host-navigation').inner_html()
        def computed(selector, property):
            return root.locator(selector).first.evaluate('(el,p)=>getComputedStyle(el)[p]', property)
        assert computed('.node', 'borderRadius') == '0px'
        assert computed('.port', 'borderRadius') == '0px'
        original = computed('.primary', 'backgroundColor')
        page.locator('[data-scheme]').select_option('oqto-light')
        assert computed('.primary', 'backgroundColor') != original
        page.locator('[data-radius]').fill('12')
        page.locator('[data-radius]').dispatch_event('input')
        assert computed('.node', 'borderRadius') == '9px'
        assert computed('.port', 'borderRadius') == '6px'
        page.locator('[data-scheme]').select_option('nord')
        assert page.evaluate("getComputedStyle(controlRoomDemo.shell).getPropertyValue('--base17').trim().length > 0")
        stage(root)
        root.locator('[data-target="east"]').check()
        root.locator('[data-action="preview"]').click()
        expect(root.locator('[data-action="simulate"]')).to_be_disabled()
        page.keyboard.press('Escape')
        page.locator('[data-lifecycle]').click()
        expect(root.locator('.node')).to_have_count(0)
        assert page.locator('.host-navigation').inner_html() == navigation
        page.locator('[data-lifecycle]').click()
        expect(root.locator('.node')).to_have_count(9)
        assert page.locator('.host-navigation').inner_html() == navigation
        print('PASS embedded: actual shared-engine scheme/base16 derivation, live inherited tokens, sharp-at-zero/proportional radius, host navigation untouched, read-only default')
        assert not errors, errors
        assert all(method == 'GET' and url.startswith(base + '/') and kind not in ('fetch','xhr','websocket') for method,url,kind in requests), requests
        for path in ['/', '/embedded.html']:
            page.set_viewport_size({'width':390,'height':844})
            page.goto(base + path)
            page.wait_for_load_state('networkidle')
            root = page.locator('[data-mount]')
            root.locator('[data-tenant]').select_option('sandbox')
            stage(root, 'explorer')
            root.locator('[data-target="sandbox"]').check()
            root.locator('[data-action="preview"]').click()
            expect(root.locator('dialog')).to_be_visible()
            page.keyboard.press('Escape')
        print('PASS desktop/mobile, read-only host and no fetch/XHR/WebSocket/non-GET traffic')
        if args.react_proof:
            page.goto(base + '/react-proof.html')
            page.wait_for_load_state('networkidle')
            react = page.locator('.react-room')
            expect(react.locator('.node')).to_have_count(9)
            expect(page.locator('[data-react-count]')).to_have_text('0')
            stage(react)
            expect(react.locator('[data-draft-count]')).to_have_text('1')
            expect(page.locator('[data-react-count]')).to_have_text('1')
            react.locator('[data-target="east"]').check()
            expect(page.locator('[data-react-count]')).to_have_text('2')
            react.locator('[data-action="preview"]').click()
            expect(react.locator('dialog')).to_be_visible()
            expect(react.locator('[data-action="simulate"]')).to_be_disabled()
            page.keyboard.press('Escape')
            page.locator('[data-react-toggle]').click()
            expect(page.locator('.node')).to_have_count(0)
            page.locator('[data-react-toggle]').click()
            expect(page.locator('.node')).to_have_count(9)
            expect(page.locator('[data-draft-count]')).to_have_text('0')
            expect(page.locator('[data-target-count]')).to_contain_text('0 explicitly')
            assert not errors, errors
            assert all(method == 'GET' and url.startswith(base + '/') and kind not in ('fetch','xhr','websocket') for method,url,kind in requests)
            print('PASS actual React StrictMode bridge: no duplicate mounts, host callback rerenders preserve drafts, readonly preview, unmount/remount cleanup')
        browser.close()

if __name__ == '__main__':
    main()
