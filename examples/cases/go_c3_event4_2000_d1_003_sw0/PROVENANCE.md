# go_c3_event4_2000_d1_003_sw0 Provenance

This bundle packages first-party Surge artifacts derived from a public GO Competition Challenge 3 dataset.

## Upstream Reference

- Dataset key: `event4_2000`
- Title: Challenge 3 Event 4 2000-bus Synthetic Dataset Scenarios
- URL: `https://data.openei.org/files/5997/C3E4N02000_20231002.zip`
- Released: 2023-10-02
- Scenario: `D1 C3E4N02000D1 scenario_003`
- Switching mode: `SW0`

## Native Packaging Contract

- Canonical network artifact: `go_c3_event4_2000_d1_003_sw0.surge.json.zst`
- Canonical request artifact: `go_c3_event4_2000_d1_003_sw0.dispatch-request.json.zst`
- Canonical GO C3 problem snapshot: `go_c3_event4_2000_d1_003_sw0.goc3-problem.json.zst`
- Canonical adapter context snapshot: `go_c3_event4_2000_d1_003_sw0.goc3-context.json.zst`

## Refresh Procedure

1. Resolve the source GO C3 scenario from the native-case manifest.
2. Rebuild the native network, request, problem snapshot, and adapter context artifacts.
3. For interval-0 variants, slice the source problem to interval `0` before building native artifacts.
4. Re-run validator parity for checked-in replay baselines before updating expected summaries.
