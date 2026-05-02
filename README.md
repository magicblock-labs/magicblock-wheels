# magicblock-wheels

Reusable building blocks for MagicBlock projects.

This repo is the shared home for code that should not stay trapped in a single
product repo: Rust crates today, and potentially TS/JS packages or small tools
later.

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

## Rust workspace

```sh
cargo check --workspace
```
