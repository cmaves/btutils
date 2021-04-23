pub use rustable::{MAC, UUID};
use std::collections::VecDeque;
use std::fmt::Display;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const SERV_UUID: UUID = UUID(0x0143d59000004dfbb83fb1135254e6b4);
const SERV_OUT: UUID = UUID(0x0143d59000014dfbb83fb1135254e6b4);
const SERV_IN: UUID = UUID(0x0143d59000024dfbb83fb1135254e6b4);
//const SEND_TIMEOUT: Duration = Duration::from_millis(500);
const DEFAULT_LAT_PERIOD: usize = 32;

mod client;
mod rendezvous;
mod server;

use rendezvous::*;

pub use client::{ClientOptions, MsgChannelClient};
pub use server::{MsgChannelServ, ServerOptions};

#[derive(Debug)]
pub enum Error {
    InThreadHungUp,
    OutThreadHungUp,
    ClientThreadHungUp,
    DeviceNotFound,
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

fn ioe_to_blee(err: std::io::Error) -> Error {
    rustable::Error::from(err).into()
}

#[derive(Default)]
struct Stats {
    avg_lat: AtomicU16,
    recv_send: AtomicU64,
    mtu: AtomicU16,
}
impl Stats {
    fn update_lat(&self, lat: Duration) {
        let millis = lat.as_millis() as u16;
        self.avg_lat.store(millis, Ordering::Relaxed);
    }
    fn get_avg_lat(&self) -> Duration {
        let millis = self.avg_lat.load(Ordering::Relaxed);
        Duration::from_millis(millis as u64)
    }
    fn get_sent(&self) -> u32 {
        self.recv_send.load(Ordering::Relaxed) as u32
    }
    fn get_loss_rate(&self) -> f64 {
        let (recvd, sent) = self.get_recvd_sent();
        1.0 - (recvd as f64 / sent as f64)
    }
    fn get_recvd_sent(&self) -> (u32, u32) {
        let val = self.recv_send.load(Ordering::Relaxed);
        let recvd = (val >> 32) as u32;
        let sent = val as u32;
        (recvd, sent)
    }
    fn get_mtu(&self) -> u16 {
        self.mtu.load(Ordering::Relaxed)
    }
    fn set_mtu(&self, mtu: u16) {
        self.mtu.store(mtu, Ordering::Relaxed)
    }
    /*fn update_lost(&self, lost: u32) {
        self.recv_send.fetch_add(lost as u64, Ordering::Relaxed);
    }*/
    fn update_recvd_sent(&self, recvd: u32, sent: u32) {
        let to_add = ((recvd as u64) << 32) + sent as u64;
        self.recv_send.fetch_add(to_add, Ordering::Relaxed);
    }
    fn reset(&self) {
        self.recv_send.store(0, Ordering::Relaxed);
    }
}

struct LatDeque {
    last_inst: Instant,
    lat_sum: Duration,
    last: VecDeque<Duration>,
    lat_sum_adjusted: Duration,
    last_adjusted: VecDeque<Duration>,
    lat_period: usize,
}
impl LatDeque {
    fn new(lat_period: usize) -> Self {
        assert_ne!(lat_period, 0);
        LatDeque {
            last_inst: Instant::now(),
            last: VecDeque::with_capacity(lat_period),
            last_adjusted: VecDeque::with_capacity(lat_period),
            lat_period,
            lat_sum: Default::default(),
            lat_sum_adjusted: Default::default(),
        }
    }
    fn avg_lat_adjusted(&self) -> Duration {
        self.lat_sum_adjusted
            .checked_div(self.last_adjusted.len() as u32)
            .unwrap_or_default()
            / 2
    }
    fn avg_lat(&self) -> Duration {
        self.lat_sum
            .checked_div(self.last.len() as u32)
            .unwrap_or_default()
            / 2
    }
    fn pop_oldest(&mut self) {
        if self.last.is_empty() {
            return;
        }
        debug_assert_eq!(self.last.len(), self.last_adjusted.len());
        self.lat_sum -= self.last.pop_front().unwrap();
        self.lat_sum_adjusted -= self.last_adjusted.pop_front().unwrap();
    }
    fn push_new(&mut self, inst: Instant) {
        let now = Instant::now();
        let elapsed = now.duration_since(inst);
        self.lat_sum += elapsed;
        let adj_elapsed = if inst < self.last_inst {
            now.duration_since(self.last_inst)
        } else {
            elapsed
        };
        self.lat_sum_adjusted += adj_elapsed;
        while self.last.len() + 1 > self.lat_period {
            self.pop_oldest();
        }
        self.last.push_back(elapsed);
        self.last_adjusted.push_back(adj_elapsed);
        self.last_inst = now;
    }
}
