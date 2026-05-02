#![no_std]

pub extern crate alloc;

pub use wheels_macros::{fixed_offset_layout, variable_offset_layout};

#[doc(hidden)]
pub mod __private {
    pub use crate::alloc;
    pub use bytemuck;
    pub use pinocchio;
    pub use pinocchio_log;
}
