# surge-sparse

`surge-sparse` provides the validated sparse matrix types and KLU wrappers used
across the solver stack.

## Public Surface

- `CscMatrix`
- `Triplet`
- `KluSolver`
- `ComplexKluSolver`
- `YBus`
- `SparseError`, `SparseResult`

## Build Contract

- SuiteSparse / KLU is currently part of the build contract for this crate.
- The only optional Cargo feature is `faer`.
- There is no `klu` feature toggle today.

## What This Crate Is For

- validating CSC structure before it reaches solver code
- solving real sparse systems with `KluSolver`
- solving complex admittance systems with `ComplexKluSolver`
- holding validated admittance matrices with `YBus`

## Example

```rust
use surge_sparse::{CscMatrix, KluSolver};

let matrix = CscMatrix::try_new(
    2,
    2,
    vec![0, 2, 4],
    vec![0, 1, 0, 1],
    vec![4.0, 1.0, 1.0, 3.0],
)?;

let mut klu = KluSolver::from_csc(&matrix)?;
klu.factor(matrix.values())?;

let mut rhs = vec![1.0, 2.0];
klu.solve(&mut rhs)?;
# Ok::<(), surge_sparse::SparseError>(())
```
