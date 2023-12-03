#![allow(clippy::uninlined_format_args)]

use std::{
    io,
    num::{ParseIntError, TryFromIntError},
};
use thiserror::Error;

#[derive(Error, Debug)]
#[error("basic error msg: {msg}")]
struct InvalidIoError {
    msg: String,
    value: i32,
    source: io::Error,
}

#[test]
fn test_struct_error() -> Result<(), InvalidIoError> {
    Ok::<_, io::Error>(()).throw_invalid_io("some msg".to_owned(), 32)?;
    Ok::<_, io::Error>(()).throw_invalid_io_with(|| ("some msg".to_owned(), 32))?;

    Ok(())
}

#[derive(Error, Debug)]
enum EnumError {
    #[error("basic error msg: {msg}")]
    InvalidMsg {
        msg: String,
        value: i32,
        source: io::Error,
    },
    #[error("another error: {1}")]
    AnotherError(#[source] ParseIntError, String),
}

trait EnumErrorInvalidMsgThrows<__RETURN> {
    fn throw_invalid_msg(self, msg: String, value: i32) -> Result<__RETURN, EnumError>;
    fn throw_invalid_msg_with<F: FnOnce() -> (String, i32)>(
        self,
        f: F,
    ) -> Result<__RETURN, EnumError>;
}
impl<__RETURN> EnumErrorInvalidMsgThrows<__RETURN> for Result<__RETURN, io::Error> {
    fn throw_invalid_msg(self, msg: String, value: i32) -> Result<__RETURN, EnumError> {
        self.map_err(|e| EnumError::InvalidMsg {
            source: e,
            msg,
            value,
        })
    }
    fn throw_invalid_msg_with<F: FnOnce() -> (String, i32)>(
        self,
        f: F,
    ) -> Result<__RETURN, EnumError> {
        self.map_err(|e| {
            let (msg, value) = f();
            EnumError::InvalidMsg {
                source: e,
                msg,
                value,
            }
        })
    }
}
trait EnumErrorAnotherErrorThrows<__RETURN> {
    fn throw_another(self, _0: String) -> Result<__RETURN, EnumError>;
    fn throw_another_with<F: FnOnce() -> (String)>(self, f: F) -> Result<__RETURN, EnumError>;
}
impl<__RETURN> EnumErrorAnotherErrorThrows<__RETURN> for Result<__RETURN, ParseIntError> {
    fn throw_another(self, _0: String) -> Result<__RETURN, EnumError> {
        self.map_err(|e| EnumError::AnotherError(e, _0))
    }
    fn throw_another_with<F: FnOnce() -> (String)>(self, f: F) -> Result<__RETURN, EnumError> {
        self.map_err(|e| {
            let (_0) = f();
            EnumError::AnotherError(e, _0)
        })
    }
}

#[test]
fn test_basic_enum() -> Result<(), EnumError> {
    Ok::<(), io::Error>(()).throw_invalid_msg("some msg".to_owned(), 32)?;
    Ok::<(), io::Error>(()).throw_invalid_msg_with(|| ("some msg".to_owned(), 32))?;
    Ok::<(), ParseIntError>(()).throw_another("another error".to_owned())?;
    Ok::<(), TryFromIntError>(()).throw_only_source()?;

    Ok(())
}

#[derive(Error, Debug)]
#[error("basic error msg: {msg}")]
struct GenericStructError<T> {
    msg: String,
    value: i32,
    source: io::Error,
    generic: T,
}

#[test]
fn test_generic_struct_error() -> Result<(), GenericStructError<String>> {
    Ok::<_, io::Error>(()).throw_generic_struct(
        "some msg".to_owned(),
        32,
        "generic arg".to_owned(),
    )?;
    Ok::<_, io::Error>(())
        .throw_generic_struct_with(|| ("some msg".to_owned(), 32, "generic arg".to_owned()))?;

    Ok(())
}

#[derive(Error, Debug)]
enum GenericEnumError<T, S> {
    #[error("basic error msg: {msg}")]
    Variant1 { msg: T, source: io::Error },
    #[error("another error: {1}")]
    Variant2(#[source] ParseIntError, S),
}

#[test]
fn test_generic_enum() -> Result<(), GenericEnumError<i32, u32>> {
    Ok::<(), io::Error>(()).throw_variant1(123)?;
    Ok::<(), io::Error>(()).throw_variant1_with(|| 123)?;
    Ok::<(), ParseIntError>(()).throw_variant2(123)?;
    Ok::<(), ParseIntError>(()).throw_variant2_with(|| 123)?;

    Ok(())
}
