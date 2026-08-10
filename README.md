Adaptive playback quality for Jellyfin Web
==========================================

Jellyfin negotiates on codec SUPPORT, not decode CAPACITY. A browser that reports
"I support HEVC" may still be unable to sustain 4K 10-bit Dolby Vision inside a
<video> element. The server direct-streams it and the viewer gets a stutter-fest.

Measured on Jellyfin 10.11.11, Safari, Intel MacBook Pro, playing a 3240x2160
HEVC Main 10 / Dolby Vision Profile 8.1 / 26.4 Mbps source:

    Dropped frames    19576
    Corrupted frames  15273
    Playback method   Direct streaming

The file itself decodes without a single error, so nothing is broken; the device
profile simply has no field for "...but not this heavy".


WHAT IT DOES
------------
Samples getVideoPlaybackQuality() and the buffer every 2 seconds and separates two
failures that look alike but need opposite remedies:

    frames dropping, buffer healthy   ->  decoder cannot keep up
    buffer starved / waiting events   ->  connection cannot keep up

For a decoder problem it repairs the FAILING AXIS, least damage first, instead of
lowering bitrate and throwing away resolution for what may be a colour problem.

Verified against 10.11.11; "server produced" is ffprobe on real HLS segments:

    step  constraint added to CodecProfiles          server produced
    0     none                                       direct play (4K HEVC 10-bit DV)
    1     VideoRangeType EqualsAny SDR|HDR10|HLG     hevc 3240x2160 yuv420p10le smpte2084
    2     + VideoBitDepth LessThanEqual 8            h264 3240x2160 yuv420p bt709
    3     + Width LessThanEqual 2560                 h264 2560x1706
    4     + Width LessThanEqual 1920                 h264 1920x1280
    5     + Width LessThanEqual 1280                 h264 1280x852

Step 1 is the point: dropping only the Dolby Vision layer keeps FULL 4K *and*
HDR10, because DV Profile 8.1 carries an HDR10 base layer. A bitrate cap would
have gone straight to 2560x1706.

Network trouble uses a separate bitrate axis (unlimited -> 20 -> 12 -> 8 -> 4 Mbps)
because colour depth is not the problem when the connection is the bottleneck.


STABILITY
---------
Quality is never raised mid-title: renegotiating restarts the stream and can simply
stutter again. Recovery is validated while PAUSED (hidden muted element, transcode-
requiring levels skipped so no second encoder is started) and applied at the next
negotiation. On a new title, resolution constraints are released and colour
constraints kept.


FALSE POSITIVES IT IGNORES
--------------------------
All of these look identical to "slow device" in the frame counters:

    hidden / backgrounded tab   browsers throttle rendering and drop frames wholesale
    the seconds after a seek    the buffer is legitimately empty while it refills
    playbackRate != 1          fast-forward drops frames by design
    its own probe element      measuring it feeds back into the diagnosis

Adjustments carry audioStreamIndex and subtitleStreamIndex through the restart, so
the viewer does not lose their subtitle track.


HOW IT HOOKS IN
---------------
Rewrites the DeviceProfile on the outgoing /PlaybackInfo request (fetch + XHR) and
restarts at position via POST /Sessions/{id}/Playing. It does NOT call jellyfin-web
internals: playbackManager is not on window and is minified, so hooking it would
break on every release. Standard web APIs plus the public REST API only.

Fails safe: any error results in no constraint at all, i.e. current behaviour.
A malformed request body is passed through untouched.


FILES
-----
    adaptive-quality.js        the mechanism (self-contained, no dependencies)
    quality-panel.js           optional admin view: who is transcoding, why, from what to what
    test-adaptive-quality.js   22 tests, run with: node test-adaptive-quality.js
    test-quality-panel.js      10 tests, run with: node test-quality-panel.js
    LICENSE                    MIT-0 - use freely, no credit required

Install: paste adaptive-quality.js into any mechanism that injects a script into
jellyfin-web (e.g. the JavaScript Injector plugin) and enable it.
