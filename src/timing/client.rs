use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_std::channel::{bounded, Sender};
use async_std::future::timeout;
use async_std::task::{spawn, JoinHandle};
use futures::future::{try_join, Either};

use async_rustbus::RpcConn;

use gatt::client::{Characteristic, NotifySocket, Service, WriteSocket};
use gatt::AttValue;
use rustable::{gatt, Adapter, MAC};

use super::{is_within_increment, one_time_channel, Error, OneSender, TIME_CHRC, TIME_SERV};
use crate::drop_select;

pub struct TimeClient {
    handle: JoinHandle<Result<(), Error>>,
    start: Instant,
    sender: Sender<ClientMsg>,
}

enum State {
    Waiting,
    ClientInit(Instant),
    GettingCI {
        insts: Vec<Instant>,
        est_ci: Duration,
        last_change: u8,
    },
    Agreeing(u32, Instant),
    ClientSetTime(Instant), /*
                            GettingCI {
                                timeout: Duration,
                                sent: Instant,
                                client_time: u64
                            }*/
}

pub struct ClientOptions<'a> {
    pub hci: u8,
    pub dev: MAC,
    pub name: Option<&'a str>,
}
impl ClientOptions<'_> {
    pub fn new(dev: MAC) -> Self {
        Self {
            dev,
            hci: 0,
            name: None,
        }
    }
}

enum ClientMsg {
    Shutdown,
    StartSync(OneSender<bool>),
}
struct ClientData {
    notify: NotifySocket,
    write: WriteSocket,
    chrc: Characteristic,
    start: Instant,
    state: State,
    client_wait: Option<OneSender<bool>>,
}

