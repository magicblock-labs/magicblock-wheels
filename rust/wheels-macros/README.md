# wheels-macros

Internal proc-macro implementation crate for `wheels`.

Consumers should depend on `wheels`, not this crate directly. The public crate
re-exports:

- `#[fixed_offset_layout(buffer_offset = N)]`, where `N` is `0..=7`
- `#[fixed_offset_layout(buffer_offset = unknown)]`
- `#[variable_offset_layout(buffer_offset = N)]`, where `N` is `0..=7`
- `#[variable_offset_layout(buffer_offset = unknown)]`
