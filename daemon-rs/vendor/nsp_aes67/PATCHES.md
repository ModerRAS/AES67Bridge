# Vendored nsp_aes67 patches (AES67Bridge daemon)

Source: `C:\WorkSpace\Claw\scratch\lead\aes67-std-ds\modules\aes67`
(branch `feat/aes67-std-ports-rates-ptime`, HEAD `0091041`), copied
2026-11-24. Only the files below were changed; everything else is upstream.

## 1. `src/lib.rs`
`mod session;` -> `pub mod session;` so the daemon can drive the Rust
`Session`/`Config` API directly instead of going through the C ABI.

## 2. `src/time/mod.rs`
Added `ClockFit::identity()`: the local (non-PTP) media clock maps local
monotonic nanoseconds 1:1 to media-clock nanoseconds.

## 3. `src/ptp/mod.rs`
Added `LocalClock` + `Clock` enum. `Clock::open(..., local)` selects the PTP
follower (upstream behaviour) or a free-run local clock with no PTP sockets.
This is what lets the bridge daemon start RTP without a grandmaster and still
emit AES67 `ts-refclk:local` instead of blocking until PTP lock.

## 4. `src/sdp/mod.rs`
`describe` is now a wrapper over `describe_with_clock`; added
`describe_local` and a `local_clock` flag in `media_block` so local mode emits
`a=ts-refclk:local` + `a=mediaclk:direct=0` (the C v5.3 lines) instead of
omitting both attributes.

## 5. `src/session/mod.rs`
* `Config.local_clock: bool` (set false by the C ABI, true for the bridge
  daemon when `--ptp` is not selected).
* `Network.ptp: ptp::Clock`.
* `activate()` picks `sdp::describe` (PTP) or `sdp::describe_local`.
* Added `Session::ring_frames()` and `Session::sdp_text(index)` accessors for
  the daemon `status.json` ring fill and SDP file contract.

## 6. `src/rtp/mod.rs`
`Output::send` validated `channels == self.flow.channels`, but
`Session::send_interval` passes the session-wide interleaved buffer width, so
every session with more than one flow failed with `InvalidInput` and died after
32 send errors. The check is now
`channels >= self.flow.offset + self.flow.channels`; the per-flow slice logic
was already correct.

The C-v5.3 fragmentation path was also unreachable: the blanket
`RTP_HEADER + count * frame_bytes > PACKET_CAPACITY` guard ran before the
`fragment_mtu` split, so a 16-channel L24 1 ms interval (2316 bytes) was
rejected instead of split into two fragments. The guard now runs per fragment
(the split is bounded by `min(mtu - RTP_HEADER, MAX_RTP_PAYLOAD)`); whole
intervals that still exceed the 1440-byte budget remain rejected.

## 7. `src/sdp/mod.rs` (test only)
The `source_filter_is_emitted_for_multicast_when_enabled` test initialised
`Flow` without the newly added `fragment_mtu` field, so the vendored crate did
not compile under `cargo test --manifest-path vendor/nsp_aes67/Cargo.toml`
(the path dependency never builds its `#[cfg(test)]` code). Added
`fragment_mtu: 0` so the upstream suite can run.

## 8. `src/net.rs` (Android target)
The socket-option branches were gated on `windows` or `target_os = "linux"`;
Rust reports Android as `target_os = "android"`, so an armv7 Android build fell
into the unsupported fallback and failed to compile (`name` undefined). The
Linux branches now cover `any(target_os = "linux", target_os = "android")`,
which is also correct at runtime (Android is Linux).

## 9. `src/session/mod.rs` (dropped counter)
`Session::submit` added `frames` to `counters.dropped` when the ring copy
failed (ring full) and returned `QUEUE_FULL`. The bridge daemon reacts to
`QUEUE_FULL` by applying backpressure and retrying the *same* frame index
(`daemon-rs/src/daemon.rs`, "PCM permanent-silence" fix), so no audio is lost
on that path — but every retry still incremented the counter. A saturated ring
therefore reported hundreds of millions of "dropped" frames in `status.json`
while the RTP stream itself was intact (measured 2026-11-25: 435k frames/s of
phantom drops during a 12ch soak whose received audio had zero discontinuities).
The ring-full branch now returns `QUEUE_FULL` without touching the counter;
`audio.discard` and oversized chunks still count as drops.

## Upstream follow-ups
These patches are candidates for the NSP `feat/aes67-std-ports-rates-ptime`
branch: local-clock mode, SDP local refclk, multi-flow `Output::send` fix and
the two accessors are all generally useful.
