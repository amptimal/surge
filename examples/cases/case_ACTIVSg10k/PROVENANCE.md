# ACTIVSg10k Bundle Provenance

This bundle packages a first-party Surge representation of `case_ACTIVSg10k`.

## Upstream Reference

- Public reference dataset: Texas A&M Electric Grid Test Case Repository,
  `ACTIVSg10k`
- URL:
  `https://electricgrids.engr.tamu.edu/electric-grid-test-cases/activsg10k/`
- Imported physical-network lineage: PSS/E RAW `ACTIVSg10k.RAW`
- Imported coordinate lineage: PowerWorld AUX `ACTIVSg10k.aux`
- Imported economics and generator-classification lineage: MATPOWER
  `case_ACTIVSg10k.m`
- Accessed for release packaging refresh: April 1, 2026

The public TAMU release materials describe the case as synthetic, non-CEII,
and distributed under the Creative Commons Attribution 4.0 International
license.

## Packaging Policy

- `case_ACTIVSg10k.surge.json.zst` is the canonical checked-in Surge artifact.
- The repository does not intentionally ship the upstream RAW file verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case_ACTIVSg10k`.
2. Prefer the TAMU PSS/E RAW release for the physical network and the
   PowerWorld AUX release for coordinates, plus the MATPOWER release for
   economics and generator metadata.
3. Regenerate `case_ACTIVSg10k.surge.json.zst` through the refresh helper so
   the AUX and MATPOWER backfills are applied during import.
4. Update this provenance note if the upstream source or access date changes.
