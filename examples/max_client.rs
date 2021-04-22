use std::env::args;
use std::str::FromStr;
use std::time::{Duration, Instant};

use async_std::stream::interval;
use futures::prelude::*;

use btlcp::client::{ChannelOptions, MsgChannelClient};
use btlcp::{MAC, UUID};
use rand::prelude::*;

const SERV: UUID = UUID(0xd0ba200e4241433294bb2f646531afa6);

#[async_std::main]
async fn main() {
    let mac = args().nth(1);
    let mac = MAC::from_str(&mac.unwrap()).unwrap();
    let mut channel_options = ChannelOptions::new(mac, SERV);
    channel_options.name = Some("io.maves.example_client");
    let client = MsgChannelClient::new(channel_options).await.unwrap();
    let mut interval = interval(Duration::from_millis(8));
    let mut rng = rand::thread_rng();
    let mut _recv_cnt: usize = 0;
    let mut send_cnt: usize = 0;
    let start = Instant::now();
    let mut last = start;
    println!("Starting send loop");
    let mut msg = [0; 196];
    for (i, u) in msg.iter_mut().enumerate() {
        *u = i as u8;
    }
    while let Some(_) = interval.next().await {
        //let msg: [u8; 8] = rng.gen();

        client.send_msg(&msg).await.unwrap();
        send_cnt += 1;
        while let Some(_) = client.try_recv_msg().unwrap() {
            _recv_cnt += 1;
        }
        if last.elapsed() > Duration::from_secs(10) {
            last += Duration::from_secs(10);
            let elapsed = start.elapsed();
            println!(
                "Time {}: avg_lat: {} ms, sent: {}, avg_tp: {} msg/sec, loss: {} %",
                elapsed.as_secs(),
                client.get_avg_lat().as_millis(),
                send_cnt,
                send_cnt as f64 / elapsed.as_secs_f64(),
                client.get_loss_rate() * 100.0,
            );
            client.reset_recvd_sent();
        }
    }
}
