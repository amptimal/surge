# Method Documentation Checklist

Use this checklist whenever a public method, solver mode, or major analysis API
is added or materially changed.

## Required Method Metadata

Every major public method should state:

1. Fidelity class
2. Governing reference
3. Assumptions / simplifications
4. Validation source
5. Known limitations
6. Recommended use

## Required Documentation Behavior

- If a method is not `Reference-equation` or `Empirical standard`, say so in the
  first substantial description, not only in a limitations section.
- Do not label a proxy or screening metric as a canonical literature method.
- Do not describe a heuristic as a proof, certificate, or guarantee.
- If a benchmark or validation artifact is referenced, the linked artifact or
  note must exist in the checked-out `surge-bench` repository.

## Minimum Repo Updates

- Update the relevant crate doc in `docs/crates/`.
- Update `docs/method-fidelity.md` if the public method inventory changes.
- Update the relevant evidence notes in `surge-bench` when the evidence source
  changes.
- Update the `surge-bench` validation manifest when a new harness, report, or
  artifact path is introduced.

## Static Check

No dedicated repository script exists for this checklist today. Treat the items
above as a manual release/doc review until a maintained checker is added.
