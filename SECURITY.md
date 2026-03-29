# Security Policy

## Reporting A Vulnerability

Do not report security vulnerabilities through public GitHub issues.

Email **security@amptimal.com** with:

- a short description of the issue
- affected interface or crate
- version, branch, or commit if known
- reproduction steps or proof of concept
- expected impact

We aim to acknowledge reports within 2 business days.

## Scope

This policy applies to the public interfaces in this repository:

- workspace crates in `src/`
- the `surge-solve` CLI
- the `surge` Python package

Correctness issues that can silently produce wrong power-system results may be
security-relevant when they can be triggered by crafted or malicious inputs.

## Out Of Scope

- vulnerabilities in third-party or optional runtime dependencies such as SuiteSparse, HiGHS, Ipopt, or Gurobi
- issues in private infrastructure not shipped from this repository
- denial-of-service from pathological study sizes or malformed files in untrusted environments
- example, notebook, or benchmark issues that do not affect shipped interfaces

## Supported Versions

Before the first public release, report issues against `main`.

After release, the latest published release receives security fixes. Older
releases may not.

## Safe Harbor

We consider good-faith research under this policy to be authorized. Please:

- avoid privacy violations or destructive testing
- avoid disrupting users or shared infrastructure
- give us reasonable time to investigate before public disclosure

## Contact

- Security: **security@amptimal.com**
- General contact: **hello@amptimal.com**
