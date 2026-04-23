# Format Interop Caveats

Surge reads and writes MATPOWER, PSS/E (RAW, RAWX, DYR), XIIDM, UCTE,
OpenDSS, EPC, and CGMES files. Each is a first-class citizen — but no
interchange format is truly lossless with every other tool. This doc
documents the specific round-trip behaviors users should know about
before comparing solver output across tools.

## Summary

- **Topology and nominal data round-trip cleanly** through every
  supported format.
- **Solver-side inputs do not always round-trip exactly.** AVR /
  voltage-setpoint convention, Q-limit enforcement flags, slack-bus
  policy, and some control metadata vary across tool formats and can
  land at a slightly different operating point after a round-trip.
- **For fidelity-critical workflows, use `.surge.json` or `.surge.bin`
  as the exchange format.** Surge's own formats are the only ones
  designed to preserve the full network state verbatim.

## Specific round-trip notes

### MATPOWER (`.m`)

- Generator voltage setpoints are stored per-generator (`Vg` in the
  `gen` matrix). When a case originates in a tool that stores voltage
  setpoints per-bus (e.g. PyPSA's `v_mag_pu_set` on buses), the export
  path folds the per-bus setpoint into each connected generator's
  `Vg`. Reading the MATPOWER file back into Surge then applies those
  per-generator setpoints in Surge's native solver. The operating
  point is usually close to the original but not bit-identical,
  particularly when the source tool and Surge differ on how PV buses
  become PQ under Q-limit enforcement.
- MATPOWER branches with `ratio == 1.0` and `angle == 0.0` are
  written as `ratio = 0.0` (MATPOWER convention for "no transformer").
  Round-tripping preserves behavior but changes the serialized value.

### PSS/E RAW (`.raw`)

- RAW `Vsched` and `Rmpct` carry the voltage-control intent. Surge
  honors both on load. RAW files written from a network that was
  solved elsewhere and then imported into Surge may end up with
  `Vsched` values reflecting the last-solver's convention.
- Switched-shunt state is stored as discrete blocks with per-block
  status. Exporting a continuously-controlled shunt from another tool
  to RAW forces quantization to Surge's step model.

### PyPSA netCDF (`.nc`) — via the optional PyPSA bridge

- Surge exposes `surge.io.pypsa_nc.load(path)` when the optional
  `pypsa` package is installed. This path goes directly from the
  PyPSA netCDF to Surge's domain model without going through MATPOWER
  or pandapower, so it preserves PyPSA's per-bus
  `v_mag_pu_set` setpoints exactly.
- When comparing Surge's solution to a PyPSA scoring tool (e.g.
  PowerAgentBench), prefer the netCDF bridge over the MATPOWER path.

### OpenDSS (`.dss`)

- OpenDSS is distribution-oriented with unbalanced modeling. Surge's
  `.dss` import path is single-phase equivalent. Round-tripping
  preserves positive-sequence behavior; three-phase imbalance is not
  represented on the Surge side.

### XIIDM, UCTE, EPC, CGMES

- These are transmission-planning formats with broader metadata than
  MATPOWER. Surge preserves bus / branch / generator / load /
  control data end-to-end, but vendor-specific extensions (project
  tags, schema extensions) may be dropped on re-export.

## When round-trip fidelity matters

If your workflow compares Surge's solution to another tool's solution
on the same nominal scenario (e.g. benchmark validation), follow
these rules:

1. **If the benchmark is PyPSA-native, use the PyPSA bridge** —
   `surge.io.pypsa_nc.load()` — instead of loading the MATPOWER
   export.
2. **If the benchmark is pandapower-native, compare against
   pandapower's own solution**, not a MATPOWER re-export. The
   MATPOWER export is a convenience path, not a fidelity path.
3. **For multi-tool pipelines under your own control, standardize
   on `.surge.json` or `.surge.bin`** between steps. Use MATPOWER /
   PSS/E / CGMES only at the boundary where a third-party tool
   requires it.

## Why this happens

AC power flow is nonlinear. Different solvers converge on slightly
different operating points when the inputs aren't bit-identical, and
format conversions rarely preserve every solver-relevant input:
- Voltage-setpoint storage: per-bus vs per-generator
- Q-limit enforcement: enabled by default vs disabled
- Slack handling: single-slack vs distributed / headroom
- PV-to-PQ switching rules: magnitude thresholds, iteration count
- Shunt modeling: continuous vs stepped

These are all legitimate choices different tools make. Surge honors
the source file faithfully; round-trip differences typically come
from the export / import step at the format boundary, not from
Surge's own solve.
