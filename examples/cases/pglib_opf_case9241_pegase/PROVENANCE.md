# PGLib PEGASE 9241 Bundle Provenance

This bundle packages a first-party Surge representation of
`pglib_opf_case9241_pegase`.

## Upstream Reference

- Public reference dataset: IEEE PES Power Grid Library, `pglib-opf`
- Repository URL: `https://github.com/power-grid-lib/pglib-opf`
- Case lineage: PEGASE European benchmark data distributed through PGLib-OPF
- Accessed for release packaging review: March 28, 2026

The public case header states the case is licensed under the Creative Commons
Attribution 4.0 International license.

## Packaging Policy

- `pglib_opf_case9241_pegase.surge.json.zst` is the canonical checked-in
  Surge artifact.
- The repository does not intentionally ship the upstream MATPOWER wrapper text
  verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for
   `pglib_opf_case9241_pegase`.
2. Convert/import it into Surge.
3. Regenerate `pglib_opf_case9241_pegase.surge.json.zst`.
4. Update this provenance note if the upstream source or access date changes.
