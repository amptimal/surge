# Python Exception Reference

Surge defines a hierarchy of exception types for structured error handling.
All Surge exceptions inherit from `SurgeError`.

## Exception Hierarchy

```text
SurgeError
  ConvergenceError
  InfeasibleError
  NetworkError
  TopologyError
    MissingTopologyError
    StaleTopologyError
    AmbiguousTopologyError
    TopologyIntegrityError
  SurgeIOError
```

## When Each Exception Is Raised

### SurgeError

Base class for all Surge exceptions. Catch this to handle any Surge-specific
error generically.

### ConvergenceError

Raised when a solver fails to converge within its iteration limit.

**Common causes:**

- AC power flow (Newton-Raphson) did not reach the mismatch tolerance within
  `max_iterations`. The network may be ill-conditioned, or the starting point
  may be poor.
- FDPF did not converge. Try Newton-Raphson with `dc_warm_start=True`.
- HVDC iteration did not converge.

**How to handle:**

```python
try:
    result = surge.solve_ac_pf(net)
except surge.ConvergenceError:
    result = surge.solve_ac_pf(net, surge.AcPfOptions(flat_start=True))
```

Note: `solve_ac_pf` may return a result with `converged=False` instead of
raising, depending on the startup policy. Check `result.converged` in
addition to catching exceptions.

### InfeasibleError

Raised when an optimization problem has no feasible solution.

**Common causes:**

- DC-OPF with hard generator limits and insufficient generation capacity.
- AC-OPF with conflicting voltage/thermal/generation constraints.
- SCOPF where no dispatch satisfies all post-contingency constraints.

**How to handle:** Relax constraints (e.g., `generator_limit_mode="soft"`) or
inspect the network for data errors.

### NetworkError

Raised for structural errors in the `Network` model.

**Common causes:**

- Duplicate bus numbers.
- Branch referencing a non-existent bus.
- Generator or load on a bus that does not exist.
- Invalid per-unit values (NaN, infinite impedance).
- Construction errors from `surge.construction.from_dataframes()`.

**How to handle:** Fix the input data. Use `surge.audit.audit_model()` to
identify structural issues.

### TopologyError

Base class for topology-related errors. Raised when topology operations fail.

### MissingTopologyError

Raised when a topology operation is requested but the network has no
node-breaker topology data (i.e., `network.topology` is None).

**Common cause:** Trying to rebuild topology on a network loaded from MATPOWER
or PSS/E, which are bus-branch formats without node-breaker data.

### StaleTopologyError

Raised when a solver rejects a network whose topology mapping is marked as
stale. This happens when switch states have been modified but
`rebuild_topology()` has not been called.

**How to handle:** Call topology rebuild before solving:

```python
net.topology.rebuild()
result = surge.solve_ac_pf(net)
```

### AmbiguousTopologyError

Raised when topology rebuild produces an ambiguous bus-branch mapping (e.g.,
overlapping connectivity regions that cannot be resolved to a unique bus
assignment).

### TopologyIntegrityError

Raised when the node-breaker topology data fails internal consistency checks
(e.g., switches referencing non-existent connectivity nodes).

### SurgeIOError

Raised for file I/O errors during `load()`, `save()`, or format-specific
operations.

**Common causes:**

- File not found or unreadable.
- Unsupported file format (unrecognized extension).
- Malformed input file (parse error in MATPOWER, PSS/E, CGMES, etc.).
- Schema version mismatch in native formats.

**How to handle:**

```python
try:
    net = surge.load("case.raw")
except surge.SurgeIOError as e:
    print(f"Failed to load: {e}")
```
