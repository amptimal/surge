# Polish Model v33 Bundle Provenance

This bundle packages a first-party Surge representation of `Polish_model_v33`.

## Upstream Reference

- Public benchmark family: MATPOWER Polish-system case library
- Repository URL: `https://github.com/MATPOWER/matpower`
- Closest public case family: `case3120sp` (Polish system summer 2008 morning
  peak)
- Accessed for release packaging review: March 28, 2026

This tracked Surge artifact has the same 3120-bus, 3693-branch, and
505-generator footprint as MATPOWER `case3120sp`. The bundle name indicates a
PSS/E v33-style export lineage, but the exact raw import file is not tracked in
this release tree. Treat this note as a family-level provenance record unless a
more specific source artifact is added.

## Packaging Policy

- `Polish_model_v33.surge.json.zst` is the canonical checked-in Surge artifact
  for this bundle.
- The repository does not intentionally ship any upstream PSS/E or MATPOWER
  wrapper text verbatim.

## Refresh Procedure

1. Start from an approved public upstream source for the corresponding Polish
   benchmark family.
2. Convert/import it into Surge.
3. Regenerate `Polish_model_v33.surge.json.zst`.
4. Replace this family-level note with a source-specific provenance record if a
   tracked raw source is added.
