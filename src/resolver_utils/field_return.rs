//! Turns what an interface implementer's field method returns into a `Result`.
//!
//! A method in an `#[Object]` impl keeps the return type it was written with,
//! a plain value or a `Result`. An interface calls the same method on each of
//! its implementers, so it accepts either: `(&value).field_return_kind()`
//! resolves to [`FieldReturnResult`] for a `Result` and, through one more
//! autoref, to [`FieldReturnValue`] for anything else.

use crate::Error;

/// Picks the conversion for a `Result`.
#[doc(hidden)]
pub trait FieldReturnResult {
    fn field_return_kind(&self) -> ResultReturn {
        ResultReturn
    }
}

impl<T, E> FieldReturnResult for Result<T, E> {}

/// Picks the conversion for a plain value.
#[doc(hidden)]
pub trait FieldReturnValue {
    fn field_return_kind(&self) -> ValueReturn {
        ValueReturn
    }
}

impl<T> FieldReturnValue for &T {}

/// Converts a field method's `Result`.
#[doc(hidden)]
pub struct ResultReturn;

impl ResultReturn {
    pub fn convert<T, E: Into<Error>>(self, value: Result<T, E>) -> Result<T, Error> {
        value.map_err(Into::into)
    }
}

/// Converts a field method's plain value.
#[doc(hidden)]
pub struct ValueReturn;

impl ValueReturn {
    pub fn convert<T>(self, value: T) -> Result<T, Error> {
        Ok(value)
    }
}
