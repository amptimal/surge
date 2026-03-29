# RTE 6515 Bundle Provenance

This bundle packages a first-party Surge representation of `case6515rte`.

## Upstream Reference

- Public reference dataset: MATPOWER case library, `case6515rte`
- Repository URL: `https://github.com/MATPOWER/matpower`
- Case lineage: 2013 French RTE transmission-network snapshot distributed
  through MATPOWER
- Accessed for release packaging review: March 28, 2026

The public MATPOWER case header states the case is licensed under the Creative
Commons Attribution 4.0 International license.

## Packaging Policy

- `case6515rte.surge.json.zst` is the canonical checked-in Surge artifact for
  this bundle.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case6515rte`.
2. Convert/import it into Surge.
3. Regenerate `case6515rte.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
