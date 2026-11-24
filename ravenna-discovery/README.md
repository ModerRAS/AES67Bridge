# ravenna-discovery (prototype, PAUSED)

RAVENNA discovery layer for AES67Bridge: DNS-SD/mDNS advertisement with the
RAVENNA service subtypes, plus a minimal RTSP 1.0 server that serves SDP for
named sessions.

**STATUS: research + prototype only, parked on 2026-11-24 at the user's request.**
The protocol research was finished and this crate compiles and passes its unit
tests, but the end-to-end validation (python-zeroconf browse, ffprobe DESCRIBE,
packet capture) was never run. Nothing here has touched the Z9X or a real
RAVENNA controller. Read:

    C:/WorkSpace/Claw/artifacts/aes67/ravenna-mdns-rtsp-2026-11-24.md

for the fact base, the known gaps and the resume plan.

## Layout

    src/sdp.rs         SDP builder (AES67 + RAVENNA extensions), SdpProvider trait
    src/mdns.rs        DNS-SD publisher/browser, RAVENNA subtype constants
    src/rtsp_proto.rs  RTSP 1.0 request parser / response serializer (+ unit tests)
    src/rtsp_server.rs tokio RTSP server: OPTIONS/DESCRIBE/SETUP/PLAY/TEARDOWN/
                       GET_PARAMETER, plus server-initiated ANNOUNCE
    src/rtp.rs         RFC 3550 packet builder + silent RTP thread (stand-in sender)
    src/config.rs      node config, local IP detection, PTP gmid parsing
    src/main.rs        CLI: sdp | advertise | serve | browse
    android/           NsdManager advertizer snippet (device-side path, see its README)

## Protocol facts this code encodes

* RAVENNA uses DNS-SD, and a node MUST publish its RTSP service:
  instance = vendor node id / user defined node name, type _rtsp._tcp,
  subtype _ravenna._sub._rtsp._tcp (Operating Principles 3.5.1).
* A session is a separate DNS-SD service: instance = unique session name,
  type _rtsp._tcp, subtype _ravenna_session._sub._rtsp._tcp (3.5.2.1).
* A controller browses the subtype, resolves host/port, then sends
  DESCRIBE rtsp://<host>:<port>/by-name/<session name> (3.5.3.3).
* SDP carries RAVENNA extensions: session-level a=clock-domain:PTPv2 <domain>
  and stream-level a=sync-time:<u32> (3.3.3.1/3.3.3.2).
* The RTSP server should answer NOT FOUND but keep the connection alive when a
  session does not exist yet, so SDP can be pushed later via ANNOUNCE (3.4.1).

## Build / test

    cargo build
    cargo test

Both were last run on 2026-11-24 (rustc 1.99.1) and passed.

## If this is resumed

1. Read the report (path above), especially the "resume plan" section.
2. Run the crate's own browse/announce pair locally (two shells).
3. Verify with python-zeroconf and ffprobe as described in the report.
4. Only then decide whether the device side goes through Android NsdManager
   (see android/README.md: NsdManager cannot register DNS-SD subtypes) or through
   a native responder sharing UDP/5353 with the Android system mDNS daemon.

## Integration points

* SDP: implement SdpProvider with the NSP nsp_aes67 handle
  (nsp_aes67_sdp per flow) - or keep StaticSdp fed from the C daemon's file.
* Media: PlaybackHook::on_play is where the existing RTP sender starts; the
  rtsp repo's rtp.rs silent sender exists only so a DESCRIBE/PLAY test has
  something to carry.
* mDNS port 5353 is taken by the Android system daemon on the Z9X, so the device
  plan is system-NSD-first; that is a separate decision, documented in the report.
