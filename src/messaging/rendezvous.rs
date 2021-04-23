use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use async_std::channel::{RecvError, SendError, TryRecvError};
use async_std::{
    future::timeout,
    sync::{Condvar, Mutex, MutexGuard},
};

pub fn rendezvous<T: Send>() -> (RendSender<T>, RendReceiver<T>) {
    let inner = Arc::new(ChannelInner {
        recv_cnt: AtomicUsize::new(1),
        send_cnt: AtomicUsize::new(1),
        data: Mutex::new((None, 0)),
        send_cond: Condvar::new(),
        recv_cond: Condvar::new(),
        in_process: Condvar::new(),
    });
    (
        RendSender {
            inner: inner.clone(),
        },
        RendReceiver { inner },
    )
}
struct ChannelInner<T> {
    recv_cnt: AtomicUsize,
    send_cnt: AtomicUsize,
    data: Mutex<(Option<T>, usize)>,
    send_cond: Condvar,
    recv_cond: Condvar,
    in_process: Condvar,
}
impl<T> ChannelInner<T> {
    fn spin(&self) -> MutexGuard<(Option<T>, usize)> {
        let mut spin = 0;
        loop {
            if let Some(guard) = self.data.try_lock() {
                return guard;
            }
            for _ in 0..(1 << spin) {
                std::hint::spin_loop();
            }
            spin = 6.min(spin + 1);
        }
    }
    fn sender_exists(&self) -> bool {
        self.send_cnt.load(Ordering::Acquire) != 0
    }
    fn recv_exists(&self) -> bool {
        self.recv_cnt.load(Ordering::Acquire) != 0
    }
}
pub struct RendReceiver<T> {
    inner: Arc<ChannelInner<T>>,
}
impl<T: Send> RendReceiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut guard = self.inner.spin();
        if let Some(val) = guard.0.take() {
            self.inner.in_process.notify_all();
            self.inner.send_cond.notify_one();
            return Ok(val);
        }
        if self.inner.sender_exists() {
            Err(TryRecvError::Empty)
        } else {
            Err(TryRecvError::Closed)
        }
    }
    pub async fn recv_timeout(&self, dur: Duration) -> Result<T, TryRecvError> {
        match timeout(dur, self.recv()).await {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(_)) => Err(TryRecvError::Closed),
            Err(_) => Err(TryRecvError::Empty),
        }
    }
    #[allow(dead_code)]
    pub async fn recv_deadline(&self, deadline: Instant) -> Result<T, TryRecvError> {
        let to_dur = deadline.saturating_duration_since(Instant::now());
        self.recv_timeout(to_dur).await
    }
    pub async fn recv(&self) -> Result<T, RecvError> {
        if !self.inner.sender_exists() {
            return Err(RecvError);
        }
        let mut guard = self.inner.spin();
        while guard.0.is_none() {
            if !self.inner.sender_exists() {
                return Err(RecvError);
            }
            guard = self.inner.recv_cond.wait(guard).await;
        }
        let ret = guard.0.take().unwrap();
        self.inner.in_process.notify_all();
        self.inner.send_cond.notify_one();
        Ok(ret)
    }
}
impl<T> Clone for RendReceiver<T> {
    fn clone(&self) -> Self {
        self.inner.recv_cnt.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Drop for RendReceiver<T> {
    fn drop(&mut self) {
        if self.inner.recv_cnt.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _lock = self.inner.spin();
            self.inner.send_cond.notify_all();
            self.inner.in_process.notify_all();
        }
    }
}
pub struct RendSender<T> {
    inner: Arc<ChannelInner<T>>,
}
impl<T> Clone for RendSender<T> {
    fn clone(&self) -> Self {
        self.inner.send_cnt.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Drop for RendSender<T> {
    fn drop(&mut self) {
        if self.inner.send_cnt.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _lock = self.inner.spin();
            self.inner.recv_cond.notify_all();
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum TrySendError<T> {
    Closed(T),
    Timeout(T),
}
impl<T: Send> RendSender<T> {
    pub async fn send(&self, val: T) -> Result<(), SendError<T>> {
        let mut guard = self.inner.spin();
        while guard.0.is_some() {
            // waiting until the send option
            if !self.inner.recv_exists() {
                return Err(SendError(val));
            }
            guard = self.inner.send_cond.wait(guard).await;
        }
        guard.0 = Some(val);
        let key = guard.1.wrapping_add(1);
        guard.1 = key;

        // wake receiver (if it exists)
        self.inner.recv_cond.notify_one();
        while let (Some(_), k) = &*guard {
            if *k != key {
                break;
            }
            // wait for receiver to signal it has taken something
            if !self.inner.recv_exists() {
                return Err(SendError(guard.0.take().unwrap()));
            }
            guard = self.inner.in_process.wait(guard).await;
        }
        Ok(())
    }
    pub async fn send_deadline(&self, val: T, deadline: Instant) -> Result<(), TrySendError<T>> {
        let mut guard = self.inner.spin();
        while guard.0.is_some() {
            // waiting until the send option
            if !self.inner.recv_exists() {
                return Err(TrySendError::Closed(val));
            }
            let to_dur = deadline.saturating_duration_since(Instant::now());
            let waiter = self.inner.send_cond.wait(guard);
            guard = match async_std::future::timeout(to_dur, waiter).await {
                Ok(g) => g,
                Err(_) => return Err(TrySendError::Timeout(val)),
            };
        }
        guard.0 = Some(val);
        let key = guard.1.wrapping_add(1);
        guard.1 = key;

        // wake receiver (if it exists)
        self.inner.recv_cond.notify_one();
        while let (Some(_), k) = &*guard {
            if *k != key {
                break;
            }
            // wait for receiver to signal it has taken something
            if !self.inner.recv_exists() {
                return Err(TrySendError::Closed(guard.0.take().unwrap()));
            }
            let waiter = self.inner.send_cond.wait(guard);
            let to_dur = deadline.saturating_duration_since(Instant::now());
            guard = match async_std::future::timeout(to_dur, waiter).await {
                Ok(g) => g,
                Err(_) => {
                    let mut guard = self.inner.spin();
                    if let Some(val) = guard.0.take() {
                        self.inner.send_cond.notify_one();
                        return Err(TrySendError::Timeout(val));
                    }
                    guard
                }
            };
            guard = self.inner.in_process.wait(guard).await;
        }
        Ok(())
    }
    #[allow(dead_code)]
    pub async fn send_timeout(&self, val: T, timeout: Duration) -> Result<(), TrySendError<T>> {
        let deadline = Instant::now() + timeout;
        self.send_deadline(val, deadline).await
    }
}
/*struct CallOnDrop<F: FnMut()>(Option<F>);

impl<F: FnMut()> CallOnDrop<F> {
    fn new(f: F) -> Self {
        Self(Some(f))
    }
    fn forget(mut self) {
        self.0.take();
    }
}
impl<F: FnMut()> Drop for CallOnDrop<F> {
    fn drop(&mut self) {
        if let Some(mut f) = self.0.take() {
            (f)()
        }
    }
}*/
#[cfg(test)]
mod tests {
    use super::*;
    use crate::future::poll_n;
    use async_std::future::timeout;
    use futures::future::{join, select, Either};
    use futures::prelude::*;
    use std::time::Duration;
    const MS_100: Duration = Duration::from_millis(100);

    #[async_std::test]
    async fn recv_deadlock() {
        let (sender, recv) = rendezvous();
        let r1 = recv.recv().boxed();
        let r2 = recv.recv().boxed();
        let r1 = match select(r1, futures::future::ready(())).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, r1)) => r1,
        };
        let join_fut = join(sender.send(2u8), r2);
        let (res, u) = timeout(MS_100, join_fut).await.unwrap();
        res.unwrap();
        assert_eq!(u.unwrap(), 2);
        let join_fut = join(sender.send(1u8), r1);
        let (res, u) = timeout(MS_100, join_fut).await.unwrap();
        res.unwrap();
        assert_eq!(u.unwrap(), 1);
    }
    #[async_std::test]
    async fn send_drop() {
        let (sender, recv) = rendezvous::<u8>();
        assert_eq!(recv.try_recv(), Err(TryRecvError::Empty));
        drop(sender);
        assert_eq!(recv.try_recv(), Err(TryRecvError::Closed));
        assert_eq!(recv.recv().await, Err(RecvError));

        println!("Beginning second set of recv drops");
        let (sender, recv) = rendezvous::<u8>();
        let r1 = recv.recv().boxed();
        let r2 = recv.recv().boxed();
        let r1 = match select(r1, futures::future::ready(())).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, r)) => r,
        };
        let r2 = match select(r2, futures::future::ready(())).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, r)) => r,
        };
        drop(sender);
        let (res1, res2) = timeout(MS_100, join(r1, r2)).await.unwrap();
        assert_eq!(res1, Err(RecvError));
        assert_eq!(res2, Err(RecvError));
    }
    #[async_std::test]
    async fn send_timeout() {
        let (sender, recv) = rendezvous::<u8>();
        let s1 = sender.send_timeout(1, Duration::from_millis(100)).boxed();
        let s1 = poll_n(s1, 1).await.unwrap_pending();
        let s2 = sender.send(2).boxed();
        let s2 = poll_n(s2, 1).await.unwrap_pending();
        assert_eq!(s1.await, Err(TrySendError::Timeout(1)));
        let join_fut = join(recv.recv(), s2);
        let (res1, res2) = timeout(MS_100, join_fut).await.unwrap();
        assert_eq!(res1, Ok(2));
        res2.unwrap();

        let s2 = sender.send(2).boxed();
        let s2 = poll_n(s2, 1).await.unwrap_pending();
        let res1 = sender.send_timeout(1, Duration::from_millis(100)).await;
        assert_eq!(res1, Err(TrySendError::Timeout(1)));
        let join_fut = join(recv.recv(), s2);
        let (res1, res2) = timeout(MS_100, join_fut).await.unwrap();
        assert_eq!(res1, Ok(2));
        res2.unwrap();
    }
    #[async_std::test]
    async fn recv_drop() {
        let (sender, recv) = rendezvous::<u8>();
        let to_res = timeout(Duration::from_secs(0), sender.send(0)).await;
        assert!(matches!(to_res, Err(_)));
        assert_eq!(recv.try_recv().unwrap(), 0);
        drop(recv);
        assert_eq!((sender.send(0).await), Err(SendError(0)));

        println!("Beginning second set of recv drops");
        let (sender, recv) = rendezvous::<u8>();
        let s1 = sender.send(1).boxed();
        let s2 = sender.send(2).boxed();
        let s1 = match select(s1, futures::future::ready(())).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, s)) => s,
        };
        let s2 = match select(s2, futures::future::ready(())).await {
            Either::Left(_) => unreachable!(),
            Either::Right((_, s)) => s,
        };
        drop(recv);
        let (res1, res2) = timeout(Duration::from_millis(100), join(s1, s2))
            .await
            .unwrap();
        assert_eq!(res1, Err(SendError(1)));
        assert_eq!(res2, Err(SendError(2)));
    }
    #[async_std::test]
    async fn simple_channel() {
        let (sender, recv) = rendezvous();
        let join_fut = join(sender.send(0u8), recv.recv());
        let (res, u) = timeout(Duration::from_millis(100), join_fut).await.unwrap();
        res.unwrap();
        assert_eq!(u.unwrap(), 0);
    }
}
