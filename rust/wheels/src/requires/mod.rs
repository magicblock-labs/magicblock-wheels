//! Assertion-style helper macros for Solana/Pinocchio programs.
//!
//! The macros are exported at the crate root, so callers normally invoke them
//! as `wheels::require!`, `wheels::require_eq!`, or
//! `wheels::require_n_accounts!`. They are also re-exported from this module
//! for documentation and grouped imports.
//!
//! The comparison macros take the error at the call site:
//!
//! ```rust,no_run
//! # use pinocchio::error::ProgramError;
//! #
//! # fn process() -> Result<(), ProgramError> {
//! # let is_initialized = true;
//! # let actual_owner = 1_u64;
//! # let expected_owner = 1_u64;
//! # let maybe_signer = Some(7_u64);
//! wheels::require!(is_initialized, ProgramError::InvalidArgument);
//! wheels::require_eq!(actual_owner, expected_owner, ProgramError::InvalidArgument);
//! let signer = wheels::require_some!(maybe_signer, ProgramError::InvalidArgument);
//! # let _ = signer;
//! # Ok(())
//! # }
//! ```
//!
//! Account-count macros use standard Solana/Pinocchio errors where those
//! errors describe the failure:
//!
//! - too few accounts returns
//!   [`pinocchio::error::ProgramError::NotEnoughAccountKeys`];
//! - too many accounts has no matching standard error, so it uses a caller
//!   provided `RequireError::TooManyAccountKeys`;
//! - slice-to-array conversion failures after an exact length check should be
//!   unreachable, so they use a caller provided `RequireError::InfallibleError`.
//!
//! To use the account-count macros, define `RequireError` at the crate root of
//! the program using `wheels`:
//!
//! ```rust,no_run
//! pub type RequireError = MyProgramError;
//!
//! #[repr(u32)]
//! pub enum MyProgramError {
//!     TooManyAccountKeys = 41,
//!     InfallibleError = 100,
//! }
//!
//! impl From<MyProgramError> for pinocchio::error::ProgramError {
//!     fn from(error: MyProgramError) -> Self {
//!         pinocchio::error::ProgramError::Custom(error as u32)
//!     }
//! }
//! ```
//!
//! Required `RequireError` members:
//!
//! - `TooManyAccountKeys`: used by [`crate::require_n_accounts!`] when the
//!   account slice has more entries than the required count.
//! - `InfallibleError`: used by [`crate::require_n_accounts!`],
//!   [`crate::require_n_accounts_with_optionals!`], and
//!   [`crate::require_n_accounts_with_ignored!`] if a slice-to-array conversion
//!   fails after the macro has already checked or split to the exact length.
//!
//! The functions using account-count macros should return
//! `Result<_, pinocchio::error::ProgramError>`.

mod accounts_comparators;
mod comparators;
mod pubkeys_comparators;

pub use crate::{
    require, require_eq, require_eq_keys, require_ge, require_gt, require_le, require_lt,
    require_n_accounts, require_n_accounts_with_ignored, require_n_accounts_with_optionals,
    require_ne, require_ne_keys, require_ok, require_owned_by, require_signer, require_some,
};
