# Changelog

All notable changes to this crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2026-11-19

### Added
- `DEPTH_FACTORS`, mapping `depth_factor_idx` to the depth factor.

### Fixed
- `depth_factor` of a screen-anchored object follows the table of ETSI TS 103 420 (0.25, 0.5, 1.0, 2.0) instead of 0.25 times the index plus one.
- `object_gain_idx` 3 takes the previous object's gain as the block ended up with it, so it is -inf after an inactive object and no longer a stale gain from an earlier one.
- `object_div_mode` 3 is reserved and leaves the divergence at 0, where it was read as a divergence code like mode 2.

## [0.1.1] - 2026-11-14

### Added
- Added a `NOTICE` file with project attribution for downstream distributions.

### Fixed
- OAMD fields cannot read past their declared element boundary. Truncated elements are bounded by the absolute end of the available payload, so their remaining data is not mistaken for another element.
- `ObjectAudioMetadataPayload::read` rejects extended metadata whose block counts no longer match the final object element. This prevents `get_damf_pos` from panicking after an object element is replaced.
- A payload that codes the reserved `sample_offset_code` 3 is read as an error instead of panicking.

## [0.1.0] - 2026-09-17

### Added
- `ObjectAudioMetadataPayload::read`, parsing an OAMD payload from the bytes its carrier hands over, with the object, trim, extended object, headphone and object description elements
- `BedObjectElement` and `DownmixRoute`, the element with `oa_element_id` 6, which says how a bed folds down to fewer outputs
