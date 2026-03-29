# Performance And Scaling

This page documents Surge's threading model, scaling behavior, and practical
guidance for getting the best performance from different study types.

## Threading Model

Surge uses [rayon](https://docs.rs/rayon) for data-parallel workloads. Each
parallel entry point (`analyze_n1_branch`, `analyze_n2_branch`,
`parameter_sweep`) builds a **scoped thread pool** for that call. There is no
shared process-wide pool.

**Python:**

```python
import surge

surge.set_max_threads(8)   # Limit to 8 threads
print(surge.get_max_threads())
```

**Default:** all available CPU cores. The setting takes effect on the **next**
parallel call, regardless of when it is made — there is no "first call"
restriction.

**Which operations are parallel:**

| Operation | Parallelism |
|---|---|
| Contingency analysis (N-1, N-2) | Per-contingency AC solves across cores |
| Batch studies (`surge.batch.batch_solve`) | Per-case solves across cores |
| LODF screening | Column-wise LODF computation |
| Single AC/DC power flow | Single-threaded (KLU is sequential) |
| Single OPF solve | Single-threaded (LP/NLP solver internal threading) |

For contingency analysis, the speedup is near-linear up to the core count
because each contingency solve is independent.

## Memory Scaling

| Operation | Memory complexity | Notes |
|---|---|---|
| AC power flow | O(n_buses + n_branches) | Jacobian is sparse; KLU factorization is memory-efficient |
| DC power flow | O(n_buses + n_branches) | Single sparse factorization |
| PTDF (full matrix) | O(n_buses * n_branches) | Dense matrix; use request-based API for subsets |
| LODF (full matrix) | O(n_branches^2) | Use column-wise streaming for large networks |
| N-1 contingency | O(n_contingencies * n_buses) | Per-contingency voltage storage is opt-in |
| DC-OPF | O(n_buses + n_branches) | LP problem size scales linearly |
| AC-OPF | O(n_buses + n_branches) | NLP variables and constraints scale linearly |
| SCOPF | O(n_buses * n_contingencies) | Grows with active contingency constraints |

For large networks (10k+ buses), avoid computing full PTDF or LODF matrices.
Use the request-based APIs (`PtdfRequest`, `LodfRequest`) to compute only the
rows and columns you need.

## Prepared Study Objects

When running multiple analyses on the same network, prepared study objects
amortize setup cost (factorization, topology processing, screening):

| Study | Object | What is reused |
|---|---|---|
| DC power flow + sensitivities | `surge.dc.prepare_study()` | B' factorization, island detection |
| Transfer capability | `surge.transfer.prepare_transfer_study()` | DC factorization, PTDF/LODF kernels |
| Contingency analysis | `surge.contingency.n1_branch_study()` | Base-case solution, screening data |
| AC power flow | `surge.powerflow.PreparedAcPf` | Admittance matrix, Jacobian pattern |

Use prepared studies when you are:

- Running multiple sensitivity queries on one network
- Evaluating multiple transfer paths
- Re-analyzing contingencies with different options
- Sweeping parameters while holding the network fixed

## Practical Guidance

### Large-case AC power flow

- Enable `dc_warm_start=True` (default) for reliable convergence.
- If convergence is slow, try `line_search=True` (default) and
  `startup_policy="adaptive"` for automatic fallback strategies.
- For cases with many zero-impedance branches, enable
  `merge_zero_impedance=True` to reduce system size.

### Large-case OPF

- AC-OPF auto-scales NLP iterations based on problem size when
  `max_iterations=0` (default).
- For networks over 2000 buses, the DC-OPF warm start for AC-OPF is enabled
  automatically (`angle_warm_start="auto"`).
- Use `constraint_screening` to reduce the NLP size for very large networks.
- The PWL cost model (`cost_model="piecewise_linear"`) avoids the QP Hessian,
  which can be faster for some LP solvers.

### Large-case contingency analysis

- Always use LODF screening (`screening="lodf"`) for large N-1 studies.
  Screening typically eliminates 90%+ of branch contingencies.
- Use `top_k` to limit results to the worst contingencies when you only need
  the critical set.
- Set `store_post_voltages=False` (default) unless you need per-contingency
  voltage profiles, as storing voltages for every contingency uses significant
  memory.

### Large-case sensitivities

- Use `PreparedDcStudy` for multiple PTDF/LODF queries on one network.
- Use `LodfColumnBuilder` or `N2LodfColumnBuilder` from `surge.dc.streaming`
  for lazy column-wise computation that avoids materializing the full matrix.
- Specify `monitored_branches` and `outage_branches` in requests to compute
  only the submatrix you need.
