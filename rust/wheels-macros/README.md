# wheels-macros

Internal proc-macro implementation crate for `wheels`.

Consumers should depend on `wheels`, not this crate directly. The public crate
re-exports:

- `#[data_layout(buffer_offset = 0..7)]`
