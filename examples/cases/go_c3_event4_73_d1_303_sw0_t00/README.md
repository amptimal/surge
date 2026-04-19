# go_c3_event4_73_d1_303_sw0_t00

This bundle packages a GO C3-derived native Surge replay fixture.

## Files

- `go_c3_event4_73_d1_303_sw0_t00.surge.json.zst` - canonical Surge network artifact
- `go_c3_event4_73_d1_303_sw0_t00.dispatch-request.json.zst` - canonical dispatch request artifact
- `go_c3_event4_73_d1_303_sw0_t00.goc3-problem.json.zst` - GO C3 problem snapshot used to build the native artifacts
- `go_c3_event4_73_d1_303_sw0_t00.goc3-context.json.zst` - adapter context required to export native results back to GO C3 solution format
- `metadata.json` - source scenario, derivation, and policy metadata
- `expected-validator-summary.json` - optional validator baseline for replay-verified bundles
- `PROVENANCE.md` - upstream source and refresh notes

## Interval-0 Slice

This derivative keeps the original GO C3 initial state and slices all period-indexed
time-series inputs down to interval `0`.
