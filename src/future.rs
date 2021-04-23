use futures::future::{select, Either, FusedFuture};
use futures::pin_mut;
use futures::prelude::*;
use futures::task::{Context, Poll};
use std::pin::Pin;

fn either_first<A, B>(
    either: Either<(A::Output, B), (B::Output, A)>,
) -> Either<A::Output, B::Output>
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
    either_first(select(left, right).await)
}

pub enum PolledN<F, O> {
    Pending(F),
    Ready(O, usize),
}
impl<F, O> PolledN<F, O> {
    pub fn unwrap_pending(self) -> F
    where
        O: std::fmt::Debug,
    {
        match self {
            PolledN::Pending(f) => f,
            PolledN::Ready(o, cnt) => panic!("Future returned after {} polls: {:?}", cnt, o),
        }
    }
    pub fn unwrap_ready(self) -> O {
        match self {
            PolledN::Ready(o, _) => o,
            PolledN::Pending(_) => panic!("Future was still pending!"),
        }
    }
}
pub fn poll_n<O, F: Future<Output = O> + Unpin>(fut: F, cnt: usize) -> PollN<F> {
    assert_ne!(cnt, 0);
    PollN {
        cnt,
        fut: Some(fut),
        polled: 0,
    }
}

pub struct PollN<F> {
    cnt: usize,
    polled: usize,
    fut: Option<F>,
}

impl<O, F: Future<Output = O> + Unpin> Future for PollN<F> {
    type Output = PolledN<F, O>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let fut = self.fut.as_mut().unwrap();
        match fut.poll_unpin(cx) {
            Poll::Pending => {
                self.polled += 1;
                if self.cnt == self.polled {
                    Poll::Ready(PolledN::Pending(self.fut.take().unwrap()))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(o) => {
                self.polled += 1;
                self.fut.take();
                Poll::Ready(PolledN::Ready(o, self.polled))
            }
        }
    }
}
impl<F> FusedFuture for PollN<F>
where
    F: Future + Unpin,
{
    fn is_terminated(&self) -> bool {
        self.cnt == self.polled || self.fut.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::{drop_select, poll_n, PolledN};
    use futures::future::{poll_fn, ready, select, Either};
    use futures::task::Poll;

    #[async_std::test]
    async fn poll_once_test() {
        let mut u = 0;
        let t = poll_fn::<u8, _>(|_| {
            u += 1;
            Poll::Pending
        });
        let mut pn = poll_n(t, 16);
        for _ in 0..15 {
            match select(pn, ready(())).await {
                Either::Right((_, p)) => pn = p,
                Either::Left(_) => panic!("PollN returned early!"),
            }
        }
        match drop_select(pn, ready(())).await {
            Either::Left(PolledN::Pending(_)) => {}
            _ => panic!(),
        }
        assert_eq!(u, 16);

        let mut u = 0;
        let t = poll_fn::<u8, _>(|_| {
            u += 1;
            if u == 8 {
                Poll::Ready(32)
            } else {
                Poll::Pending
            }
        });
        let mut pn = poll_n(t, 16);
        for _ in 0..7 {
            match select(pn, ready(())).await {
                Either::Right((_, p)) => pn = p,
                Either::Left(_) => panic!("PollN returned early!"),
            }
        }
        match drop_select(pn, ready(())).await {
            Either::Left(PolledN::Ready(u, cnt)) => {
                assert_eq!(cnt, 8);
                assert_eq!(u, 32);
            }
            _ => panic!(),
        }
        assert_eq!(u, 8);
        let u = poll_n(ready(16u8), 16).await.unwrap_ready();
        assert_eq!(u, 16);
    }
}
