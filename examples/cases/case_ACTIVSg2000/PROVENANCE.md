# ACTIVSg2000 Bundle Provenance

This bundle packages a first-party Surge representation of `case_ACTIVSg2000`.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  `ACTIVSg2000`
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/activsg2000/`
- Imported physical-network lineage: PSS/E RAW `ACTIVSg2000.RAW`
- Imported coordinate lineage: PowerWorld AUX `ACTIVSg2000.aux`
- Imported economics and generator-classification lineage: MATPOWER
  `case_ACTIVSg2000.m`
- Accessed for release packaging refresh: April 1, 2026

The public TAMU release materials describe the dataset as synthetic, non-CEII,
and distributed under the Creative Commons Attribution 4.0 International
license.

## Packaging Policy

- `case_ACTIVSg2000.surge.json.zst` is the canonical checked-in Surge artifact
  for this bundle.
- The repository does not intentionally ship the upstream RAW file verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case_ACTIVSg2000`.
2. Prefer the TAMU PSS/E RAW release for the physical network and the
   PowerWorld AUX release for coordinates, plus the MATPOWER release for
   economics and generator metadata.
3. Regenerate `case_ACTIVSg2000.surge.json.zst` through the refresh helper so
   the AUX and MATPOWER backfills are applied during import.
4. Update this provenance note if the upstream source or access date changes.
