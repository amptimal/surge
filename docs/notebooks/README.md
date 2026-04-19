# Surge Notebooks

These notebooks are the Python companions to the tutorial set. They are public
release documentation for Python users and should stay aligned with the current
`surge` package surface.

The canonical packaged case bundle for repo-level examples lives under
`examples/cases/ieee118/`.

## How To Use Them

- build the Python package from source first
- open the notebook that matches the workflow you care about
- compare the notebook with the corresponding markdown tutorial when you want
  more narrative context

```bash
pip install maturin jupyterlab pandas matplotlib
cd src/surge-py && maturin develop --release
jupyter lab
```

## Notebook Set

| Notebook | Markdown companion | Focus |
|---|---|---|
| [01-basic-power-flow.ipynb](01-basic-power-flow.ipynb) | [Tutorial 01](../tutorials/01-basic-power-flow.md) | AC, DC, and FDPF comparisons |
| [02-contingency-analysis.ipynb](02-contingency-analysis.ipynb) | [Tutorial 02](../tutorials/02-contingency-analysis.md) | N-1, N-2, and study reuse |
| [03-optimal-power-flow.ipynb](03-optimal-power-flow.ipynb) | [Tutorial 03](../tutorials/03-optimal-power-flow.md) | DC-OPF, AC-OPF, and SCOPF |
| [04-transfer-capability.ipynb](04-transfer-capability.ipynb) | [Tutorial 04](../tutorials/04-transfer-capability.md) | ATC and voltage-stress workflows |
| [05-python-workbench.ipynb](05-python-workbench.ipynb) | [Tutorial 05](../tutorials/05-python-api.md) | Root package versus namespaces |
| [06-cli-companion.ipynb](06-cli-companion.ipynb) | [Tutorial 06](../tutorials/06-cli-reference.md) | Driving `surge-solve` from Python |
| [07-node-breaker.ipynb](07-node-breaker.ipynb) | [Tutorial 07](../tutorials/07-node-breaker.md) | Retained topology and rebuilds |
| [08-shift-factors.ipynb](08-shift-factors.ipynb) | [Tutorial 08](../tutorials/08-shift-factors.md) | PTDF, flowgate shift factors, OTDF |
| [09-pandas-construction.ipynb](09-pandas-construction.ipynb) | [Tutorial 09](../tutorials/09-pandas-construction.md) | Build networks from pandas DataFrames |
| [10-subsystems.ipynb](10-subsystems.ipynb) | [Tutorial 10](../tutorials/10-subsystems.md) | Bus-set filters, areas, tie lines |
| [11-loss-sensitivities.ipynb](11-loss-sensitivities.ipynb) | [Tutorial 11](../tutorials/11-loss-sensitivities.md) | Marginal loss factors, loss-aware OPF, LMP decomposition |
| [12-dispatch-activsg.ipynb](12-dispatch-activsg.ipynb) | [Tutorial 12](../tutorials/12-dispatch-activsg.md) | 24-hour ACTIVSg dispatch and LMP heat maps |

## Requirements

- Python 3.12 through 3.14
- `surge` built from this repository
- `numpy`
- `pandas`
- `matplotlib`
- JupyterLab or another notebook frontend

Some workflows require user-supplied case files or topology models. Those
notebooks should skip cleanly when the input files are not present rather than
raising placeholder exceptions.
