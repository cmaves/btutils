use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_std::channel::{unbounded, Receiver, Sender};
use async_std::task::JoinHandle;
use futures::future::{try_join, try_join4, Either};
use futures::prelude::*;

use async_rustbus::RpcConn;
use gatt::client::{Characteristic, NotifySocket, Service, WriteSocket};
use gatt::AttValue;
use rustable::gatt;
use rustable::{Adapter, MAC};

use super::{ioe_to_blee, Error, LatDeque, DEFAULT_LAT_PERIOD};
use super::{rendezvous, RendReceiver, RendSender};
use super::{Stats, SERV_IN as CLIENT_OUT, SERV_OUT as CLIENT_IN, SERV_UUID};
use crate::future::drop_select;

pub struct ClientOptions<'a> {
    pub target_lt: Duration,
    pub dev: MAC,
    pub hci: u8,
    pub lat_period: usize,
    pub name: Option<&'a str>,
}
impl ClientOptions<'_> {
    pub fn new(dev: MAC) -> Self {
        ClientOptions {
            dev,
            hci: 0,
            lat_period: DEFAULT_LAT_PERIOD,
            target_lt: Duration::from_millis(8) * DEFAULT_LAT_PERIOD as u32,
            name: None,
        }
    }
}

pub struct MsgChannelClient {
    outbound: RendSender<AttValue>,
    inbound: Receiver<AttValue>,
    handle: JoinHandle<Result<(), Error>>,
    stats: Arc<Stats>,
}

struct ClientData {
    lat: LatDeque,
    timeout: Instant,
    sent: VecDeque<(NonZeroU32, Instant)>,
    idx: u32,
    target_lt: Duration,
    sender: Sender<AttValue>,
    recv: RendReceiver<AttValue>,
    stats: Arc<Stats>,
    out_chrc: Characteristic,
    out_write: WriteSocket,
    out_notify: NotifySocket,
    in_chrc: Characteristic,
    in_write: WriteSocket,
    in_notify: NotifySocket,
}

