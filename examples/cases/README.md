Canonical case bundles in this directory should default to a single tracked `*.surge.json.zst` file per case.
Keep a sibling `PROVENANCE.md` beside each tracked bundle so upstream sourcing,
license context, and refresh steps stay explicit.

Why:
- It keeps the repo small.
- It gives `surge-ac` and CLI tests a deterministic native fixture format.
- RAW, MATPOWER, and CGMES source cases can be regenerated on demand through `surge-solve --convert`.

Other formats (`.m`, `.surge.json`, `.surge.bin`) can be generated on demand via `surge-solve --convert` and should not be checked in.

Exception:
- GO C3 replay bundles may include additional checked-in native sidecars beside the canonical
  `*.surge.json.zst` network artifact.
- These sidecars are limited to the minimum self-contained replay surface: dispatch request,
  GO C3 problem snapshot, adapter context, and metadata/provenance documents.
