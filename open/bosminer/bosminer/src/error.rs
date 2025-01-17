// Copyright (C) 2019  Braiins Systems s.r.o.
//
// This file is part of Braiins Open-Source Initiative (BOSI).
//
// BOSI is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Please, keep in mind that we may also license BOSI or any part thereof
// under a proprietary license. For more information on the terms and conditions
// of such proprietary license or if you have any other questions, please
// contact us at opensource@braiins.com.

//! The bosminer errors

use failure::{Backtrace, Context, Fail};
use std::fmt::{self, Debug, Display};

use std::io;

pub struct Error {
    inner: Context<ErrorKind>,
}

#[derive(Clone, Eq, PartialEq, Debug, Fail)]
pub enum ErrorKind {
    /// Standard input/output error.
    #[fail(display = "IO error: {}", _0)]
    Io(String),

    /// General error used for more specific input/output error.
    #[fail(display = "General error: {}", _0)]
    General(String),

    /// Error generated by backend for selected target.
    #[fail(display = "Backend error: {}", _0)]
    Backend(String),
}

/// Implement Fail trait instead of use Derive to get more control over custom type.
/// The main advantage is customization of Context type which allows conversion of
/// any error types to this custom error with general error kind by calling context
/// method on any result type.
impl Fail for Error {
    fn cause(&self) -> Option<&Fail> {
        self.inner.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.inner.backtrace()
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Display::fmt(&self.inner, f)
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.inner.get_context().clone()
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self {
            inner: Context::new(kind),
        }
    }
}

impl From<Context<ErrorKind>> for Error {
    fn from(inner: Context<ErrorKind>) -> Self {
        Self { inner }
    }
}

impl From<String> for Error {
    fn from(msg: String) -> Self {
        ErrorKind::General(msg).into()
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        let msg = e.to_string();
        Self {
            inner: e.context(ErrorKind::Io(msg)),
        }
    }
}

impl From<Context<&str>> for Error {
    fn from(context: Context<&str>) -> Self {
        Self {
            inner: context.map(|info| ErrorKind::General(info.to_string())),
        }
    }
}

impl From<Context<String>> for Error {
    fn from(context: Context<String>) -> Self {
        Self {
            inner: context.map(|info| ErrorKind::General(info)),
        }
    }
}

pub trait ResultExt<T, E> {
    fn context<D>(self, context: D) -> std::result::Result<T, Context<ErrorKind>>
    where
        D: Display + Send + Sync + 'static;

    fn with_context<F, D>(self, f: F) -> std::result::Result<T, Context<ErrorKind>>
    where
        F: FnOnce(&E) -> D,
        D: Display + Send + Sync + 'static;
}

pub mod backend {
    pub use super::ResultExt;
    use super::{Error, ErrorKind};

    use failure::{Context, Fail};
    use std::fmt::Display;

    pub fn from_error<T: Fail>(error: T) -> Error {
        let msg = error.to_string();
        Error {
            inner: error.context(ErrorKind::Backend(msg)),
        }
    }

    pub fn from_error_kind<T: ToString>(kind: T) -> Error {
        Error {
            inner: Context::new(ErrorKind::Backend(kind.to_string())),
        }
    }

    impl<T, E> ResultExt<T, E> for Result<T, E>
    where
        E: Fail,
    {
        fn context<D>(self, context: D) -> Result<T, Context<ErrorKind>>
        where
            D: Display + Send + Sync + 'static,
        {
            self.map_err(|failure| {
                failure
                    .context(context)
                    .map(|info| ErrorKind::Backend(info.to_string()))
            })
        }

        fn with_context<F, D>(self, f: F) -> Result<T, Context<ErrorKind>>
        where
            F: FnOnce(&E) -> D,
            D: Display + Send + Sync + 'static,
        {
            self.map_err(|failure| {
                let context = f(&failure);
                failure
                    .context(context)
                    .map(|info| ErrorKind::Backend(info.to_string()))
            })
        }
    }
}

/// A specialized `Result` type bound to [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