impl ClientData {
    async fn write_data(&mut self, att_val: &AttValue) -> Result<(), Error> {
        eprintln!("\tClientData::write_data: {:?}", att_val);
        match self.write.write(att_val).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotConnected || e.kind() == ErrorKind::BrokenPipe => {
                let write_fut = self.chrc.write_value_wo_response(att_val, 0).await?;
                let sock_fut = self.chrc.acquire_write().await?;
                let res = try_join(write_fut, sock_fut).await?;
                self.write = res.1;
                Ok(())
            }
            Err(e) => Err(Error::Dbus(e)),
        }
    }
    async fn start_client_sync(&mut self) -> Result<Duration, Error> {
        eprintln!("ClientData::start_client_sync()");
        let now = Instant::now();
        self.write_data(&AttValue::from(&[0x00u8][..])).await?;
        self.state = State::ClientInit(now);
        Ok(Duration::from_secs(9))
    }
    async fn handle_to(&mut self) -> Result<Duration, Error> {
        eprintln!("ClientData::handle_to()");
        match &self.state {
            State::Waiting => Ok(Duration::from_secs(3600)),
            _ => self.start_client_sync().await,
        }
    }
    async fn handle_not(&mut self, res: std::io::Result<AttValue>) -> Result<Duration, Error> {
        eprintln!("ClientData::handle_not(): res: {:?}", res);
        let val = match res {
            Ok(v) => v,
            Err(_) => {
                self.notify = self.chrc.acquire_notify().await?.await?;
                return self.handle_to().await;
            }
        };
        if val.is_empty() {
            return Ok(self.get_to());
        }
        match (&self.state, val[0]) {
            (State::Waiting, 0xFF) => Ok(self.get_to()),
            (State::Waiting, _) => unimplemented!(),
            (State::ClientInit(start), 0x80) => {
                let start = *start;
                self.begin_ci_loop(start.elapsed()).await
            }
            (State::ClientInit(_), _) => Ok(self.get_to()),

            (State::GettingCI { .. }, 0x81) if val.len() >= 3 => {
                let mut idx = [0; 2];
                idx.copy_from_slice(&val[1..3]);
                let idx = u16::from_le_bytes(idx);
                self.do_ci_loop(idx).await
            }
            (State::Agreeing(att_ci, _), 0x82) => {
                let att_ci = *att_ci;
                self.client_set_time(att_ci).await
            }
            (State::ClientSetTime(_), 0x83) if val.len() >= 9 => {
                let now = Instant::now();
                let mut st_ref = [0; 8];
                st_ref.copy_from_slice(&val[1..9]);
                let st_ref = u64::from_le_bytes(st_ref);
                let ct_ref = get_time(now, self.start);
                let ref_diff = (ct_ref as i64 - st_ref as i64).abs() as u64;
                eprintln!("handle_not(): ref_diff: {} us", ref_diff);
                if ref_diff > 1250 {
                    self.start_client_sync().await
                } else {
                    self.state = State::Waiting;
                    if let Some(barrier) = self.client_wait.take() {
                        barrier.send(true).ok();
                    }
                    Ok(Duration::from_secs(3600))
                }
            }
            _ => self.start_client_sync().await,
        }
    }
    async fn begin_ci_loop(&mut self, est_ci: Duration) -> Result<Duration, Error> {
        let now = Instant::now();
        self.state = State::GettingCI {
            insts: vec![now],
            est_ci,
            last_change: 0,
        };
        let att_val = AttValue::from(&[0x01, 0x00, 0x00][..]);
        self.write_data(&att_val).await?;
        Ok(Duration::from_secs(4).mul_f32(2.25))
    }
    async fn do_ci_loop(&mut self, idx: u16) -> Result<Duration, Error> {
        let now = Instant::now();
        let (insts, est_ci, last_change) = match &mut self.state {
            State::GettingCI {
                insts,
                est_ci,
                last_change,
            } => (insts, est_ci, last_change),
            _ => unreachable!(),
        };
        if idx as usize != insts.len() - 1 {
            let est_ci = *est_ci;
            return self.begin_ci_loop(est_ci).await;
        }
        let last = insts.last().unwrap();

        // Check for timeout
        if let None = (*last + est_ci.mul_f32(2.25)).checked_duration_since(now) {
            return self.begin_ci_loop(Duration::from_secs(4)).await;
        }

        // Check if the estimated CI has decreased
        let start = insts[0];
        let new_est_ci = now.duration_since(start) / insts.len() as u32 / 2;
        eprintln!(
            "do_ci_loop(): iteration time {} us, new_est_ci: {}",
            now.duration_since(*last).as_micros(),
            new_est_ci.as_micros()
        );
        if is_within_increment(*est_ci, new_est_ci) {
            *last_change += 1;
        } else {
            *last_change = 0;
        }
        *est_ci = new_est_ci;
        let mut att_val = AttValue::new(0);
        let est_ci = *est_ci;
        if *last_change >= 3 {
            eprintln!("do_ci_loop(): CI stabilized");
            // Estimated CI has stabilized write, check for CI agreement
            att_val.push(0x02);
            //let att_ci = get_ci_att(est_ci);
            let att_ci = est_ci.as_micros() as u32;
            att_val.extend_from_slice(&att_ci.to_le_bytes());
            self.state = State::Agreeing(att_ci, now + est_ci.mul_f32(2.25));
            self.write_data(&att_val).await?;
        } else {
            let idx = (insts.len() as u16).to_le_bytes();
            insts.push(now);
            att_val.push(0x01);
            att_val.extend_from_slice(&idx);
            self.write_data(&att_val).await?;
        }
        Ok(est_ci.mul_f32(2.25))
    }
    async fn client_set_time(&mut self, att_ci: u32) -> Result<Duration, Error> {
        let now = Instant::now();
        let ct_ref = get_time(now, self.start) + att_ci as u64;
        let mut att_val = AttValue::new(0);
        att_val.push(0x03);
        att_val.extend_from_slice(&ct_ref.to_le_bytes());
        let end = now + Duration::from_micros(att_ci as u64).mul_f32(2.25);
        self.state = State::ClientSetTime(end);
        self.write_data(&att_val).await?;
        Ok(Duration::from_micros(att_ci as u64).mul_f64(2.25))
    }
    fn get_to(&self) -> Duration {
        match &self.state {
            State::Waiting => Duration::from_secs(3600),
            State::ClientInit(start) => {
                let now = Instant::now();
                (*start + Duration::from_secs(8)).saturating_duration_since(now)
            }
            State::GettingCI { insts, est_ci, .. } => {
                let target = *insts.last().unwrap() + est_ci.mul_f32(2.25);
                target.saturating_duration_since(Instant::now())
            }
            State::Agreeing(_, end) => {
                //let target = *start + Duration::from_millis(*est_ci as u64).mul_f32(2.25);
                end.saturating_duration_since(Instant::now())
            }
            State::ClientSetTime(inst) => inst.saturating_duration_since(Instant::now()),
        }
    }
}
fn get_time(inst: Instant, start: Instant) -> u64 {
    inst.duration_since(start).as_micros() as u64
}
impl TimeClient {
    pub async fn new(options: ClientOptions<'_>) -> Result<Self, Error> {
        let conn = RpcConn::system_conn(true).await?;
        Self::from_conn(Arc::new(conn), options).await
    }
    pub async fn from_conn(conn: Arc<RpcConn>, options: ClientOptions<'_>) -> Result<Self, Error> {
        let hci = Adapter::from_conn(conn, options.hci).await?;
        let dev = hci.get_device(options.dev).await?;
        let serv = dev.get_service(TIME_SERV).await?;
        Self::from_service(serv).await
    }
    pub async fn from_service(serv: Service) -> Result<Self, Error> {
        if serv.uuid() != TIME_SERV {
            return Err(Error::InvalidService);
        }
        let chrc = serv.get_characteristic(TIME_CHRC).await?;
        let (notify, write) = {
            let f = try_join(chrc.acquire_notify(), chrc.acquire_write()).await?;
            try_join(f.0, f.1).await?
        };
        let (sender, recv) = bounded(1);
        let start = Instant::now();
        let mut data = ClientData {
            notify,
            write,
            chrc,
            start,
            state: State::Waiting,
            client_wait: None,
        };
        let handle = spawn(async move {
            let mut to = data.get_to();
            loop {
                let notify = data.notify.recv_notification();
                let recv = recv.recv();
                let sel = drop_select(recv, notify);
                to = match timeout(to, sel).await {
                    Err(_) => data.handle_to().await?,
                    Ok(Either::Left(msg)) => {
                        let msg = msg.unwrap();
                        match msg {
                            ClientMsg::Shutdown => break,
                            ClientMsg::StartSync(b) => {
                                if let Some(barrier) = data.client_wait.replace(b) {
                                    barrier.send(false).ok();
                                }
                                if matches!(
                                    data.state,
                                    State::GettingCI { .. } | State::Agreeing { .. }
                                ) {
                                    data.get_to()
                                } else {
                                    data.start_client_sync().await?
                                }
                            }
                        }
                    }
                    Ok(Either::Right(notify)) => data.handle_not(notify).await?,
                }
            }
            Result::<_, Error>::Ok(())
        });
        Ok(Self {
            handle,
            start,
            sender,
        })
    }
    pub fn get_start(&self) -> Instant {
        self.start
    }
    pub fn get_time(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
    pub async fn shutdown(self) -> Result<(), Error> {
        self.sender.send(ClientMsg::Shutdown).await.unwrap();
        self.handle.await
    }
    pub async fn do_client_sync(&self) -> Result<bool, Error> {
        let (sender, recv) = one_time_channel();
        self.sender
            .send(ClientMsg::StartSync(sender))
            .await
            .unwrap();
        Ok(recv.recv().await.unwrap())
    }
}
