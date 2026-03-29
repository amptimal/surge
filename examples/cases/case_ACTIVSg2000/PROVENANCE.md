# ACTIVSg2000 Bundle Provenance

This bundle packages a first-party Surge representation of `case_ACTIVSg2000`.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  `ACTIVSg2000`
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/activsg2000/`
- Common case-file lineage: MATPOWER `case_ACTIVSg2000`
- Accessed for release packaging review: March 28, 2026

The public MATPOWER case header states the dataset is synthetic, contains no
CEII, and is licensed under the Creative Commons Attribution 4.0 International
license.

## Packaging Policy

- `case_ACTIVSg2000.surge.json.zst` is the canonical checked-in Surge artifact
  for this bundle.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case_ACTIVSg2000`.
2. Convert/import it into Surge.
3. Regenerate `case_ACTIVSg2000.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
