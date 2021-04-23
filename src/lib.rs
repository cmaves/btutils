use std::fmt::Display;

pub mod future;

pub use rustable::{MAC, UUID};
pub mod messaging;
pub mod timing;

#[derive(Debug)]
pub enum Error {
    InThreadHungUp,
    OutThreadHungUp,
    ClientThreadHungUp,
    InvalidService,
    FailedToRequestName,
    Ble(rustable::Error),
    Dbus(std::io::Error),
}
impl From<rustable::Error> for Error {
    fn from(err: rustable::Error) -> Self {
        Error::Ble(err)
    }
}
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Dbus(err)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for Error {}
