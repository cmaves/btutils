use rustable::UUID;
use std::time::Duration;

mod client;
mod one_sender;
mod server;
use one_sender::{one_time_channel, OneSender};

#[derive(Debug)]
pub enum Error {
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

pub const TIME_SERV: UUID = UUID(0x02f9727572264595a4f9fb54738dd751);
const TIME_CHRC: UUID = UUID(0x33858edffe544e5c8f13ad8bafda30dc);

pub use client::{ClientOptions, TimeClient};
pub use server::TimeService;
/*
fn get_ci_att(dur: Duration) -> u16 {
    let micros = dur.as_micros() - 7250;
    let ci_val = micros / 1250;
    ci_val as u16
}

fn get_ci_dur(val: u16) -> Duration {
    Duration::from_micros(7250) + Duration::from_micros(1250) * val as u32
}

fn get_delay(insts: &[Instant], ci: u16) -> u32 {
    let ci_dur = get_ci_dur(ci);
    let mut total_local_delay = Duration::from_secs(0);
    let start = insts[0];
    for (i, inst) in insts.iter().enumerate().skip(1) {
        let elapsed = inst.duration_since(start);
        let ci_elapsed = ci_dur * i as u32 * 2;
        total_local_delay += elapsed - ci_elapsed;
    }
    let delay = total_local_delay / (insts.len() - 1) as u32;
    delay.as_micros() as u32
}*/

fn is_within_increment(first: Duration, second: Duration) -> bool {
    let diff = (first.as_micros() as i64 - second.as_micros() as i64).abs() as u64;
    diff < 1250
}
