#![no_std]

pub extern crate alloc;

pub mod layout;
pub mod requires;

mod data_layout_error;

pub use data_layout_error::DataLayoutError;

pub type Pubkey = pinocchio::Address;

pub use wheels_macros::{fixed_offset_layout, variable_offset_layout};
