#![no_std]

pub extern crate alloc;

pub mod layout;

mod data_layout_error;
mod requires;

pub use data_layout_error::DataLayoutError;

pub type Pubkey = pinocchio::Address;

pub use wheels_macros::variable_offset_layout;
