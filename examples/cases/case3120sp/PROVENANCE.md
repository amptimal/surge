# Polish 3120 Summer Peak Bundle Provenance

This bundle packages a first-party Surge representation of `case3120sp`.

## Upstream Reference

- Public reference dataset: MATPOWER case library, `case3120sp`
- Repository URL: `https://github.com/MATPOWER/matpower`
- Case lineage: Polish system summer 2008 morning peak data distributed in
  MATPOWER with permission from Roman Korab
- Accessed for release packaging review: March 28, 2026

MATPOWER's top-level license states that case files are not covered by the code
BSD license; case-specific rights come from the underlying data source and file
header.

## Packaging Policy

- `case3120sp.surge.json.zst` is the canonical checked-in Surge artifact for
  this bundle.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case3120sp`.
2. Convert/import it into Surge.
3. Regenerate `case3120sp.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
