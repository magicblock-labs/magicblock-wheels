# wheels-macros

Internal proc-macro implementation crate for `wheels`.

Consumers should depend on `wheels`, not this crate directly. The public crate
re-exports:

- `#[fixed_offset_layout]`
- `#[variable_offset_layout(buffer_offset = 0..7)]`
