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


## 10. `src/ptp/codec.rs`, `src/ptp/mod.rs` (ST 2110-30 / ST 2059-2 alignment)
* One-step clocks: added `Body::SyncOneStep(i128)`; Sync decodes to the one-step
  variant when the twoStepFlag is clear and the follower pairs the embedded
  origin timestamp on the event socket (ST 2059-2 sec 6.6 requires followers to
  support one-step and two-step leaders).
* Added `ptp::Profile` (`Aes67` / `St2059_2`): profile default domain (0 / 127),
  announce/sync cadence ranges for diagnostics, announceReceiptTimeout 3
  (replacing the hard-coded 5 misses). `ptp::Config.profile` defaults to Aes67;
  the bridge daemon sets it from `ptp_profile=`. Delay_Req stays at 125 ms
  (inside AES67's permitted -3..5 range and ST 2059-2's default init value).
* `Diagnostics` counts announces/syncs outside the selected profile's range
  (accepted anyway for interoperability; visible in the PTP diagnostics line).

## 11. `src/sdp/mod.rs` (SMPTE2110 channel-order)
`channel_order(labels)` derives `a=fmtp:<pt> channel-order=SMPTE2110.(...)` from
the flow's channel labels (ST/51/71 groups matched greedily, remaining runs as
`U<nn>`), replacing the stereo-only special case, so 5.1/7.1/5.1.2/7.1.4 streams
signal the ST 2110-30 sec 6.2.2 grouping convention.

## 12. `src/rtp/mod.rs`, `src/net.rs` (Level B wire robustness)
* `raise_send_buffer` (SO_SNDBUF, best effort) on RTP outputs.
* `Output::write` retries a transient `WouldBlock` (EAGAIN) a bounded number of
  times; Android returns it at 8000 pkt/s.
* `Output::emit` only advances the sequence number when the packet actually left
  the process, so a failed slot no longer creates an unaccountable gap.

## 13. `src/session/mod.rs` (short packet times, PTP polling)
* For packet times <= 250 us the pacer spins through the interval instead of
  `thread::sleep`: Android/Linux timer coalescing measured p99 wake lateness of
  1.37 ms at 125 us (vs a 125 us slot), which dropped ~38 % of slots. The
  1 ms product path keeps the sleep+spin pacer.
* PTP follower sockets are polled every slot while acquiring and every fourth
  slot once locked at short packet times (8-1000 PTP messages/s do not need a
  8000 Hz poll).
* The first UDP send error per second is printed with its `io::ErrorKind` when
  `NSP_AES67_PTP_DIAGNOSTICS=1`, which is what identified EAGAIN.

## Upstream follow-ups
These patches are candidates for the NSP `feat/aes67-std-ports-rates-ptime`
branch: local-clock mode, SDP local refclk, multi-flow `Output::send` fix and
the two accessors are all generally useful.
