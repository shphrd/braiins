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

//! The Antminer S9 errors

use failure::{Backtrace, Context, Fail};
use std::fmt::{self, Debug, Display};

use std::io;
use sysfs_gpio;
use uio_async;

pub struct Error {
    inner: Context<ErrorKind>,
}

#[derive(Clone, Eq, PartialEq, Debug, Fail)]
pub enum ErrorKind {
    /// General error used for more specific input/output error.
    #[fail(display = "{}", _0)]
    General(String),

    /// Standard input/output error.
    #[fail(display = "IO: {}", _0)]
    Io(String),

    /// Error tied to a particular UIO device
    #[fail(display = "UIO device {}: {}", _0, _1)]
    UioDevice(String, String),

    /// Generic UIO error
    #[fail(display = "UIO: {}", _0)]
    Uio(String),

    /// Unexpected version of something.
    #[fail(display = "Unexpected {} version: {}, expected: {}", _0, _1, _2)]
    UnexpectedVersion(String, String, String),

    /// Error concerning hashboard with specific index.
    #[fail(display = "Hashboard {}: {}", _0, _1)]
    Hashboard(usize, String),

    /// Error concerning hashchip.
    #[fail(display = "Hashchip: {}", _0)]
    Hashchip(String),

    /// Work or command FIFO timeout.
    #[fail(display = "FIFO: {}: {}", _0, _1)]
    Fifo(Fifo, String),

    /// Baud rate errors.
    #[fail(display = "Baud rate: {}", _0)]
    BaudRate(String),

    /// GPIO errors.
    #[fail(display = "GPIO: {}", _0)]
    Gpio(String),

    /// I2C errors.
    #[fail(display = "I2C: {}", _0)]
    I2c(String),

    /// Power controller errors.
    #[fail(display = "Power: {}", _0)]
    Power(String),
}

#[derive(Clone, Eq, PartialEq, Debug, Fail)]
pub enum Fifo {
    #[fail(display = "timed out")]
    TimedOut,
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

impl From<Context<String>> for Error {
    fn from(context: Context<String>) -> Self {
        Self {
            inner: context.map(|info| ErrorKind::General(info)),
        }
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

impl From<uio_async::UioError> for Error {
    fn from(uio_error: uio_async::UioError) -> Self {
        let msg = uio_error.to_string();
        Self {
            inner: uio_error.context(ErrorKind::Uio(msg)),
        }
    }
}

impl From<sysfs_gpio::Error> for Error {
    fn from(gpio_error: sysfs_gpio::Error) -> Self {
        let msg = gpio_error.to_string();
        Self {
            inner: gpio_error.context(ErrorKind::Gpio(msg)),
        }
    }
}

/// A specialized `Result` type bound to [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
