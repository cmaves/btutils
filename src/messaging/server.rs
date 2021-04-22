use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_std::channel::{bounded, unbounded, Receiver, Sender, TryRecvError};
use async_std::sync::Mutex;
use futures::future::poll_fn;
use futures::prelude::*;
use futures::task::Poll;

use gatt::server::{Application, Characteristic, Service, ShouldNotify};
use gatt::{AttValue, CharFlags, ValOrFn};
use rustable::gatt;

use super::{Error, LatDeque, Stats, DEFAULT_LAT_PERIOD, SEND_TIMEOUT};
use super::{SERV_IN, SERV_OUT, SERV_UUID};

#[derive(Clone)]
pub struct ServerOptions {
    pub target_lt: Duration,
    pub lat_period: usize,
}
impl Default for ServerOptions {
    fn default() -> Self {
        Self::new()
    }
}
impl ServerOptions {
    pub fn new() -> Self {
        ServerOptions {
            lat_period: DEFAULT_LAT_PERIOD,
            target_lt: Duration::from_millis(8) * DEFAULT_LAT_PERIOD as u32,
        }
    }
}

pub struct MsgChannelServ {
    inbound: Receiver<AttValue>,
    outbound: Sender<AttValue>,
    not_sender: Sender<()>,
    stats: Arc<Stats>,
}

struct OutData {
    sent: VecDeque<(NonZeroU32, Instant)>,
    idx: u32,
    target_lt: Duration,
    recv: Receiver<AttValue>,
    stats: Arc<Stats>,
    lat: LatDeque,
    timeout: Instant,
}

