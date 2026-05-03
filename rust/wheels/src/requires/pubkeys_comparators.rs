///
/// Require $key1 == $key2
///
/// $key1 : &Address
/// $key2 : &Address
///
#[macro_export]
macro_rules! require_eq_keys {
    ( $key1:expr, $key2:expr, $error:expr) => {{
        if !pinocchio::address::address_eq($key1, $key2) {
            pinocchio_log::log!(
                "require_eq_keys!({}, {}) failed: ",
                stringify!($key1),
                stringify!($key2)
            );
            $key1.log();
            $key2.log();
            return Err($error.into());
        }
    }};
}

///
/// Require $key1 != $key2
///
/// $key1 : &Address
/// $key2 : &Address
///
#[macro_export]
macro_rules! require_ne_keys {
    ( $key1:expr, $key2:expr, $error:expr) => {{
        if pinocchio::address::address_eq($key1, $key2) {
            pinocchio_log::log!(
                "require_ne_keys!({}, {}) failed: ",
                stringify!($key1),
                stringify!($key2)
            );
            $key1.log();
            $key2.log();
            return Err($error.into());
        }
    }};
}
