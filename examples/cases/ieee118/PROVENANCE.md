# IEEE 118-Bus Bundle Provenance

This bundle packages a first-party Surge representation of the IEEE 118-bus
test case.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  "IEEE 118-Bus System"
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/ieee-118-bus-system/`
- Accessed for release packaging review: March 15, 2026

Texas A&M states that its test cases are free for commercial or non-commercial
use.

## Packaging Policy

- `case118.surge.json.zst` is the canonical checked-in Surge artifact for this
  bundle.
- The repository does not intentionally ship the MATPOWER-distributed
  `case118.m` wrapper text verbatim.

## Refresh Procedure

If the underlying IEEE 118 data is intentionally refreshed:

1. Start from an approved public upstream source for the IEEE 118-bus dataset.
2. Convert/import it into Surge.
3. Regenerate `case118.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