impl OutData {
    fn try_send(&mut self) -> AttValue {
        let now = Instant::now();
        if self.can_send() {
            match self.recv.try_recv() {
                Ok(mut val) => {
                    let idx = NonZeroU32::new(self.idx).unwrap_or_else(|| unsafe {
                        self.idx += 1;
                        NonZeroU32::new_unchecked(self.idx)
                    });
                    self.idx += 1;
                    self.sent.push_back((idx, Instant::now()));
                    for (src, dest) in idx.get().to_le_bytes().iter().zip(&mut val[..4]) {
                        *dest = *src;
                    }
                    let to_dur = self.lat.avg_lat().max(Duration::from_micros(1450)) * 16;
                    self.timeout = now + to_dur;
                    val
                }
                Err(TryRecvError::Empty) => {
                    self.timeout = now + Duration::from_secs(3600);
                    AttValue::new(4)
                }
                Err(TryRecvError::Closed) => {
                    self.timeout = now + Duration::from_secs(3600 * 365 * 1000);
                    AttValue::new(4)
                }
            }
        } else {
            self.timeout = now + Duration::from_secs(3600);
            AttValue::new(4)
        }
    }
    fn handle_write(&mut self, val: AttValue) -> (Option<ValOrFn>, bool) {
        if val.len() != 4 {
            return (None, false);
        }
        let mut idx = [0; 4];
        idx.copy_from_slice(&val[..4]);
        let idx = u32::from_le_bytes(idx);
        match NonZeroU32::new(idx) {
            Some(i) => {
                if let Some(loc) = self.sent.iter().position(|s| s.0 == i) {
                    let sent = loc + 1;
                    let (_, inst) = self.sent.drain(0..sent).last().unwrap();
                    self.lat.push_new(inst);
                    self.stats.update_lat(self.avg_lat());
                    self.stats.update_recvd_sent(1, sent as u32);
                }
            }
            None => self.sent.clear(),
        }
        (None, self.can_send())
    }
    fn can_send(&self) -> bool {
        !self.recv.is_empty() && self.avg_lat() * self.sent.len() as u32 <= self.target_lt
    }
    fn avg_lat(&self) -> Duration {
        self.lat.avg_lat()
    }
}
impl MsgChannelServ {
    pub fn new(app: &mut Application, options: &ServerOptions) -> Self {
        assert_ne!(options.lat_period, 0);

        let mut primary = Service::new(SERV_UUID, true);

        let mut flags = CharFlags::default();
        flags.write_wo_response = true;
        flags.write = true;
        flags.notify = true;
        // flags.indicate = true;

        let mut serv_out = Characteristic::new(SERV_OUT, flags);
        let (outbound, recv) = bounded(1);
        let stats: Arc<Stats> = Arc::default();
        let (not_sender, not_recv) = unbounded();
        let data = Arc::new(Mutex::new(OutData {
            recv,
            stats: stats.clone(),
            target_lt: options.target_lt,
            lat: LatDeque::new(options.lat_period),
            sent: VecDeque::new(),
            timeout: Instant::now() + Duration::from_secs(3600),
            idx: 0,
        }));
        let data_not = data.clone();
        let data_clone = data.clone();
        serv_out.set_value(ValOrFn::Function(Box::new(move || {
            let mut data_lock = data_not.try_lock().unwrap();
            data_lock.try_send()
        })));
        serv_out.set_write_cb(move |val| {
            let mut data_lock = data.try_lock().unwrap();
            data_lock.handle_write(val)
        });

        serv_out.set_notify_cb(move || {
            let data = data_clone.clone();
            let recv = not_recv.clone();
            async move {
                let data_lock = data.try_lock().unwrap();
                let mut to = data_lock.timeout;
                drop(data_lock);
                loop {
                    let to_dur = to.saturating_duration_since(Instant::now());
                    match async_std::future::timeout(to_dur, recv.recv()).await {
                        Ok(_) => break,
                        Err(_) => {
                            let data_lock = data.try_lock().unwrap();
                            if to == data_lock.timeout {
                                break;
                            }
                            to = data_lock.timeout;
                        }
                    }
                }
                ShouldNotify::Yes
            }
            .boxed()
        });
        let mut serv_in = Characteristic::new(SERV_IN, flags);
        let (sender, inbound) = unbounded();
        serv_in.set_value(ValOrFn::Value(AttValue::new(4)));
        serv_in.set_write_cb(move |mut val| {
            if val.len() < 4 {
                return (None, false);
            }
            sender.try_send(val.clone()).unwrap();
            val.resize(4, 0);
            (Some(ValOrFn::Value(val)), true)
        });
        primary.add_char(serv_out);
        primary.add_char(serv_in);
        app.add_service(primary);

        Self {
            stats,
            inbound,
            outbound,
            not_sender,
        }
    }
    pub fn recv_msg(&self) -> impl Future<Output = Result<AttValue, Error>> + Unpin + '_ {
        self.inbound
            .recv()
            .map_err(|_| Error::InThreadHungUp)
            .map_ok(|v| AttValue::from(&v[4..]))
    }
    pub fn try_recv_msg(&self) -> Result<Option<AttValue>, Error> {
        match self.inbound.try_recv() {
            Ok(v) => Ok(Some(AttValue::from(&v[..4]))),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Closed) => Err(Error::ClientThreadHungUp),
        }
    }
    pub fn send_msg(&self, buf: &[u8]) -> impl Future<Output = Result<(), Error>> + Unpin + '_ {
        assert!(buf.len() <= 508);
        let mut val = AttValue::new(4);
        val.extend_from_slice(buf);
        let mut send_fut = self.outbound.send(val);
        let mut timer = async_std::stream::interval(SEND_TIMEOUT);
        poll_fn(move |ctx| {
            if let Poll::Ready(res) = send_fut.poll_unpin(ctx) {
                return Poll::Ready(match res {
                    Ok(()) => Ok(()),
                    Err(_) => Err(Error::OutThreadHungUp),
                });
            }
            let _ = timer.poll_next_unpin(ctx); // prime the context to wake after timeout
            Poll::Pending
        })
        .and_then(move |_| {
            futures::future::ready(match self.not_sender.try_send(()) {
                Ok(()) => Ok(()),
                Err(_) => Err(Error::OutThreadHungUp),
            })
        })
    }
    pub fn get_avg_lat(&self) -> Duration {
        self.stats.get_avg_lat()
    }
    pub fn get_lost_rate(&self) -> f64 {
        self.stats.get_loss_rate()
    }
    pub fn get_sent(&self) -> u32 {
        self.stats.get_sent()
    }
    pub fn reset_recvd_sent(&self) {
        self.stats.reset();
    }
}
