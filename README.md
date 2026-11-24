# magicblock-wheels

Reusable building blocks for MagicBlock projects.

This repo contains Rust crates and a TypeScript package shared by MagicBlock projects.

⚠️ **This is MagicBlock's internal SDK and reusable codebase. It is not the
user-facing SDK**.

For the user-facing SDK, see:

- `ephemeral-rollups-sdk`: <https://github.com/magicblock-labs/ephemeral-rollups-sdk>

## Layout

- `rust/`: Rust workspace members
- `ts/`: TypeScript packages and utilities

## Current contents

- `rust/wheels`: public `no_std` Rust crate
- `rust/wheels-macros`: proc-macro implementation crate re-exported by `wheels`
- `ts`: `@magicblock-labs/wheels`, a TypeScript package for encoding and decoding layouts

The Rust crates and npm package have independent versions. See the [release guide](RELEASING.md).

## Rust workspace

```sh
cargo check --workspace
```
