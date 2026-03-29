Canonical case bundles in this directory should default to a single tracked `*.surge.json.zst` file per case.
Keep a sibling `PROVENANCE.md` beside each tracked bundle so upstream sourcing,
license context, and refresh steps stay explicit.

Why:
- It keeps the repo small.
- It gives `surge-ac` and CLI tests a deterministic native fixture format.
- RAW, MATPOWER, and CGMES source cases can be regenerated on demand through `surge-solve --convert`.

Other formats (`.m`, `.surge.json`, `.surge.bin`) can be generated on demand via `surge-solve --convert` and should not be checked in.
