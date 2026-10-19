use pinocchio::error::ProgramError;

const TOO_MANY_ACCOUNT_KEYS: u32 = 42;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestError {
    TooManyAccountKeys,
    InfallibleError,
}

pub type RequireError = TestError;

impl From<TestError> for ProgramError {
    fn from(value: TestError) -> Self {
        match value {
            TestError::TooManyAccountKeys => ProgramError::Custom(TOO_MANY_ACCOUNT_KEYS),
            TestError::InfallibleError => ProgramError::InvalidArgument,
        }
    }
}

fn require_exact(accounts: &[u8]) -> Result<&[u8; 2], ProgramError> {
    Ok(wheels::require_n_accounts!(accounts, 2))
}

fn require_with_optionals(accounts: &[u8]) -> Result<(&[u8; 2], &[u8]), ProgramError> {
    Ok(wheels::require_n_accounts_with_optionals!(accounts, 2))
}

fn require_with_ignored(accounts: &[u8]) -> Result<&[u8; 2], ProgramError> {
    Ok(wheels::require_n_accounts_with_ignored!(accounts, 2))
}

#[test]
fn require_n_accounts_accepts_exact_count() {
    let accounts = [1, 2];

    assert_eq!(require_exact(&accounts).unwrap(), &[1, 2]);
}

#[test]
fn require_n_accounts_uses_standard_error_for_too_few_accounts() {
    let accounts = [1];

    assert_eq!(
        require_exact(&accounts).unwrap_err(),
        ProgramError::NotEnoughAccountKeys
    );
}

#[test]
fn require_n_accounts_uses_caller_error_for_too_many_accounts() {
    let accounts = [1, 2, 3];

    assert_eq!(
        require_exact(&accounts).unwrap_err(),
        ProgramError::Custom(TOO_MANY_ACCOUNT_KEYS)
    );
}

#[test]
fn require_n_accounts_with_optionals_returns_extra_accounts() {
    let accounts = [1, 2, 3, 4];
    let (exact, optionals) = require_with_optionals(&accounts).unwrap();

    assert_eq!(exact, &[1, 2]);
    assert_eq!(optionals, &[3, 4]);
}

#[test]
fn require_n_accounts_with_optionals_uses_standard_error_for_too_few_accounts() {
    let accounts = [1];

    assert_eq!(
        require_with_optionals(&accounts).unwrap_err(),
        ProgramError::NotEnoughAccountKeys
    );
}

#[test]
fn require_n_accounts_with_ignored_accepts_extra_accounts() {
    let accounts = [1, 2, 3, 4];

    assert_eq!(require_with_ignored(&accounts).unwrap(), &[1, 2]);
}

#[test]
fn require_n_accounts_with_ignored_uses_standard_error_for_too_few_accounts() {
    let accounts = [1];

    assert_eq!(
        require_with_ignored(&accounts).unwrap_err(),
        ProgramError::NotEnoughAccountKeys
    );
}
