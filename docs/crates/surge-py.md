# surge-py - Python Bindings

`surge-py` builds the native extension and the `surge` package layer that
Python users should program against.

## Contract Sources

- [Generated Python root surface](../generated/python-root-surface.md)
- [Generated Python namespace surface](../generated/python-namespace-surface.md)

The package layer under `src/surge-py/python/surge/` is the public contract.
The private `_surge` module is an implementation detail.

## Package Shape

Root functions cover the common one-shot workflows:

- `solve_ac_pf`
- `solve_dc_pf`
- `solve_dc_opf`
- `solve_ac_opf`
- `solve_scopf`

Public namespaces cover the specialist workflows:

- `surge.powerflow`
- `surge.dc`
- `surge.contingency`
- `surge.transfer`
- `surge.io`
- `surge.batch`
- `surge.construction`
- `surge.subsystem`
- `surge.compose`
- `surge.audit`
- `surge.losses`
- `surge.opf`
- `surge.units`
- `surge.contingency_io`

## Typed Interface Rules

- Power flow uses typed options objects.
- OPF uses `(network, options=None, runtime=None)`.
- Batch-study method strings use the canonical method names:
  `acpf`, `dcpf`, `fdpf`, `dc-opf`, `ac-opf`, `scopf`.
- The package layer does not expose legacy compatibility aliases.

## Example

```python
import surge

net = surge.load("case118.surge.json.zst")
ac = surge.solve_ac_pf(net, surge.AcPfOptions())
dcopf = surge.solve_dc_opf(
    net,
    options=surge.DcOpfOptions(enforce_thermal_limits=True),
    runtime=surge.DcOpfRuntime(lp_solver="highs"),
)
print(ac.converged, dcopf.total_cost)
```

## Build Notes

```bash
pip install maturin
cd src/surge-py && maturin develop --release
python -c "import surge; print(surge.version())"
```

Optional commercial backends remain runtime-loaded. If `COPT_HOME` points to a
COPT 8.x install when `surge-py` is built, the wheel bundles the standalone
Surge COPT NLP shim and `surge` auto-configures it at import time. Use
`SURGE_PY_REQUIRE_COPT_NLP_SHIM=1` on release builders when a COPT-enabled
wheel is required.
