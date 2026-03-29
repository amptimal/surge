# IEEE 118-Bus Example Bundle

This bundle is the canonical release-facing example case for Surge.

## Files

- `PROVENANCE.md` — upstream source and regeneration policy for this bundle
- `case118.surge.json.zst` — zstd-compressed `surge-json` document, the
  canonical checked-in format for this network bundle

## Native Format Contracts

- `surge-json` schema version: `0.1.0`

The native schema versions are tracked separately from the overall Surge package
version so the document contracts can evolve on their own migration path when
needed.

## Provenance

The checked-in bundle files are first-party Surge exports. They are not shipped
as verbatim copies of MATPOWER-distributed case files.

For upstream source and refresh guidance, see `PROVENANCE.md`.
