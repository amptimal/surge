# ACTIVSg10k Bundle Provenance

This bundle packages a first-party Surge representation of `case_ACTIVSg10k`.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  `ACTIVSg10k`
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/activsg10k/`
- Common case-file lineage: MATPOWER `case_ACTIVSg10k`
- Accessed for release packaging review: March 28, 2026

The public MATPOWER case header states the case is synthetic, non-CEII, and
licensed under the Creative Commons Attribution 4.0 International license.

## Packaging Policy

- `case_ACTIVSg10k.surge.json.zst` is the canonical checked-in Surge artifact.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case_ACTIVSg10k`.
2. Convert/import it into Surge.
3. Regenerate `case_ACTIVSg10k.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
