/// Require `AccountView` owner == `&Address`.
#[macro_export]
macro_rules! require_owned_by {
    ($account:expr, $owner:expr) => {{
        if !pinocchio::address::address_eq(unsafe { $account.owner() }, $owner) {
            pinocchio_log::log!(
                "require_owned_by!({}, {}) failed.",
                stringify!($account),
                stringify!($owner)
            );
            $account.address().log();
            unsafe { $account.owner() }.log();
            $owner.log();
            return Err(pinocchio::error::ProgramError::InvalidAccountOwner);
        }
    }};
}

/// Require `AccountView` is signer.
#[macro_export]
macro_rules! require_signer {
    ($account:expr) => {{
        if !$account.is_signer() {
            pinocchio_log::log!("require_signer!({}) failed.", stringify!($account));
            $account.address().log();
            unsafe { $account.owner() }.log();
            return Err(pinocchio::error::ProgramError::MissingRequiredSignature);
        }
    }};
}

/// Require exactly `N` `AccountView`s; return `&[AccountView; N]`.
#[macro_export]
macro_rules! require_n_accounts {
    ( $accounts:expr, $n:literal) => {{
        match $accounts.len().cmp(&$n) {
            core::cmp::Ordering::Less => {
                pinocchio_log::log!(
                    "Need {} accounts, but got less ({}) accounts",
                    $n,
                    $accounts.len()
                );
                return Err(pinocchio::error::ProgramError::NotEnoughAccountKeys);
            }
            core::cmp::Ordering::Equal => core::convert::TryInto::<&[_; $n]>::try_into($accounts)
                .map_err(|_| crate::RequireError::InfallibleError)?,
            core::cmp::Ordering::Greater => {
                pinocchio_log::log!(
                    "Need {} accounts, but got more ({}) accounts",
                    $n,
                    $accounts.len()
                );
                return Err(crate::RequireError::TooManyAccountKeys.into());
            }
        }
    }};
}

/// Require at least `N` `AccountView`s; return array and extras.
#[macro_export]
macro_rules! require_n_accounts_with_optionals {
    ( $accounts:expr, $n:literal) => {{
        match $accounts.len().cmp(&$n) {
            core::cmp::Ordering::Less => {
                pinocchio_log::log!(
                    "Need {} accounts, but got less ({}) accounts",
                    $n,
                    $accounts.len()
                );
                return Err(pinocchio::error::ProgramError::NotEnoughAccountKeys);
            }
            _ => {
                let (exact, optionals) = $accounts.split_at($n);

                (
                    core::convert::TryInto::<&[_; $n]>::try_into(exact)
                        .map_err(|_| crate::RequireError::InfallibleError)?,
                    optionals,
                )
            }
        }
    }};
}

/// Require at least `N` `AccountView`s; return array and ignore extras.
#[macro_export]
macro_rules! require_n_accounts_with_ignored {
    ( $accounts:expr, $n:literal) => {{
        match $accounts.len().cmp(&$n) {
            core::cmp::Ordering::Less => {
                pinocchio_log::log!(
                    "Need {} accounts, but got less ({}) accounts",
                    $n,
                    $accounts.len()
                );
                return Err(pinocchio::error::ProgramError::NotEnoughAccountKeys);
            }
            _ => {
                let (exact, _) = $accounts.split_at($n);
                core::convert::TryInto::<&[_; $n]>::try_into(exact)
                    .map_err(|_| crate::RequireError::InfallibleError)?
            }
        }
    }};
}
