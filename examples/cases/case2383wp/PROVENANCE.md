# Polish 2383 Winter Peak Bundle Provenance

This bundle packages a first-party Surge representation of `case2383wp`.

## Upstream Reference

- Public reference dataset: MATPOWER case library, `case2383wp`
- Repository URL: `https://github.com/MATPOWER/matpower`
- Case lineage: Polish system winter 1999-2000 peak data distributed in
  MATPOWER with permission from Roman Korab
- Accessed for release packaging review: March 28, 2026

MATPOWER's top-level license states that case files are not covered by the code
BSD license; case-specific rights come from the underlying data source and file
header.

## Packaging Policy

- `case2383wp.surge.json.zst` is the canonical checked-in Surge artifact for
  this bundle.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for `case2383wp`.
2. Convert/import it into Surge.
3. Regenerate `case2383wp.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
