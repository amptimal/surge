# IEEE 300-Bus Bundle Provenance

This bundle packages a first-party Surge representation of `case300`.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  "IEEE 300-Bus System"
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/ieee-300-bus-system/`
- Common case-file lineage: MATPOWER `case300`
- Accessed for release packaging review: March 28, 2026

Texas A&M states that its test cases are free for commercial or non-commercial
use.

## Packaging Policy

- `case300.surge.json.zst` is the canonical checked-in Surge artifact for this
  bundle.
- The repository does not intentionally ship the MATPOWER-distributed
  `case300.m` wrapper text verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for the IEEE 300-bus dataset.
2. Convert/import it into Surge.
3. Regenerate `case300.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
