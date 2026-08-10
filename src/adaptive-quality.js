/*
 * Adaptive Quality for Jellyfin Web
 * ---------------------------------
 * Jellyfin negotiates on *codec support*, not on *decode capability*. A browser that
 * reports "I support HEVC" may still be unable to sustain 4K 10-bit Dolby Vision in a
 * <video> element. The server does exactly what it was told, and the viewer gets a
 * stutter-fest with tens of thousands of dropped frames.
 *
 * This script measures what actually happens during playback and repairs the axis that
 * is failing — instead of bluntly lowering bitrate, which costs resolution for what may
 * well be a colour problem.
 *
 * Escalation for a decode problem (least damage first):
 *   1. drop Dolby Vision  -> 4K and HDR10 are preserved
 *   2. drop 10-bit / HDR  -> 4K is still preserved
 *   3. reduce resolution  -> 2K, 1080p, 720p
 * Network problems get a separate axis (bitrate), because colour depth is not the issue
 * when the connection is the bottleneck.
 *
 * STABILITY RULE: once playback is good it stays good. Quality is never raised in the
 * middle of a title — raising it means renegotiating, which restarts the stream and may
 * simply stutter again. Instead the better level is validated *while paused* (free
 * capacity, disturbs nothing) and applied at the next negotiation.
 *
 * Implementation note: it rewrites the DeviceProfile in the outgoing /PlaybackInfo
 * request (fetch + XHR) rather than calling into jellyfin-web internals. playbackManager
 * is not exposed on window and is minified, so hooking it would break on every update.
 * Everything here uses standard web APIs plus the public REST API.
 *
 * Fails safe: on any error it applies no restriction at all, i.e. current behaviour.
 *
 * License: MIT-0 (MIT No Attribution) — use freely, no credit required.
 */
