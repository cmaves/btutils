use std::time::{Duration, Instant};

use async_std::stream::interval;
use futures::prelude::*;

use rand::prelude::*;

use gatt::server::Application;
use rustable::gatt;
use rustable::Adapter;

use btutils::messaging::{MsgChannelServ, ServerOptions};

#[async_std::main]
async fn main() {
    let hci = Adapter::new(0).await.unwrap();
    let mut app = Application::new(&hci, "/io/btlcp/example_server");
    let server = MsgChannelServ::new(&mut app, &ServerOptions::new());
    // named _worker so it lives till the end of the function
    let _worker = app.register().await.unwrap();

    let mut interval = interval(Duration::from_millis(8));
    let mut rng = rand::thread_rng();
    let mut recv_cnt: usize = 0;
    let mut send_cnt: usize = 0;
    let start = Instant::now();
    let mut last = start;
    println!("Beginning main loop");
    while let Some(_) = interval.next().await {
        let msg: [u8; 4] = rng.gen();
        // server.send_msg(&msg).await.unwrap();
        send_cnt += 1;
        while let Some(_) = server.try_recv_msg().unwrap() {
            recv_cnt += 1;
        }
        if last.elapsed() > Duration::from_secs(10) {
            last += Duration::from_secs(10);
            let elapsed = start.elapsed();
            println!(
                "Time {}: avg_lat: {} ms, sent: {}, avg_tp: {} msg/sec",
                elapsed.as_secs(),
                server.get_avg_lat().as_millis(),
                send_cnt,
                send_cnt as f64 / elapsed.as_secs_f64()
            );
        }
    }
}
