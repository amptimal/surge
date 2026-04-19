# Surge Documentation

This index covers the release-facing documentation in this repository.

## Start Here

| Document | Purpose |
|---|---|
| [README](../README.md) | Repository overview and public interfaces |
| [Quickstart](quickstart.md) | Shortest route to a working source build |
| [Support And Compatibility](support-compatibility.md) | Supported versions, platforms, and dependencies |
| [Data Model And Conventions](data-model.md) | Per-unit system, sign conventions, and entity schema |
| [Glossary](glossary.md) | Power-systems terminology and acronyms |
| [Native Formats](native-formats.md) | Canonical `.surge.json`, `.surge.json.zst`, and `.surge.bin` guidance |
| [Examples](../examples/README.md) | Packaged release-facing example assets |
| [Architecture](architecture.md) | Workspace structure and dependency boundaries |

## User Workflows

| Document | Purpose |
|---|---|
| [Tutorials](tutorials/) | Markdown walkthroughs for current user workflows |
| [Notebook Index](notebooks/README.md) | Python notebook companions for the tutorial set |
| [Method Fidelity](method-fidelity.md) | What Surge means by exact, approximate, heuristic, and experimental methods |
| [Performance And Scaling](performance.md) | Threading, memory scaling, and large-case guidance |

## Dispatch And Markets

| Document | Purpose |
|---|---|
| [Markets Overview](../markets/README.md) | The `markets/` contract — `Policy`, `Problem`, `solve` — and reusable `DispatchRequest` builder |
| [`markets/rto/`](../markets/rto/README.md) | ISO day-ahead energy + AS clearing reference market |
| [`markets/battery/`](../markets/battery/README.md) | Single-site price-taker BESS reference market |
| [`markets/go_c3/`](../markets/go_c3/README.md) | GO Competition Challenge 3 adapter and solve |
| [`surge-dispatch`](crates/surge-dispatch.md) | Unified SCED/SCUC kernel crate doc |
| [`surge-market`](crates/surge-market.md) | Canonical market-formulation layer crate doc |

## Interfaces And Reference

| Document | Purpose |
|---|---|
| [Crate Docs](crates/README.md) | Per-crate documentation index |
| [CLI Reference](tutorials/06-cli-reference.md) | Current `surge-solve` option surface |
| [Generated CLI Surface](generated/cli-surface.md) | Generated contract from `surge-solve --help` |
| [Generated Python Root Surface](generated/python-root-surface.md) | Generated root-package contract from `.pyi` |
| [Generated Python Namespace Surface](generated/python-namespace-surface.md) | Generated namespace contract from stubs/modules |
| [Python Result Objects](python-results.md) | Fields and methods on all Python result types |
| [Python Exception Reference](python-exceptions.md) | Exception hierarchy and when each type is raised |
| [References](references.md) | Mathematical citations and governing references |

## Contributor Docs

| Document | Purpose |
|---|---|
| [CONTRIBUTING.md](../CONTRIBUTING.md) | Contribution process |
| [Developer Setup](contributing/setup.md) | Contributor environment setup |
| [Method Documentation Checklist](contributing/method-documentation-checklist.md) | What to update when public methods change |
| [VERSIONING.md](VERSIONING.md) | Versioning policy |
| [RELEASING.md](../RELEASING.md) | Release process |
| [SECURITY.md](../SECURITY.md) | Security reporting policy |

## Legal

| Document | Purpose |
|---|---|
| [LICENSE](../LICENSE) | PolyForm Noncommercial license text |
| [COMMERCIAL-LICENSE.md](../COMMERCIAL-LICENSE.md) | Commercial-use overview |
| [license-notes.md](license-notes.md) | Third-party license notes |
| [NOTICE](../NOTICE) | Attribution notices |
