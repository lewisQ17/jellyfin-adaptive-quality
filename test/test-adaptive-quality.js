/* Test harness for adaptive-quality.js: mocks a video element and the Jellyfin API and
   checks that the correct AXIS is repaired, that playback is never disturbed to raise
   quality, and that the known false-positive sources are ignored. */
const fs = require('fs');
const path = require('path');

let clock = 1000000, tick = null, watch = null;
const restarts = [];
const toasts = [];
let lastPlaybackInfoBody = null;
let probeRequests = [];
let hidden = false;

function mkVideo(opts) {
  return Object.assign({
    paused: false, readyState: 4, videoWidth: 3240, currentTime: 100, playbackRate: 1,
    _dropped: 0, _total: 0, _corrupt: 0, _ahead: 30, _ev: {},
    getVideoPlaybackQuality() {
      return { droppedVideoFrames: this._dropped, totalVideoFrames: this._total,
               corruptedVideoFrames: this._corrupt };
    },
    get buffered() {
      const self = this;
      return { length: 1, start: () => 0, end: () => self.currentTime + self._ahead };
    },
    addEventListener(ev, fn) { this._ev[ev] = fn; },
    fire(ev) { if (this._ev[ev]) this._ev[ev](); },
    remove() {}, pause() {}, play() { return Promise.resolve(); },
    removeAttribute() {}, load() {}
  }, opts || {});
}

const video = mkVideo();
let videos = [video];

global.window = global;
global.navigator = { userAgent: 'node-test' };
global.document = {
  readyState: 'complete',
  get hidden() { return hidden; },
  querySelectorAll: (s) => (s === 'video' ? videos : []),
  createElement: () => {
    const el = mkVideo({ paused: true });
    el.style = {};
    Object.defineProperty(el, 'textContent', { set(v) { toasts.push(v); }, configurable: true });
    return el;
  },
  body: { appendChild(el) { if (el && el.getVideoPlaybackQuality) videos.push(el); return el; } },
  addEventListener() {}
};
global.requestAnimationFrame = (fn) => fn();
global.setTimeout = () => 0;
global.setInterval = (fn, ms) => { if (ms === 2000) tick = fn; if (ms === 10000) watch = fn; return 0; };
global.clearInterval = () => {};
global.XMLHttpRequest = function () {};
global.XMLHttpRequest.prototype = { open() {}, send() {} };
Date.now = () => clock;

global.ApiClient = {
  accessToken: () => 'tok', serverAddress: () => 'http://jf.test',
  deviceId: () => 'dev-1', getCurrentUserId: () => 'user-1'
};

let sessionItem = 'item-9';
let sessionState = { PositionTicks: 12345, AudioStreamIndex: 2, SubtitleStreamIndex: 5 };

global.fetch = (url, opts) => {
  const ok = (d) => Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(d) });
  if (url.includes('/PlaybackInfo')) {
    lastPlaybackInfoBody = opts && opts.body;
    if (opts && opts.body && opts.body.includes('"AutoOpenLiveStream":false')) {
      probeRequests.push(JSON.parse(opts.body));
      return ok({ MediaSources: [{ Id: 'ms1', SupportsDirectPlay: true, SupportsDirectStream: true }] });
    }
    return ok({});
  }
  if (url.includes('/Sessions?deviceId=')) {
    return ok([{ Id: 'sess-1', NowPlayingItem: { Id: sessionItem }, PlayState: sessionState }]);
  }
  if (url.includes('/Playing?playCommand=PlayNow')) { restarts.push(url); return ok(null); }
  if (url.includes('/DisplayPreferences/')) {
    return Promise.resolve({ ok: true, status: 204, json: () => Promise.resolve(null) });
  }
  return ok(null);
};

// eval() is deliberate: a harness running OUR OWN local file in a mocked browser.
eval(fs.readFileSync(path.join(__dirname, '..', 'src', 'adaptive-quality.js'), 'utf8'));
if (!tick) { console.error('ERROR: tick not captured'); process.exit(1); }

const flush = () => new Promise(r => setImmediate(r));
async function advance(sec, o = {}) {
  for (let i = 0; i < sec / 2; i++) {
    clock += 2000; video.currentTime += 2; video._total += 48;
    video._dropped += o.dropPerStep || 0;
    if (o.ahead !== undefined) video._ahead = o.ahead;
    if (o.waiting) video.fire('waiting');
    tick();
    for (let k = 0; k < 5; k++) await flush();
  }
}
async function profile() {
  lastPlaybackInfoBody = null;
  await window.fetch('http://jf.test/Items/item-9/PlaybackInfo?userId=user-1', {
    method: 'POST',
    body: JSON.stringify({ UserId: 'user-1', DeviceProfile: { DirectPlayProfiles: [], CodecProfiles: [] } })
  });
  return JSON.parse(lastPlaybackInfoBody);
}
function cond(p, prop) {
  for (const cp of (p.DeviceProfile.CodecProfiles || [])) {
    for (const c of (cp.Conditions || [])) if (c.Property === prop) return c;
  }
  return null;
}
let failures = 0;
const check = (n, c, e) => { console.log((c ? '  PASS  ' : '  FAIL  ') + n + (e ? '  -> ' + e : '')); if (!c) failures++; };

