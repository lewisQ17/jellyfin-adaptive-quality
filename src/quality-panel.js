/* Quality Panel — admin overview: who is watching, is it being transcoded, and why (in plain
   language). Adds a button on the dashboard route; never touches the page's React tree. */
(function () {
  'use strict';
  if (window.__qualityPanel) return;
  window.__qualityPanel = true;

  var REASON_TEXT = {
    ContainerNotSupported:        'player does not support this container',
    VideoCodecNotSupported:       'player does not support this video codec',
    AudioCodecNotSupported:       'player does not support this audio codec',
    ContainerBitrateExceedsLimit: 'file exceeds the configured bitrate limit',
    VideoBitrateNotSupported:     'video bitrate too high for the player',
    AudioBitrateNotSupported:     'audio bitrate too high for the player',
    VideoResolutionNotSupported:  'resolution too high for the player',
    VideoBitDepthNotSupported:    'player cannot handle 10-bit colour',
    VideoRangeTypeNotSupported:   'player cannot handle this HDR type (e.g. Dolby Vision)',
    VideoProfileNotSupported:     'video profile not supported',
    VideoLevelNotSupported:       'video level too high for the player',
    AudioChannelsNotSupported:    'channel layout not supported',
    SubtitleCodecNotSupported:    'subtitle format requires burn-in',
    DirectPlayError:              'direct play failed',
    AnamorphicVideoNotSupported:  'anamorphic video not supported',
    InterlacedVideoNotSupported:  'interlaced video not supported'
  };

  function api(path) {
    var a = window.ApiClient;
    if (!a || !a.accessToken) return Promise.reject(new Error('no ApiClient'));
    return fetch(a.serverAddress() + path, {
      headers: { 'Authorization': 'MediaBrowser Token="' + a.accessToken() + '"' }
    }).then(function (r) {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    });
  }

  function mb(n) { return n ? (n / 1e6).toFixed(1) + ' Mbps' : '—'; }
  function res(w, h) { return (w && h) ? (Number(w) + '×' + Number(h)) : '—'; }

  /* Everything coming from the server/clients must be escaped: device and user names are
     chosen by users and therefore untrusted. */
  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c];
    });
  }

  function reasonsText(list) {
    if (!list || !list.length) return '';
    return list.map(function (r) { return esc(REASON_TEXT[r] || r); }).join(', ');
  }

  /* Verdicts written by adaptive-quality. One account can be signed in on several
     devices, so each user's record holds one entry per device (key "d_<deviceId>"). */
  function loadVerdicts() {
    return api('/Users').then(function (users) {
      return Promise.all(users.map(function (u) {
        return api('/DisplayPreferences/adaptivequality?userId=' + encodeURIComponent(u.Id) +
                   '&client=adaptivequality')
          .then(function (d) {
            var c = (d && d.CustomPrefs) || {};
            var out = [];
            Object.keys(c).forEach(function (k) {
              if (k.indexOf('d_') !== 0) return;
              try {
                var e = JSON.parse(c[k]);
                if (!e || !e.ts) return;
                out.push({ userId: u.Id, name: u.Name, deviceId: k.slice(2),
                           verdict: e.verdict, reason: e.reason, level: e.level,
                           bitrate: e.bitrate, ts: e.ts });
              } catch (err) { /* skip unreadable entry */ }
            });
            return out;
          })
          .catch(function () { return []; });
      }));
    }).then(function (rows) {
      return rows.reduce(function (acc, r) { return acc.concat(r); }, []);
    });
  }

  function render(sessions, verdicts) {
    var byDevice = {}, nameOf = {};
    verdicts.forEach(function (v) { if (v.deviceId) byDevice[v.deviceId] = v; });
    sessions.forEach(function (s) { if (s.DeviceId) nameOf[s.DeviceId] = s.DeviceName || ''; });

    var playing = sessions.filter(function (s) { return s.NowPlayingItem; });
    var html = '<h2 style="margin:0 0 4px;font-size:19px">Playback quality</h2>' +
      '<div style="opacity:.65;font-size:12px;margin-bottom:14px">' +
      playing.length + ' active · refreshes every 5 s</div>';

    if (!playing.length) {
      html += '<div style="opacity:.6;padding:16px 0">Nobody is watching right now.</div>';
    } else {
      html += '<table style="width:100%;border-collapse:collapse;font-size:13px">' +
        '<thead><tr style="text-align:left;opacity:.6">' +
        '<th style="padding:6px 8px">User</th><th style="padding:6px 8px">Device</th>' +
        '<th style="padding:6px 8px">Method</th><th style="padding:6px 8px">From → to</th>' +
        '<th style="padding:6px 8px">Why</th></tr></thead><tbody>';

      playing.forEach(function (s) {
        var ni = s.NowPlayingItem || {}, ti = s.TranscodingInfo, ps = s.PlayState || {};
        var direct = !ti || (ps.PlayMethod && ps.PlayMethod.indexOf('Transcode') === -1);
        var vs = (ni.MediaStreams || []).filter(function (m) { return m.Type === 'Video'; })[0] || {};
        var from = res(vs.Width, vs.Height) + ' ' + esc((vs.Codec || '').toUpperCase()) +
                   (vs.VideoRangeType && vs.VideoRangeType !== 'SDR' ? ' ' + esc(vs.VideoRangeType) : '');
        var to = ti ? (res(ti.Width, ti.Height) + ' ' + esc((ti.VideoCodec || '').toUpperCase()) +
                       ' @ ' + mb(ti.Bitrate)) : '(unchanged)';
        var mine = byDevice[s.DeviceId];
        var why = direct ? '' : reasonsText(ti && ti.TranscodeReasons);
        if (mine && mine.reason) {
          why = (mine.verdict === 'netwerk' ? '📶 ' : '🖥️ ') + esc(mine.reason) +
                (why ? ' <span style="opacity:.55">(' + why + ')</span>' : '');
        } else if (!why && !direct) {
          why = 'unknown';
        }
        html += '<tr style="border-top:1px solid rgba(255,255,255,.09)">' +
          '<td style="padding:7px 8px">' + esc(s.UserName || '?') + '</td>' +
          '<td style="padding:7px 8px">' + esc(s.DeviceName || '?') +
            '<div style="opacity:.5;font-size:11px">' + esc(s.Client || '') + '</div></td>' +
          '<td style="padding:7px 8px">' + (direct
              ? '<span style="color:#5fd97a">direct</span>'
              : '<span style="color:#ffb648">transcoded</span>') + '</td>' +
          '<td style="padding:7px 8px">' + from +
            (direct ? '' : '<div style="opacity:.7">→ ' + to + '</div>') + '</td>' +
          '<td style="padding:7px 8px">' + (why || '—') + '</td></tr>';
      });
      html += '</tbody></table>';
    }

    if (verdicts.length) {
      html += '<h3 style="margin:20px 0 6px;font-size:14px;opacity:.8">Recent automatic adjustments</h3>' +
        '<div style="font-size:12px;opacity:.75">';
      verdicts.sort(function (a, b) { return (b.ts || '').localeCompare(a.ts || ''); })
        .slice(0, 8).forEach(function (v) {
          var icon = v.verdict === 'network' ? '📶' : (v.verdict === 'recovery' ? '⬆️' : '🖥️');
          // The same account can appear more than once here, one line per device.
          var dev = nameOf[v.deviceId] || ('device ' + String(v.deviceId).slice(0, 6));
          html += '<div style="padding:3px 0">' + icon + ' <b>' + esc(v.name) + '</b>' +
            ' <span style="opacity:.7">on ' + esc(dev) + '</span> — ' +
            esc(v.reason || '') +
            (v.level ? ' <span style="opacity:.8">[' + esc(v.level) + ']</span>' : '') +
            ' <span style="opacity:.55">(' +
            esc(new Date(v.ts).toLocaleString(undefined)) + ')</span></div>';
        });
      html += '</div>';
    }
    return html;
  }

  var overlay, timer;

  function refresh() {
    Promise.all([api('/Sessions'), loadVerdicts().catch(function () { return []; })])
      .then(function (r) {
        var body = overlay && overlay.querySelector('.qg-body');
        if (body) body.innerHTML = render(r[0] || [], r[1] || []);
      })
      .catch(function (e) {
        var body = overlay && overlay.querySelector('.qg-body');
        if (body) body.innerHTML = '<div style="opacity:.7">Could not load data: ' + esc(e.message) + '</div>';
      });
  }

  function open() {
    if (overlay) { overlay.style.display = 'block'; refresh(); timer = setInterval(refresh, 5000); return; }
    overlay = document.createElement('div');
    overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.72);z-index:2147483000;' +
      'display:block;overflow:auto;padding:5vh 4vw';
    overlay.innerHTML =
      '<div style="max-width:1000px;margin:0 auto;background:#1b1b1f;color:#eee;border-radius:12px;' +
      'padding:22px 24px;box-shadow:0 12px 60px rgba(0,0,0,.6)">' +
      '<button class="qg-close" style="float:right;background:none;border:0;color:#aaa;font-size:24px;' +
      'cursor:pointer;line-height:1">×</button><div class="qg-body">loading…</div></div>';
    document.body.appendChild(overlay);
    overlay.addEventListener('click', function (e) {
      if (e.target === overlay || e.target.classList.contains('qg-close')) close();
    });
    refresh();
    timer = setInterval(refresh, 5000);
  }

  function close() {
    if (timer) { clearInterval(timer); timer = null; }
    if (overlay) overlay.style.display = 'none';
  }

  function ensureButton(isAdmin) {
    if (!isAdmin) return;
    var onDash = (location.hash || '').toLowerCase().indexOf('dashboard') !== -1;
    var btn = document.getElementById('qg-btn');
    if (!onDash) { if (btn) btn.remove(); close(); return; }
    if (btn) return;
    btn = document.createElement('button');
    btn.id = 'qg-btn';
    btn.textContent = '📊 Playback quality';
    btn.style.cssText = 'position:fixed;right:18px;bottom:18px;z-index:2147482000;' +
      'background:#6f5cff;color:#fff;border:0;border-radius:22px;padding:11px 18px;' +
      'font:14px system-ui,sans-serif;cursor:pointer;box-shadow:0 4px 18px rgba(0,0,0,.45)';
    btn.onclick = open;
    document.body.appendChild(btn);
  }

  function boot() {
    api('/Users/Me').then(function (me) {
      var isAdmin = !!(me && me.Policy && me.Policy.IsAdministrator);
      if (!isAdmin) return;
      ensureButton(true);
      window.addEventListener('hashchange', function () { ensureButton(true); });
      setInterval(function () { ensureButton(true); }, 3000);
    }).catch(function () { /* not signed in or not an admin: do nothing */ });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { setTimeout(boot, 1500); });
  } else {
    setTimeout(boot, 1500);
  }
})();
