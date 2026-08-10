# Adaptive Quality for Jellyfin Web

Jellyfin negotiates playback on **codec support**, not on **decode capability**. A browser
that reports "I support HEVC" may still be unable to sustain 4K 10-bit Dolby Vision inside a
`<video>` element. The server does exactly what it was told, direct-streams the file, and the
viewer gets a stutter-fest.

This is a drop-in userscript that measures what actually happens during playback and repairs
**the axis that is failing** — instead of bluntly lowering bitrate, which sacrifices resolution
for what may well be a colour problem.

> Reference implementation for
> [jellyfin-web discussion](https://github.com/jellyfin/jellyfin-web) — see *Why this might
> belong upstream* below.

## The problem, measured

Playing a 3240×2160 HEVC Main 10 remux, Dolby Vision Profile 8.1, 26.4 Mbps, in Safari on
an Intel MacBook Pro, via Jellyfin 10.11.11:

```
Dropped frames    19576
Corrupted frames  15273
Playback method   Direct streaming
```

The file itself is fine: `ffmpeg -v error -ss 00:39:30 -t 45 -i <file> -f null -` reports
**zero errors**. Safari genuinely does have an HEVC decoder — it just cannot sustain *this*.
There is no field in the device profile for "…but not this heavy", so nothing is wrong from
Jellyfin's point of view.

## What this does instead

Every 2 seconds it samples `getVideoPlaybackQuality()` plus how far the buffer runs ahead,
and distinguishes two failures that look similar but need opposite remedies:

| Observation | Diagnosis | Remedy |
|---|---|---|
| frames dropping, buffer **healthy** | decoder cannot keep up | step down the quality axis |
| buffer **starved** / `waiting` events | connection cannot keep up | step down the bitrate axis |

### Quality axis — least damage first

Verified against Jellyfin 10.11.11 with a source of those characteristics. The "server produced" column is
`ffprobe` output of an actual HLS segment fetched from the server:

| Step | Constraint added to `DeviceProfile.CodecProfiles` | Server produced |
|---|---|---|
| 0 | none | direct play (4K HEVC 10-bit DV) |
| 1 | `VideoRangeType EqualsAny SDR\|HDR10\|HLG` | `hevc 3240×2160 yuv420p10le smpte2084` |
| 2 | + `VideoBitDepth LessThanEqual 8` | `h264 3240×2160 yuv420p bt709` |
| 3 | + `Width LessThanEqual 2560` | `h264 2560×1706` |
| 4 | + `Width LessThanEqual 1920` | `h264 1920×1280` |
| 5 | + `Width LessThanEqual 1280` | `h264 1280×852` |

**Step 1 is the point of the whole thing:** dropping only the Dolby Vision layer keeps full
4K resolution *and* HDR10, because DV Profile 8.1 carries an HDR10 base layer. Lowering the
bitrate instead would have thrown away resolution to fix a colour problem.

Every step above decodes without a single error (`ffmpeg -f null -`), which also rules out
the main risk of DV transcoding: washed-out or green output from failed tone-mapping.

### Network axis

`MaxStreamingBitrate`: unlimited → 20 → 12 → 8 → 4 Mbps. Deliberately independent of the
quality axis — colour depth is not the problem when the connection is the bottleneck.

### Recovery — never mid-title

Raising quality means renegotiating, which restarts the stream and may simply stutter again.
So once playback is good it stays good for the rest of the title. Recovery happens two ways
instead, neither of which interrupts anything:

- **while paused** — the next-better level is loaded into a hidden, muted element and judged
  on its own dropped-frame count. `AutoOpenLiveStream` stays `false` and a level that would
  need transcoding is skipped, so the probe never spins up a second encoder next to the
  stream the viewer is watching. A pass raises the level for the next negotiation.
- **on the next title** — resolution constraints are released, colour constraints are kept.
  A colour constraint reflects the device and costs nothing on content that is not Dolby
  Vision or 10-bit; a resolution cap would needlessly downscale the next film.

### False positives it deliberately ignores

These all look exactly like "this device is too slow" and none of them are:

| Situation | Why it is ignored |
|---|---|
| hidden / backgrounded tab | browsers throttle rendering and drop frames wholesale |
| the seconds after a seek | the buffer is legitimately empty while it refills |
| `playbackRate !== 1` | fast-forward drops frames by design |
| its own probe element | measuring the probe would feed back into the diagnosis |

Adjustments also preserve the viewer's audio and subtitle selection across the restart, and
the on-screen notice only appears once the change has actually been applied.

## How it hooks in

It rewrites the `DeviceProfile` in the outgoing `/PlaybackInfo` request (both `fetch` and
`XMLHttpRequest`), then restarts playback at the same position via the public remote-control
API:

```
POST /Sessions/{id}/Playing?playCommand=PlayNow&itemIds={id}&startPositionTicks={ticks}
```

It deliberately does **not** call into jellyfin-web internals. `playbackManager` is not
exposed on `window` and is minified, so hooking it would break on every release. Everything
here is standard web APIs plus the public REST API.

**Fails safe.** Any error at any point results in *no* constraint being applied, i.e. exactly
today's behaviour. A malformed request body is passed through untouched.

## Install

Any mechanism that injects a script into jellyfin-web works. Tested with the
[JavaScript Injector](https://github.com/n8pjl/jellyfin-javascript-injector) plugin: paste
`src/adaptive-quality.js` as a custom script and enable it.

`src/quality-panel.js` is optional — it adds an admin-only button on the dashboard route
showing who is transcoding, from what to what, and why, with Jellyfin's `TranscodeReasons`
translated into plain language.

### One account, several devices

Accounts are not tied to devices — the same login is routinely used on a laptop and a phone
at once, and each has its own capabilities. Two things follow:

- **Adaptation is already per device.** The current level lives in the page, not on the
  server, so one device stepping down never constrains another.
- **Reporting is per device too.** Each user's record holds one entry per device
  (`d_<deviceId>`), so a phone and a laptop do not overwrite each other's diagnosis and the
  dashboard can show the right reason next to the right session. The record is pruned to the
  ten most recent devices.

## Tests

```sh
node test/test-adaptive-quality.js
node test/test-quality-panel.js
```

Covers: no intervention on healthy playback · Dolby Vision dropped first with 4K/HDR10 kept ·
10-bit dropped only on the next failure · resolution reduced only after that · quality never
raised mid-title · hidden tabs, seeks and fast-forward ignored · network trouble never touching
the quality axis · audio and subtitle selection preserved across a restart · no crash without
`ApiClient` or `<video>` · malformed bodies passed through untouched · one account on two
devices keeping both diagnoses · hostile device names escaped.

The captured profiles are also replayed against a live server to confirm the ladder produces
the resolutions in the table above.

## Why this might belong upstream

The mechanism is a workaround for something only the client can know: **whether it is actually
keeping up**. `getVideoPlaybackQuality()` is already available in every relevant browser, and
jellyfin-web already reads it — the playback info dialog displays the dropped-frame count. It
simply does not act on it.

A native implementation would be considerably cleaner than a `fetch` interceptor: the playback
manager could constrain the profile directly and renegotiate without a visible restart. The
axis ordering and the measured ladder above are the parts worth taking; the plumbing here is
just what is possible from outside.

## License

MIT