(async () => {
  console.log('\n=== 1. healthy playback: change nothing ===');
  await advance(40, { dropPerStep: 0, ahead: 30 });
  check('no restart', restarts.length === 0);
  check('no constraint', (await profile()).DeviceProfile.CodecProfiles.length === 0);

  console.log('\n=== 2. device trouble: drop Dolby Vision first, keep 4K ===');
  await advance(24, { dropPerStep: 20, ahead: 30 });
  let p = await profile();
  check('Dolby Vision excluded', !!cond(p, 'VideoRangeType'));
  check('NO resolution constraint (4K kept)', !cond(p, 'Width'));
  check('restart preserves audio track', restarts.some(u => u.includes('audioStreamIndex=2')));
  check('restart preserves subtitle track', restarts.some(u => u.includes('subtitleStreamIndex=5')));

  console.log('\n=== 3. STABILITY: quality is never raised mid-title ===');
  restarts.length = 0; clock += 25000;
  await advance(400, { dropPerStep: 0, ahead: 30 });   // long clean stretch
  check('no restart during a long clean stretch', restarts.length === 0,
        restarts.length + ' restart(s)');
  check('still at the safe level', !!cond(await profile(), 'VideoRangeType'));

  console.log('\n=== 4. hidden tab must not be diagnosed as a slow device ===');
  restarts.length = 0; hidden = true; clock += 25000;
  await advance(40, { dropPerStep: 40, ahead: 30 });   // massive drops while hidden
  check('no action while the tab is hidden', restarts.length === 0);
  hidden = false;

  console.log('\n=== 5. seeking must not be diagnosed as a slow network ===');
  restarts.length = 0; clock += 25000;
  video.fire('seeking');
  await advance(4, { dropPerStep: 0, ahead: 0, waiting: true });
  check('no action right after a seek', restarts.length === 0);

  console.log('\n=== 6. fast-forward must not count ===');
  restarts.length = 0; clock += 25000; video.playbackRate = 2;
  await advance(30, { dropPerStep: 40, ahead: 30 });
  check('no action while fast-forwarding', restarts.length === 0);
  video.playbackRate = 1;

  console.log('\n=== 7. network trouble uses its own axis ===');
  restarts.length = 0; clock += 25000;
  const widthBefore = cond(await profile(), 'Width');
  await advance(30, { dropPerStep: 0, ahead: 1, waiting: true });
  p = await profile();
  check('bitrate applied from the ladder', [20000000,12000000,8000000,4000000].includes(p.MaxStreamingBitrate),
        String(p.MaxStreamingBitrate));
  check('quality axis untouched', !!cond(p, 'VideoRangeType') && !cond(p, 'Width') === !widthBefore);

  console.log('\n=== 8. paused probe raises the level WITHOUT restarting ===');
  restarts.length = 0; probeRequests = []; clock += 400000;
  const before = cond(await profile(), 'VideoBitDepth') ? 'constrained' : 'free';
  video.paused = true;
  tick(); for (let k = 0; k < 20; k++) await flush();
  check('a probe request was made', probeRequests.length >= 1, probeRequests.length + ' probe(s)');
  check('probe asked with AutoOpenLiveStream:false',
        probeRequests.every(r => r.AutoOpenLiveStream === false));
  check('probe did NOT restart playback', restarts.length === 0);
  video.paused = false;

  console.log('\n=== 9. new title releases the resolution constraint, keeps colour ===');
  clock += 25000;
  // force down to a resolution level first
  video.paused = false;
  for (let i = 0; i < 3; i++) { clock += 25000; await advance(24, { dropPerStep: 30, ahead: 30 }); }
  check('resolution constrained before title change', !!cond(await profile(), 'Width'));
  sessionItem = 'item-NEW';
  watch(); for (let k = 0; k < 8; k++) await flush();
  p = await profile();
  check('resolution constraint released on new title', !cond(p, 'Width'));
  check('colour constraint kept on new title', !!cond(p, 'VideoRangeType'));

  console.log('\n=== 10. robustness ===');
  const saved = global.ApiClient; global.ApiClient = undefined;
  let crashed = false;
  try { await advance(24, { dropPerStep: 20, ahead: 30 }); } catch (e) { crashed = true; }
  check('no crash without ApiClient', !crashed);
  global.ApiClient = saved;
  lastPlaybackInfoBody = null;
  await window.fetch('http://jf.test/Items/x/PlaybackInfo', { method: 'POST', body: 'not json' });
  check('malformed body passed through untouched', lastPlaybackInfoBody === 'not json');

  console.log('\n' + (failures === 0 ? 'ALL TESTS PASSED' : failures + ' TEST(S) FAILED'));
  process.exit(failures === 0 ? 0 : 1);
})();
