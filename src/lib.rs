use std::fmt::Display;

use futures::future::{select, Either};
use futures::pin_mut;
use futures::prelude::*;

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

fn factor_fut<A, B>(either: Either<(A::Output, B), (B::Output, A)>) -> Either<A::Output, B::Output>
where
    A: Future,
    B: Future,
{
    match either {
        Either::Left((out, _)) => Either::Left(out),
        Either::Right((out, _)) => Either::Right(out),
    }
}

pub async fn drop_select<A, B>(left: A, right: B) -> Either<A::Output, B::Output>
where
    A: Future,
    B: Future,
{
    pin_mut!(left);
    pin_mut!(right);
    factor_fut(select(left, right).await)
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
