# Packaged Examples

This directory holds release-facing example assets that are safe to reference
from the repository `README`, tutorials, notebooks, and CLI examples.

## Current Bundles

- `cases/ieee118/` — IEEE 118-bus network packaged as first-party Surge
  artifacts: a convenience MATPOWER export plus the native Surge formats
  `surge-json`, `surge-json.zst`, and `surge-bin`
- `cases/case_ACTIVSg10k/` — large synthetic WECC-scale transmission model
  packaged as first-party Surge artifacts
- `cases/pglib_opf_case9241_pegase/` — large European PEGASE benchmark
  packaged as first-party Surge artifacts
- `cases/pglib_opf_case10192_epigrids/` — large synthetic Midwest EPIGRIDS
  benchmark packaged as first-party Surge artifacts
- `cases/pglib_opf_case30000_goc/` — very large ARPA-E GO competition synthetic
  benchmark packaged as first-party Surge artifacts

## Packaging Rules

- Keep example assets self-contained under `examples/`.
- Prefer `.surge.json` when you want an inspectable canonical document.
- Prefer `.surge.json.zst` for efficient on-disk native examples.
- Keep `.surge.bin` beside the text variants so every native format is visible
  and testable.
- Keep provenance notes alongside generated assets so upstream sourcing and
  regeneration are explicit.
- Do not point release-facing docs at `tests/` fixtures or sibling repositories.
