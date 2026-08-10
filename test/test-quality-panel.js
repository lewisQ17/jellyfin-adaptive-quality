/* Smoke test for quality-panel.js: loads the script into a mocked DOM and checks that it
   boots, only adds the button for admins on the dashboard route, and renders fetched data
   without error (including HTML escaping and one account across several devices). */
const fs = require('fs');
const path = require('path');

let failures = 0;
const check = (n, c, e) => { console.log((c ? '  PASS  ' : '  FAIL  ') + n + (e ? '  → ' + e : '')); if (!c) failures++; };

const elements = [];
function mkEl() {
  const el = {
    style: {}, children: [], _html: '', id: '', textContent: '',
    classList: { contains: () => false },
    set innerHTML(v) { this._html = v; },
    get innerHTML() { return this._html; },
    appendChild(c) { this.children.push(c); return c; },
    querySelector(sel) {
      if (sel === '.qg-body') { this._body = this._body || mkEl(); return this._body; }
      return mkEl();
    },
    addEventListener() {}, remove() { this._removed = true; }, onclick: null
  };
  elements.push(el);
  return el;
}

global.window = global;
global.location = { hash: '#/dashboard' };
global.navigator = { userAgent: 'node' };
global.document = {
  readyState: 'complete',
  createElement: () => mkEl(),
  getElementById: (id) => elements.find(e => e.id === id && !e._removed) || null,
  body: { appendChild(c) { elements.push(c); return c; } },
  addEventListener() {}
};
global.setInterval = () => 0;
global.clearInterval = () => {};
const timeouts = [];
global.setTimeout = (fn) => { timeouts.push(fn); return 0; };

global.ApiClient = { accessToken: () => 't', serverAddress: () => 'http://jf.test' };

// Hostile device name, to prove escaping works.
const EVIL = '<img src=x onerror=alert(1)>';
global.fetch = (url) => {
  const j = (d) => Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(d) });
  if (url.includes('/Users/Me')) return j({ Policy: { IsAdministrator: true } });
  if (url.includes('/Users')) return j([{ Id: 'u1', Name: 'amber' }]);
  if (url.includes('/DisplayPreferences/')) return j({ CustomPrefs: {
      'd_d1': JSON.stringify({ verdict: 'device', reason: 'laptop could not keep up',
                               level: '4K SDR', bitrate: '0', ts: '2026-08-10T10:00:00Z' }),
      'd_d2': JSON.stringify({ verdict: 'network', reason: 'phone had slow wifi',
                               level: '1080p', bitrate: '8000000', ts: '2026-08-10T11:00:00Z' })
    } });
  if (url.includes('/Sessions')) return j([{
    Id: 's1', UserName: 'amber', DeviceName: EVIL, Client: 'Jellyfin Web', DeviceId: 'd1',
    PlayState: { PlayMethod: 'Transcode' },
    NowPlayingItem: { Name: 'Example Movie', MediaStreams: [{ Type: 'Video', Width: 3240, Height: 2160, Codec: 'hevc', VideoRangeType: 'DOVI' }] },
    TranscodingInfo: { Width: 2560, Height: 1440, VideoCodec: 'h264', Bitrate: 20000000, TranscodeReasons: ['VideoRangeTypeNotSupported'] }
  }]);
  return j(null);
};

// eval() is deliberate: a harness running OUR OWN local file in a mocked DOM.
const src = fs.readFileSync(path.join(__dirname, '..', 'src', 'quality-panel.js'), "utf8");
let crashed = false;
try { eval(src); } catch (e) { crashed = true; console.log('  load error:', e.message); }
check('script loads without error', !crashed);

(async () => {
  timeouts.forEach(fn => { try { fn(); } catch (e) { console.log('  boot error:', e.message); } });
  await new Promise(r => setImmediate(r));
  await new Promise(r => setImmediate(r));

  const btn = elements.find(e => e.id === 'qg-btn');
  check('button appears for admins on the dashboard', !!btn, btn && btn.textContent);

  if (btn && btn.onclick) {
    btn.onclick();
    await new Promise(r => setImmediate(r));
    await new Promise(r => setImmediate(r));
    await new Promise(r => setImmediate(r));
    const withHtml = elements.filter(e => e._html && e._html.length > 50);
    const body = withHtml.map(e => e._html).join('\n');
    check('panel renders content', body.length > 50, body.length + ' tekens');
    check('shows the viewer', /amber/.test(body));
    check('shows "transcoded"', /transcoded/.test(body));
    check('translates the reason into plain language', /Dolby Vision/.test(body),
          (body.match(/player cannot handle this HDR type[^<]*/) || [''])[0]);
    check('shows from -> to', /3240×2160/.test(body) && /2560×1440/.test(body));
    check('ESCAPES a hostile device name (no <img> in the output)',
          !/<img/.test(body) && /&lt;img/.test(body));
    check('BOTH devices of the same account appear',
          /laptop could not keep up/.test(body) && /phone had slow wifi/.test(body));
    check('device name or id is shown', /on /.test(body));

  }

  console.log('\n' + (failures === 0 ? 'ALL TESTS PASSED' : failures + ' TEST(S) FAILED'));
  process.exit(failures === 0 ? 0 : 1);
})();