(function () {
  'use strict';
  if (window.__adaptiveQuality) return;
  window.__adaptiveQuality = true;

  var CFG = {
    sampleMs:          2000,   // how often we sample playback quality
    graceMs:           10000,  // ignore start-up and restart spikes
    cooldownMs:        20000,  // after an intervention the stream restarts; stay quiet
    seekQuietMs:       6000,   // a seek starves the buffer; that is not a network fault
    windowSize:        3,      // this many consecutive bad samples before acting
    dropPct:           5,      // >5% dropped frames in a sample = decode trouble
    corruptDelta:      30,     // or this many corrupted frames across the window
    bufferLowSec:      2,      // less buffered ahead than this = network trouble
    bufferHealthySec:  5,      // more than this = network is not the culprit
    probeMs:           8000,   // how long a paused-probe plays before judging
    probeMinGapMs:     300000, // never probe more often than this
    netLadder: [0, 20000000, 12000000, 8000000, 4000000],
    debug: true
  };

  /*
   * Quality ladder. Each step gives up as little as possible.
   * Verified against Jellyfin 10.11.11 with a 3240x2160 HEVC Main10 Dolby Vision P8.1
   * source of those characteristics; the comment shows what the server actually produced.
   */
  var LEVELS = [
    { label: 'original',      c: {} },
    { label: '4K with HDR10', c: { noDolbyVision: true } },                                // hevc 3240x2160 10-bit PQ
    { label: '4K SDR',        c: { noDolbyVision: true, no10bit: true } },                  // h264 3240x2160 bt709
    { label: '2K',            c: { noDolbyVision: true, no10bit: true, maxWidth: 2560 } },  // h264 2560x1706
    { label: '1080p',         c: { noDolbyVision: true, no10bit: true, maxWidth: 1920 } },  // h264 1920x1280
    { label: '720p',          c: { noDolbyVision: true, no10bit: true, maxWidth: 1280 } }   // h264 1280x852
  ];
  var COLOUR_ONLY_MAX = 2;   // above this we constrain resolution, which is content-specific

  var MSG = {
    device:  function (lvl) { return 'Quality adjusted to ' + lvl + ' — this device could not play the original smoothly.'; },
    network: function ()    { return 'Quality temporarily reduced — your connection is too slow.'; }
  };

  var qLevel = 0;                 // device/quality axis
  var nLevel = 0;                 // network axis
  var lastClientProfile = null;   // the real DeviceProfile jellyfin-web last sent
  var lastProbeAt = 0;
  var currentItemId = null;

  function log() {
    if (!CFG.debug) return;
    try { console.log.apply(console, ['[AdaptiveQuality]'].concat([].slice.call(arguments))); } catch (e) {}
  }

  /* ---------- 1. rewrite the DeviceProfile on every PlaybackInfo request ---------- */

  function conditionsFor(level) {
    var c = LEVELS[level].c, out = [];
    if (c.noDolbyVision) {
      out.push({ Condition: 'EqualsAny', Property: 'VideoRangeType',
                 Value: 'SDR|HDR10|HLG', IsRequired: true });
    }
    if (c.no10bit) {
      out.push({ Condition: 'LessThanEqual', Property: 'VideoBitDepth', Value: '8', IsRequired: true });
    }
    if (c.maxWidth) {
      out.push({ Condition: 'LessThanEqual', Property: 'Width', Value: String(c.maxWidth), IsRequired: true });
    }
    return out;
  }

  function constrain(profile, level) {
    var conds = conditionsFor(level);
    if (conds.length) {
      profile.CodecProfiles = (profile.CodecProfiles || []).concat([{
        Type: 'Video', Codec: 'h264,hevc,vp9,av1', Conditions: conds
      }]);
    }
    return profile;
  }

  function patchBody(text, url) {
    try {
      // The item id is in the request path. Taking it here means a probe can run the
      // moment playback is negotiated, instead of waiting for the session poll.
      var m = typeof url === 'string' && url.match(/\/Items\/([^/]+)\/PlaybackInfo/);
      if (m && m[1] && m[1] !== currentItemId) currentItemId = m[1];
      var d = JSON.parse(text);
      var p = d && d.DeviceProfile;
      if (!p) return text;
      // Remember the client's genuine profile so a probe can reuse it verbatim.
      try { lastClientProfile = JSON.parse(JSON.stringify(p)); } catch (e) {}
      if (qLevel === 0 && nLevel === 0) return text;
      constrain(p, qLevel);
      if (nLevel > 0) {
        d.MaxStreamingBitrate = CFG.netLadder[nLevel];
        p.MaxStreamingBitrate = CFG.netLadder[nLevel];
      }
      log('profile constrained ->', LEVELS[qLevel].label,
          nLevel > 0 ? '+ ' + (CFG.netLadder[nLevel] / 1e6) + ' Mbps' : '');
      return JSON.stringify(d);
    } catch (e) {
      log('profile left untouched:', e && e.message);
      return text;                       // when in doubt, change nothing
    }
  }

  function isPlaybackInfo(url) {
    return typeof url === 'string' && url.indexOf('/PlaybackInfo') !== -1;
  }

  (function patchFetch() {
    var orig = window.fetch;
    if (!orig) return;
    window.fetch = function (input, init) {
      try {
        var url = (typeof input === 'string') ? input : (input && input.url) || '';
        if (isPlaybackInfo(url) && init && typeof init.body === 'string') {
          init.body = patchBody(init.body, url);
        }
      } catch (e) {}
      return orig.apply(this, arguments);
    };
  })();

  (function patchXhr() {
    var XHR = window.XMLHttpRequest;
    if (!XHR || !XHR.prototype) return;
    var open = XHR.prototype.open, send = XHR.prototype.send;
    XHR.prototype.open = function (m, u) { this.__aqUrl = u; return open.apply(this, arguments); };
    XHR.prototype.send = function (b) {
      try {
        if (isPlaybackInfo(this.__aqUrl) && typeof b === 'string') arguments[0] = patchBody(b, this.__aqUrl);
      } catch (e) {}
      return send.apply(this, arguments);
    };
  })();

  /* ---------- 2. Jellyfin REST helpers ---------- */

  function api() {
    var a = window.ApiClient;
    if (!a || !a.accessToken || !a.serverAddress) return null;
    var t = a.accessToken(), b = a.serverAddress();
    if (!t || !b) return null;
    return { base: b, token: t, deviceId: (a.deviceId && a.deviceId()) || null,
             userId: (a.getCurrentUserId && a.getCurrentUserId()) || null };
  }

  function apiFetch(path, opts) {
    var a = api();
    if (!a) return Promise.reject(new Error('no ApiClient'));
    opts = opts || {};
    opts.headers = opts.headers || {};
    opts.headers['Authorization'] = 'MediaBrowser Token="' + a.token + '"';
    if (opts.body) opts.headers['Content-Type'] = 'application/json';
    return fetch(a.base + path, opts).then(function (r) {
      if (!r.ok) throw new Error('HTTP ' + r.status + ' for ' + path);
      return r.status === 204 ? null : r.json().catch(function () { return null; });
    });
  }

  function ownSession() {
    var a = api();
    if (!a || !a.deviceId) return Promise.reject(new Error('no deviceId'));
    return apiFetch('/Sessions?deviceId=' + encodeURIComponent(a.deviceId)).then(function (l) {
      if (!l || !l.length) throw new Error('own session not found');
      // Prefer the session that is actually playing: two tabs can share a deviceId.
      for (var i = 0; i < l.length; i++) if (l[i].NowPlayingItem) return l[i];
      return l[0];
    });
  }

  /* Restart at the same position, preserving the viewer's audio and subtitle choice. */
  function restartPlayback() {
    return ownSession().then(function (s) {
      var ni = s.NowPlayingItem, ps = s.PlayState || {};
      if (!ni) throw new Error('nothing playing');
      var q = '/Sessions/' + s.Id + '/Playing?playCommand=PlayNow' +
              '&itemIds=' + encodeURIComponent(ni.Id) +
              '&startPositionTicks=' + (ps.PositionTicks || 0);
      // Losing the chosen subtitle or audio track on every adjustment would be worse
      // than the stutter we are fixing.
      if (ps.AudioStreamIndex != null) q += '&audioStreamIndex=' + ps.AudioStreamIndex;
      if (ps.SubtitleStreamIndex != null) q += '&subtitleStreamIndex=' + ps.SubtitleStreamIndex;
      return apiFetch(q, { method: 'POST' });
    });
  }

  /* One account can be signed in on several devices at once, so the record holds one
     entry per device. A single row would mean a phone and a laptop overwriting each
     other's diagnosis, and the dashboard showing the wrong reason for a session. */
  function reportVerdict(verdict, reason) {
    var a = api();
    if (!a || !a.userId || !a.deviceId) return;
    var path = '/DisplayPreferences/adaptivequality?userId=' + encodeURIComponent(a.userId) +
               '&client=adaptivequality';
    var entry = JSON.stringify({
      verdict: verdict, reason: reason, level: LEVELS[qLevel].label,
      bitrate: String(CFG.netLadder[nLevel] || 0), ts: new Date().toISOString()
    });
    apiFetch(path).catch(function () { return null; }).then(function (cur) {
      var prefs = (cur && cur.CustomPrefs) || {};
      prefs['d_' + a.deviceId] = entry;
      // Keep the record bounded: drop the oldest entries beyond a sensible number.
      var keys = Object.keys(prefs).filter(function (k) { return k.indexOf('d_') === 0; });
      if (keys.length > 10) {
        keys.map(function (k) {
          var ts = '';
          try { ts = JSON.parse(prefs[k]).ts || ''; } catch (e) {}
          return { k: k, ts: ts };
        }).sort(function (x, y) { return x.ts < y.ts ? -1 : 1; })
          .slice(0, keys.length - 10)
          .forEach(function (o) { delete prefs[o.k]; });
      }
      return apiFetch(path, {
        method: 'POST',
        body: JSON.stringify({
          Id: 'adaptivequality', Client: 'adaptivequality', ViewType: null, SortBy: null,
          IndexBy: null, RememberIndexing: false, RememberSorting: false,
          SortOrder: 'Ascending', PrimaryImageHeight: 0, PrimaryImageWidth: 0,
          CustomPrefs: prefs
        })
      });
    }).catch(function (e) { log('storing verdict failed (harmless):', e.message); });
  }

  /* ---------- 3. viewer notification ---------- */

  function toast(msg) {
    try {
      var d = document.createElement('div');
      d.textContent = msg;
      d.style.cssText = 'position:fixed;left:50%;bottom:12%;transform:translateX(-50%);' +
        'background:rgba(20,20,20,.94);color:#fff;padding:12px 20px;border-radius:8px;' +
        'font:14px/1.4 system-ui,sans-serif;z-index:2147483647;max-width:80vw;text-align:center;' +
        'box-shadow:0 4px 24px rgba(0,0,0,.5);pointer-events:none;opacity:0;transition:opacity .3s';
      document.body.appendChild(d);
      requestAnimationFrame(function () { d.style.opacity = '1'; });
      setTimeout(function () {
        d.style.opacity = '0';
        setTimeout(function () { d.remove(); }, 400);
      }, 6000);
    } catch (e) {}
  }

  /* ---------- 4. measurement ---------- */

  function activeVideo() {
    var v = document.querySelectorAll('video');
    for (var i = 0; i < v.length; i++) {
      if (v[i].__aqProbe) continue;                       // never measure our own probe
      if (!v[i].paused && v[i].readyState >= 2 && v[i].videoWidth > 0) return v[i];
    }
    return null;
  }

  function pausedVideo() {
    var v = document.querySelectorAll('video');
    for (var i = 0; i < v.length; i++) {
      if (v[i].__aqProbe) continue;
      if (v[i].paused && v[i].readyState >= 2 && v[i].videoWidth > 0) return v[i];
    }
    return null;
  }

  function quality(v) {
    try {
      if (v.getVideoPlaybackQuality) {
        var q = v.getVideoPlaybackQuality();
        return { dropped: q.droppedVideoFrames || 0, total: q.totalVideoFrames || 0,
                 corrupted: q.corruptedVideoFrames || 0 };
      }
      if (typeof v.webkitDroppedFrameCount === 'number') {
        return { dropped: v.webkitDroppedFrameCount, total: v.webkitDecodedFrameCount || 0,
                 corrupted: 0 };
      }
    } catch (e) {}
    return null;
  }

  function bufferAhead(v) {
    try {
      var b = v.buffered;
      for (var i = 0; i < b.length; i++) {
        if (v.currentTime >= b.start(i) - 0.5 && v.currentTime <= b.end(i)) {
          return b.end(i) - v.currentTime;
        }
      }
    } catch (e) {}
    return 0;
  }

  /* ---------- 5. background probe (only while paused: costs the viewer nothing) ---------- */

  function probeUrl(level) {
    if (!lastClientProfile) return Promise.reject(new Error('no client profile captured yet'));
    var a = api();
    if (!a || !a.userId || !currentItemId) return Promise.reject(new Error('nothing to probe'));
    var prof = constrain(JSON.parse(JSON.stringify(lastClientProfile)), level);
    return apiFetch('/Items/' + currentItemId + '/PlaybackInfo?userId=' + encodeURIComponent(a.userId), {
      method: 'POST',
      // AutoOpenLiveStream stays false on purpose: opening a second encoder session for
      // the same item could disturb the stream the viewer is actually watching.
      body: JSON.stringify({ UserId: a.userId, DeviceProfile: prof, AutoOpenLiveStream: false })
    }).then(function (d) {
      var ms = d && d.MediaSources && d.MediaSources[0];
      if (!ms) throw new Error('no media source');
      if (!ms.SupportsDirectPlay && !ms.SupportsDirectStream) {
        // Would need a transcode; skip rather than spin up a second encoder.
        throw new Error('target level needs transcoding - not probing');
      }
      return a.base + '/Videos/' + currentItemId + '/stream?static=true&mediaSourceId=' +
             encodeURIComponent(ms.Id) + '&api_key=' + encodeURIComponent(a.token);
    });
  }

  function runProbe(target, atSeconds) {
    return probeUrl(target).then(function (url) {
      return new Promise(function (resolve) {
        var v = document.createElement('video');
        v.__aqProbe = true;
        v.muted = true; v.playsInline = true; v.preload = 'auto';
        v.style.cssText = 'position:fixed;width:2px;height:2px;opacity:0.01;' +
                          'pointer-events:none;left:-10px;top:-10px';
        var done = false;
        function finish(ok, why) {
          if (done) return;
          done = true;
          try { v.pause(); v.removeAttribute('src'); v.load(); v.remove(); } catch (e) {}
          resolve({ ok: ok, why: why });
        }
        v.addEventListener('error', function () { finish(false, 'load error'); });
        v.addEventListener('playing', function () {
          setTimeout(function () {
            var q = quality(v);
            if (!q || !q.total) return finish(false, 'no frames decoded');
            var pct = (q.dropped / q.total) * 100;
            finish(pct <= CFG.dropPct, 'dropped ' + pct.toFixed(1) + '%');
          }, CFG.probeMs);
        });
        document.body.appendChild(v);
        v.src = url + '#t=' + Math.max(0, Math.floor(atSeconds || 0));
        var p = v.play();
        if (p && p.catch) p.catch(function () { finish(false, 'autoplay blocked'); });
        setTimeout(function () { finish(false, 'timeout'); }, CFG.probeMs + 15000);
      });
    }).catch(function (e) { return { ok: false, why: e.message }; });
  }

  function maybeProbe(v) {
    if (qLevel === 0) return;
    if (Date.now() - lastProbeAt < CFG.probeMinGapMs) return;
    lastProbeAt = Date.now();
    var target = qLevel - 1;
    log('probing whether level "' + LEVELS[target].label + '" would play cleanly...');
    runProbe(target, v ? v.currentTime : 0).then(function (r) {
      if (r.ok) {
        qLevel = target;
        log('probe passed (' + r.why + ') - level raised to "' + LEVELS[qLevel].label +
            '"; applies at the next negotiation, playback is not interrupted');
        reportVerdict('recovery', 'Background check passed - quality goes up on the next title.');
      } else {
        log('probe did not pass (' + r.why + ') - staying at "' + LEVELS[qLevel].label + '"');
      }
    });
  }

  /* ---------- 6. decision logic ---------- */

  var S = null;
  function resetState(v) {
    S = { video: v, startedAt: Date.now(), last: quality(v), samples: [],
          lastAction: 0, waitEvents: 0, lastSeekAt: 0 };
    try {
      v.addEventListener('waiting', function () { if (S) S.waitEvents++; });
      v.addEventListener('stalled', function () { if (S) S.waitEvents++; });
      v.addEventListener('seeking', function () {
        if (S) { S.lastSeekAt = Date.now(); S.samples = []; }
      });
    } catch (e) {}
    log('tracking playback; current level:', LEVELS[qLevel].label);
  }

  function applyChange(message, verdict) {
    S.lastAction = Date.now();
    S.samples = [];
    reportVerdict(verdict, message);
    restartPlayback()
      .then(function () {
        toast(message);                  // only claim it once it actually happened
        log('playback restarted at level:', LEVELS[qLevel].label);
      })
      .catch(function (e) {
        // Restart failed: the constraint simply applies from the next playback start.
        // Never surface an error, and never claim something that did not happen.
        log('restart failed, constraint applies from next start:', e.message);
      });
  }

  function worseQuality() {
    if (qLevel >= LEVELS.length - 1) { log('already at lowest quality level'); return; }
    qLevel++;
    log('quality axis down ->', LEVELS[qLevel].label);
    applyChange(MSG.device(LEVELS[qLevel].label), 'device');
  }

  function worseNetwork() {
    if (nLevel >= CFG.netLadder.length - 1) { log('already at lowest bitrate'); return; }
    nLevel++;
    log('network axis down ->', CFG.netLadder[nLevel] / 1e6, 'Mbps');
    applyChange(MSG.network(), 'network');
  }

  /* A new title is a fresh situation. Colour constraints reflect the device and cost
     nothing on content that is not Dolby Vision or 10-bit, so they carry over. Resolution
     constraints are content- and network-specific, so they are released. */
  function onItemChanged(id) {
    currentItemId = id;
    nLevel = 0;
    if (qLevel > COLOUR_ONLY_MAX) {
      qLevel = COLOUR_ONLY_MAX;
      log('new title - resolution constraint released, keeping', LEVELS[qLevel].label);
    } else {
      log('new title - keeping', LEVELS[qLevel].label);
    }
  }

  function tick() {
    try {
      // A hidden or backgrounded tab drops frames wholesale because the browser throttles
      // rendering. Measuring then would diagnose a perfectly capable device as too slow.
      if (document.hidden) { if (S) S.samples = []; return; }

      var v = activeVideo();
      if (!v) {
        var pv = pausedVideo();
        if (pv) maybeProbe(pv);          // paused: free capacity, probing disturbs nothing
        return;
      }
      if (!S || S.video !== v) { resetState(v); return; }
      if (Date.now() - S.startedAt < CFG.graceMs) return;
      if (Date.now() - S.lastAction < CFG.cooldownMs) return;
      if (Date.now() - S.lastSeekAt < CFG.seekQuietMs) return;   // a seek is not a fault
      if (v.playbackRate && v.playbackRate !== 1) return;        // fast-forward drops frames by design

      var q = quality(v);
      if (!q || !S.last) { S.last = q; return; }
      var dDrop = Math.max(0, q.dropped - S.last.dropped);
      var dTotal = Math.max(0, q.total - S.last.total);
      var dCorrupt = Math.max(0, q.corrupted - S.last.corrupted);
      S.last = q;
      if (dTotal === 0) return;

      var pct = (dDrop / dTotal) * 100;
      var ahead = bufferAhead(v);
      var waits = S.waitEvents; S.waitEvents = 0;

      S.samples.push({ pct: pct, corrupt: dCorrupt, ahead: ahead, waits: waits });
      if (S.samples.length > CFG.windowSize) S.samples.shift();
      if (S.samples.length < CFG.windowSize) return;

      var badNet = 0, badDecode = 0, corruptSum = 0;
      S.samples.forEach(function (s) {
        corruptSum += s.corrupt;
        // Starved buffer means the network cannot keep up. Dropped frames with a healthy
        // buffer means the decoder cannot keep up. These need opposite remedies.
        if (s.ahead < CFG.bufferLowSec || s.waits > 0) badNet++;
        else if (s.pct > CFG.dropPct && s.ahead >= CFG.bufferHealthySec) badDecode++;
      });

      if (badNet >= CFG.windowSize) worseNetwork();
      else if (badDecode >= CFG.windowSize || corruptSum > CFG.corruptDelta) worseQuality();
      // Deliberately no upgrade here: raising quality mid-title restarts the stream and
      // may stutter all over again. Recovery is handled by the paused-probe above.
    } catch (e) {
      log('tick error (ignored):', e && e.message);
    }
  }

  /* Watch which title is playing so a new one starts from a sensible level. */
  function watchItem() {
    ownSession().then(function (s) {
      var id = s.NowPlayingItem && s.NowPlayingItem.Id;
      if (id && id !== currentItemId) onItemChanged(id);
    }).catch(function () {});
  }

  setInterval(tick, CFG.sampleMs);
  setInterval(watchItem, 10000);
  log('active - axes: Dolby Vision -> 10-bit -> resolution; network separate; ' +
      'never raised mid-title');
})();
