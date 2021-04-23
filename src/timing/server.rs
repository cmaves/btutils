use std::sync::Arc;
use std::time::{Duration, Instant};

use async_std::sync::{Mutex, MutexGuard};

use rustable::gatt;

use gatt::server::{Application, Characteristic, Service};
use gatt::{AttValue, CharFlags, ValOrFn};

use super::{TIME_CHRC, TIME_SERV};

enum State {
    Waiting,
    BeginReq,
    ClientInit {
        insts: Vec<Instant>,
        est_ci: Duration,
    },
    Agreeing(u32),
    Finishing(u64),
}
/*enum ServerMsg {
    Shutdown,
    StartSync(OneSender<bool>)
}*/

fn calc_ref_time(time: u64, start: Instant) -> u64 {
    start.elapsed().as_micros() as u64 + time
}

#[derive(Clone)]
struct RefClock(Arc<Mutex<(u64, Instant)>>);

impl RefClock {
    fn new() -> Self {
        RefClock(Arc::new(Mutex::new((0, Instant::now()))))
    }
    fn update_ref_time(&self, time: u64, start: Instant) {
        let mut ref_time = self.acquire_lock();
        ref_time.0 = time;
        ref_time.1 = start;
    }
    fn get_time(&self) -> u64 {
        let ref_time = self.acquire_lock();
        calc_ref_time(ref_time.0, ref_time.1)
    }
    fn acquire_lock(&self) -> MutexGuard<(u64, Instant)> {
        let mut spin = 0;
        loop {
            if let Some(ref_time) = self.0.try_lock() {
                return ref_time;
            }
            for _ in 0..(1 << spin) {
                std::hint::spin_loop();
            }
            spin = 6.min(spin + 1);
        }
    }
}
struct ServerData {
    state: State,
    ref_clock: RefClock,
}
impl ServerData {
    fn handle_write(&mut self, att_val: &AttValue) {
        eprintln!("ServerWrite(): {:?}", att_val);
        match (&mut self.state, att_val[0]) {
            (State::Waiting, 0x00) => {
                self.state = State::BeginReq;
            }
            (State::BeginReq, 0x00) => {} // Do nothing
            (State::BeginReq, 0x01) => {
                self.state = State::ClientInit {
                    insts: vec![Instant::now()],
                    est_ci: Duration::from_secs(4),
                };
            }
            (State::ClientInit { insts, est_ci }, 0x01) => {
                eprintln!("ServerWrite(): doing_ci_iter");
                if att_val.len() >= 3 {
                    let now = Instant::now();
                    let mut idx = [0; 2];
                    idx.copy_from_slice(&att_val[1..3]);
                    let idx = u16::from_le_bytes(idx) as usize;
                    if idx == insts.len() && idx != 0 {
                        let start = insts[0];
                        let last = insts.last().unwrap();
                        // Check for timeout
                        match (*last + est_ci.mul_f32(2.25)).checked_duration_since(now) {
                            None => {
                                insts.clear();
                                insts.push(now);
                            }
                            Some(_) => {
                                let new_est = now.duration_since(start) / insts.len() as u32 / 2;
                                *est_ci = new_est;
                                insts.push(now);
                            }
                        }
                    } else {
                        insts.clear();
                        insts.push(now);
                    }
                }
            }
            (State::ClientInit { insts, est_ci }, 0x02) => {
                eprintln!("ServerWrite(): getting estimated ci");
                if att_val.len() >= 5 {
                    // Exchange and compare estimated CI
                    let mut c_est_ci = [0; 4];
                    c_est_ci.copy_from_slice(&att_val[1..5]);
                    let c_est_ci = u32::from_le_bytes(c_est_ci);
                    let s_est_ci = est_ci.as_micros() as u32;
                    if (c_est_ci as i32 - s_est_ci as i32).abs() < 1250 {
                        self.state = State::Agreeing(c_est_ci);
                    } else {
                        insts.clear();
                        insts.push(Instant::now());
                    }
                }
            }
            (State::Agreeing(est_ci), 0x03) => {
                eprintln!("ServerWrite(): getting ref time");
                let now = Instant::now();
                let mut ref_time = [0; 8];
                ref_time.copy_from_slice(&att_val[1..9]);
                let ref_time = u64::from_le_bytes(ref_time);
                let est_ci = *est_ci;
                self.ref_clock.update_ref_time(ref_time, now);
                eprintln!("Ref_time: {}", self.ref_clock.get_time() + est_ci as u64);
                let ref_time = ref_time + est_ci as u64;
                self.state = State::Finishing(ref_time);
            }
            (State::Finishing(_), _) => unreachable!(),
            _ => self.state = State::Waiting,
        }
    }
    fn value_cb(&mut self) -> AttValue {
        let ret = match &self.state {
            State::Waiting => AttValue::from(&[0xFF][..]),
            State::BeginReq => AttValue::from(&[0x80][..]),
            State::ClientInit { insts, .. } => {
                let len = (insts.len() - 1).to_le_bytes();
                AttValue::from(&[0x81, len[0], len[1]][..])
            }
            State::Agreeing(_) => AttValue::from(&[0x82u8][..]),
            State::Finishing(ref_time) => {
                let mut att_val = AttValue::new(0);
                att_val.push(0x83);
                att_val.extend_from_slice(&ref_time.to_le_bytes());
                self.state = State::Waiting;
                att_val
            }
        };
        eprintln!("\tvalue_cb(): {:?}", ret);
        ret
    }
}
pub struct TimeService {
    ref_clock: RefClock,
}
/*
impl<'a> ServerOptions<'a> {
    pub fn new(path: &'a str) -> Self {
        Self {
            service,
            path,
            hci: 0,
            name: None,
            filter: true,
        }
    }
}
pub struct ServerOptions<'a> {
    pub name: Option<&'a str>,
    pub hci: u8,
    pub path: &'a str,
    pub filter: bool,
}*/
impl TimeService {
    pub fn new(app: &mut Application) -> Self {
        let mut service = Service::new(TIME_SERV, true);
        let flags = CharFlags {
            write_wo_response: true,
            notify: true,
            ..Default::default()
        };

        let mut chrc = Characteristic::new(TIME_CHRC, flags);
        let ref_clock = RefClock::new();
        let data = Arc::new(Mutex::new(ServerData {
            state: State::Waiting,
            ref_clock: ref_clock.clone(),
        }));
        let d_clone = data.clone();
        chrc.set_write_cb(move |val| {
            if val.is_empty() {
                (None, false)
            } else {
                let mut data = d_clone.try_lock().unwrap();
                data.handle_write(&val);
                (None, true)
            }
        });
        chrc.set_value(ValOrFn::Function(Box::new(move || {
            let mut data = data.try_lock().unwrap();
            data.value_cb()
        })));
        service.add_char(chrc);
        app.add_service(service);
        TimeService { ref_clock }
    }
    pub fn get_time(&self) -> u64 {
        self.ref_clock.get_time()
    }
    /*pub async fn shutdown(self) -> Result<(), Error> {
        let app = self.worker.unregister().await?;
        if let Some(name) = self.name {
            let conn = app.get_conn();
            let rel_name = release_name(&name);
            conn.send_msg_with_reply(&rel_name).await?.await?;
        }
        Ok(())
    }*/
}
