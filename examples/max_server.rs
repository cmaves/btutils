use std::time::{Duration, Instant};

use async_std::stream::interval;
use futures::prelude::*;

use rand::prelude::*;

use btlcp::server::{ChannelOptions, MsgChannelServ};
use btlcp::UUID;

const SERV: UUID = UUID(0xd0ba200e4241433294bb2f646531afa6);

#[async_std::main]
async fn main() {
    let mut channel_options = ChannelOptions::new(SERV, "/io/btlcp/example_server");
    channel_options.name = Some("io.maves.example_server");
    channel_options.filter = false;
    let server = MsgChannelServ::new(&channel_options).await.unwrap();
    let mut interval = interval(Duration::from_millis(8));
    let mut rng = rand::thread_rng();
    let mut recv_cnt: usize = 0;
    let mut send_cnt: usize = 0;
    let start = Instant::now();
    let mut last = start;
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
