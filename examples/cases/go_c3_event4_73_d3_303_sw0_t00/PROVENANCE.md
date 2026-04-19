# go_c3_event4_73_d3_303_sw0_t00 Provenance

This bundle packages first-party Surge artifacts derived from a public GO Competition Challenge 3 dataset.

## Upstream Reference

- Dataset key: `event4_73`
- Title: Challenge 3 Event 4 73-bus Synthetic Dataset Scenarios
- URL: `https://data.openei.org/files/5997/C3E4N00073_20231002.zip`
- Released: 2023-10-02
- Scenario: `D3 C3E4N00073D3 scenario_303`
- Switching mode: `SW0`

## Native Packaging Contract

- Canonical network artifact: `go_c3_event4_73_d3_303_sw0_t00.surge.json.zst`
- Canonical request artifact: `go_c3_event4_73_d3_303_sw0_t00.dispatch-request.json.zst`
- Canonical GO C3 problem snapshot: `go_c3_event4_73_d3_303_sw0_t00.goc3-problem.json.zst`
- Canonical adapter context snapshot: `go_c3_event4_73_d3_303_sw0_t00.goc3-context.json.zst`

## Interval-0 Derivation

- The source GO C3 problem is truncated to one period by taking interval `0` only.
- Original initial commitment, branch, shunt, transformer, and HVDC state are preserved unchanged.
- The single-interval derivative is intended for fundamental interval-level regression tests.

## Refresh Procedure

1. Resolve the source GO C3 scenario from the native-case manifest.
2. Rebuild the native network, request, problem snapshot, and adapter context artifacts.
3. For interval-0 variants, slice the source problem to interval `0` before building native artifacts.
4. Re-run validator parity for checked-in replay baselines before updating expected summaries.
