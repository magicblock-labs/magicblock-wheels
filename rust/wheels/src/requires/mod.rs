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
//! // Return the provided error if the condition is false.
//! wheels::require!(is_initialized, ProgramError::InvalidArgument);
//!
//! // Return the provided error if the values are not equal.
//! wheels::require_eq!(actual_owner, expected_owner, ProgramError::InvalidArgument);
//!
//! // Return the inner value, or return the provided error if it is `None`.
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
//! ## `RequireError` Contract
//!
//! To use the account-count macros, the program crate must provide
//! `RequireError` at its crate root.
//!
//! - Define `RequireError` as a type alias or enum in the user project.
//! - Required members:
//!   - `TooManyAccountKeys`
//!   - `InfallibleError`
//! - Match only the variant names; integer values are chosen by each user
//!   project.
//! - Convert the error type into [`pinocchio::error::ProgramError`].
//!
//! ```rust,no_run
//! // Provide this exact name at the crate root.
//! pub type RequireError = DlpError;
//!
//! #[repr(u32)]
//! pub enum DlpError {
//!     // Variant names are required by `wheels`
//!     // The integral values are chosen by user program.
//!     TooManyAccountKeys = 6000,
//!     InfallibleError = 6001,
//!
//!     // User program is free to define more variants.
//! }
//!
//! // Convert the program error into Pinocchio's `ProgramError`.
//! impl From<DlpError> for pinocchio::error::ProgramError {
//!     fn from(error: DlpError) -> Self {
//!         pinocchio::error::ProgramError::Custom(error as u32)
//!     }
//! }
//! ```
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
