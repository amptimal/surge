# surge-bindings

CLI binary (`surge-solve`) for the Surge power systems analysis engine.

Provides a command-line interface to all Surge solvers: AC/DC power flow,
contingency analysis, OPF, SCOPF, HVDC, and transfer capability.

```bash
cargo build --release --bin surge-solve
./target/release/surge-solve examples/cases/case9/case9.surge.json.zst --method acpf
```
