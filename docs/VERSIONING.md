# Surge Versioning Policy

## First-Party Workspace Version

The first-party Surge crates use the shared version declared in the root
`Cargo.toml` under `[workspace.package]`.

## SemVer Intent

Surge follows Semantic Versioning in intent, with the usual pre-1.0 caveat that
breaking changes can still land in minor releases.

## Current Rules

- Bump the shared workspace version in the root `Cargo.toml` when first-party
  public APIs or packaged behavior change.
- Update `CHANGELOG.md` when shipping a release-worthy change.
- Keep docs aligned with the source-defined interface names whenever public APIs
  change.

## MSRV

The minimum supported Rust version is declared in the root manifest as
`rust-version`. Treat an MSRV bump as a versioned compatibility change and call
it out in the changelog.

## Native Format Schemas

The native `surge-json` and `surge-bin` document formats start at schema
version `0.1.0` for the first public release.

- The schema version is tracked separately from crate/package versions.
- It may diverge when the native document contracts need a migration and the
  rest of Surge does not.
- Breaking native schema changes should bump the relevant schema version and
  include a migration note in the changelog or release notes.
