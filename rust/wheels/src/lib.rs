#![no_std]

pub extern crate alloc;

pub mod layout;

mod data_layout_error;
mod requires;

pub use data_layout_error::DataLayoutError;

pub use wheels_macros::data_layout;