impl ClientData {
    async fn send(&mut self, mut val: AttValue) -> Result<(), Error> {
        debug_assert!(val == AttValue::new(4) || self.can_send());
        let now = Instant::now();
        let idx = self.allocate_idx();
        for (src, dest) in idx.get().to_le_bytes().iter().zip(&mut val[..4]) {
            *dest = *src;
        }
        match self.out_write.write(&val).await {
            Ok(_) => {
                self.sent.push_back((idx, now));
                self.set_timeout(now);
                Ok(())
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotConnected
                    || e.kind() == std::io::ErrorKind::BrokenPipe =>
            {
                let write_fut = self.out_chrc.write_value_wo_response(&val, 0).await?;
                self.sent.push_back((idx, now));
                let sock_fut = self.out_chrc.acquire_write().await?;
                let res = try_join(write_fut, sock_fut).await?;
                self.set_timeout(now);
                self.out_write = res.1;
                self.stats.set_mtu(self.out_write.mtu());
                Ok(())
            }
            Err(e) => Err(rustable::Error::from(e).into()),
        }
    }
    fn set_timeout(&mut self, now: Instant) {
        let to_dur = self.lat.avg_lat().max(Duration::from_micros(7250)) * 16;
        self.timeout = now + to_dur;
    }
    fn allocate_idx(&mut self) -> NonZeroU32 {
        let ret = NonZeroU32::new(self.idx).unwrap_or_else(|| unsafe {
            self.idx += 1;
            NonZeroU32::new_unchecked(self.idx)
        });
        self.idx += 1;
        ret
    }
    fn can_send(&self) -> bool {
        self.lat.avg_lat_adjusted() * self.sent.len() as u32 <= self.target_lt
    }
    async fn handle_send_res(&mut self, res: std::io::Result<AttValue>) -> Result<(), Error> {
        let val = match res {
            Ok(v) => v,
            Err(_) => {
                self.out_notify = self.out_chrc.acquire_notify().await?.await?;
                AttValue::new(0)
            }
        };
        if val.len() != 4 {
            return Ok(());
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
                    self.stats.update_lat(self.lat.avg_lat());
                    self.stats.update_recvd_sent(1, sent as u32);
                }
            }
            None => self.sent.clear(),
        }
        Ok(())
    }
    async fn handle_recv(&mut self, res: std::io::Result<AttValue>) -> Result<(), Error> {
        let mut val = match res {
            Err(_) => {
                self.out_notify = self.out_chrc.acquire_notify().await?.await?;
                return Ok(());
            }
            Ok(v) => v,
        };
        if val.len() < 4 || val == AttValue::new(4) {
            return Ok(());
        }
        self.sender.send(val.clone()).await.unwrap();
        val.resize(4, 0);
        match self.in_write.write(&val).await {
            Ok(_) => Ok(()),
            Err(e)
                if e.kind() == std::io::ErrorKind::NotConnected
                    || e.kind() == std::io::ErrorKind::BrokenPipe =>
            {
                let write_fut = self.in_chrc.write_value_wo_response(&val, 0).await?;
                let sock_fut = self.in_chrc.acquire_write().await?;
                let res = try_join(write_fut, sock_fut).await?;
                self.in_write = res.1;
                Ok(())
            }
            Err(e) => Err(rustable::Error::from(e).into()),
        }
    }
    /*fn drop_oldest(&mut self) {
        self.sent.pop_front();
        self.lat.pop_oldest();
    }*/
    fn get_to(&self) -> Duration {
        self.timeout.saturating_duration_since(Instant::now())
    }
    /*
    fn await_notify(&self) -> impl Future<Output=Either<std::io::Result<AttValue>, std::io::Result<AttValue>>> + Unpin + '_ {
        let in_fut = self.in_notify.recv_notification();
        let out_fut = self.out_notify.recv_notification();
        pin_mut!(in_fut);
        pin_mut!(out_fut);
        select(in_fut, out_fut).map(|e| {
            Either::Left(Ok(AttValue::new(0)))
        })
    }
    */
}
impl MsgChannelClient {
    pub async fn new(options: ClientOptions<'_>) -> Result<Self, Error> {
        let conn = RpcConn::system_conn(true).await.map_err(ioe_to_blee)?;
        Self::from_conn(Arc::new(conn), options).await
    }
    pub async fn from_conn(conn: Arc<RpcConn>, options: ClientOptions<'_>) -> Result<Self, Error> {
        assert_ne!(options.lat_period, 0);
        let hci = Adapter::from_conn(conn, options.hci).await?;
        let dev = hci.get_device(options.dev).await?;
        let serv = dev.get_service(SERV_UUID).await?;
        Self::from_service(serv, options).await
    }
    pub async fn from_service(serv: Service, options: ClientOptions<'_>) -> Result<Self, Error> {
        let chrcs = serv.get_characteristics().await?;
        let mut chrcs = chrcs
            .into_iter()
            .filter(|c| c.uuid() == CLIENT_IN || c.uuid() == CLIENT_OUT);
        let in_chrc;
        let out_chrc;
        match (chrcs.next(), chrcs.next()) {
            (Some(inp), Some(out)) | (Some(out), Some(inp))
                if inp.uuid() == CLIENT_IN && out.uuid() == CLIENT_OUT =>
            {
                in_chrc = inp;
                out_chrc = out;
            }
            _ => return Err(Error::InvalidService),
        }
        let (in_notify, out_notify, in_write, out_write) = {
            let f = try_join4(
                in_chrc.acquire_notify(),
                out_chrc.acquire_notify(),
                in_chrc.acquire_write(),
                out_chrc.acquire_write(),
            )
            .await?;
            try_join4(f.0, f.1, f.2, f.3).await?
        };
        let (outbound, recv) = rendezvous();
        let (sender, inbound) = unbounded();
        let stats: Arc<Stats> = Arc::default();
        stats.set_mtu(out_write.mtu());
        let mut data = ClientData {
            in_chrc,
            in_notify,
            in_write,
            out_chrc,
            out_notify,
            out_write,
            sender,
            recv,
            lat: LatDeque::new(options.lat_period),
            target_lt: options.target_lt,
            idx: 0,
            stats: stats.clone(),
            sent: VecDeque::new(),
            timeout: Instant::now(),
        };
        let handle = async_std::task::spawn(async move {
            loop {
                //data.try_send().await?;
                let in_fut = data.in_notify.recv_notification();
                let out_fut = data.out_notify.recv_notification();
                let await_notify = drop_select(in_fut, out_fut);
                if data.can_send() {
                    match drop_select(data.recv.recv(), await_notify).await {
                        Either::Left(val) => match val {
                            Ok(v) if v.len() > 0 => data.send(v).await?,
                            _ => break,
                        },
                        Either::Right(Either::Left(in_not)) => data.handle_recv(in_not).await?,
                        Either::Right(Either::Right(out_not)) => {
                            data.handle_send_res(out_not).await?
                        }
                    }
                } else {
                    let to = data.get_to();
                    match async_std::future::timeout(to, await_notify).await {
                        Ok(Either::Left(in_not)) => data.handle_recv(in_not).await?,
                        Ok(Either::Right(out_not)) => data.handle_send_res(out_not).await?,
                        Err(_) => data.send(AttValue::new(4)).await?,
                    }
                }
            }
            unimplemented!()
        });
        Ok(Self {
            inbound,
            outbound,
            handle,
            stats,
        })
    }
    pub fn recv_msg(&self) -> impl Future<Output = Result<AttValue, Error>> + Unpin + '_ {
        self.inbound
            .recv()
            .map_err(|_| Error::ClientThreadHungUp)
            .map_ok(|v| AttValue::from(&v[4..]))
    }
    pub fn try_recv_msg(&self) -> Result<Option<AttValue>, Error> {
        match self.inbound.try_recv() {
            Ok(v) => Ok(Some(AttValue::from(&v[4..]))),
            Err(async_std::channel::TryRecvError::Empty) => Ok(None),
            Err(_) => Err(Error::ClientThreadHungUp),
        }
    }
    pub async fn send_msg(&self, buf: &[u8]) -> Result<(), Error> {
        let mut val = AttValue::new(4);
        val.update(&buf[..buf.len().min(508)], 4);
        self.outbound
            .send(val)
            .map_err(|_| Error::ClientThreadHungUp)
            .await
        /*
        self.outbound
            .send(val)
            */
    }
    pub fn get_avg_lat(&self) -> Duration {
        self.stats.get_avg_lat()
    }
    pub fn get_loss_rate(&self) -> f64 {
        self.stats.get_loss_rate()
    }
    pub fn get_sent(&self) -> u32 {
        self.stats.get_sent()
    }
    pub fn get_out_mtu(&self) -> u16 {
        self.stats.get_mtu() - 4
    }
    pub fn reset_recvd_sent(&self) {
        self.stats.reset();
    }
    pub async fn shutdown(self) -> Result<(), Error> {
        let val = AttValue::new(0);
        self.outbound
            .send(val)
            .await
            .map_err(|_| Error::ClientThreadHungUp)?;
        self.handle.await
    }
}
