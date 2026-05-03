///
/// Require $cond == true
///
/// $cond: bool
///
#[macro_export]
macro_rules! require {
    ($cond:expr, $error:expr) => {{
        if !$cond {
            let expr = stringify!($cond);
            pinocchio_log::log!("require!({}) failed.", expr);
            return Err($error.into());
        }
    }};
}

///
/// Require $a < $b
///
/// $a : impl Ord
/// $b : impl Ord
///
#[macro_export]
macro_rules! require_lt {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a < $b) {
            pinocchio_log::log!(
                "require_lt!({}, {}) failed: {} < {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $a <= $b
///
/// $a : impl Ord
/// $b : impl Ord
///
#[macro_export]
macro_rules! require_le {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a <= $b) {
            pinocchio_log::log!(
                "require_le!({}, {}) failed: {} <= {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $a == $b
///
/// $a : impl Eq
/// $b : impl Eq
///
#[macro_export]
macro_rules! require_eq {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a == $b) {
            pinocchio_log::log!(
                "require_eq!({}, {}) failed: {} == {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $a != $b
///
/// $a : impl Eq
/// $b : impl Eq
///
#[macro_export]
macro_rules! require_ne {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a != $b) {
            pinocchio_log::log!(
                "require_ne!({}, {}) failed: {} == {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $a >= $b
///
/// $a : impl Ord
/// $b : impl Ord
///
#[macro_export]
macro_rules! require_ge {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a >= $b) {
            pinocchio_log::log!(
                "require_ge!({}, {}) failed: {} >= {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $a > $b
///
/// $a : impl Ord
/// $b : impl Ord
///
#[macro_export]
macro_rules! require_gt {
    ( $a:expr, $b:expr, $error:expr) => {{
        if !($a > $b) {
            pinocchio_log::log!(
                "require_gt!({}, {}) failed: {} > {}",
                stringify!($a),
                stringify!($b),
                $a,
                $b
            );
            return Err($error.into());
        }
    }};
}

///
/// Require $option to be Some, and produce the value
///
/// $option: Option<T>
///
#[macro_export]
macro_rules! require_some {
    ($option:expr, $error:expr) => {{
        match $option {
            Some(val) => val,
            None => return Err($error.into()),
        }
    }};
}

///
/// Require $result to be Ok, and produce the value
///
/// $result: Result<T, E>
///
#[macro_export]
macro_rules! require_ok {
    ($result:expr, $error:expr) => {{
        match $result {
            Ok(val) => val,
            Err(err) => return Err(err.into()),
        }
    }};
}
