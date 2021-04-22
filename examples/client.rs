use std::env::args;
use std::str::FromStr;

use async_std::io::stdin;
use futures::future::Either;

use btutils::messaging::{ClientOptions, MsgChannelClient};
use btutils::{drop_select, MAC, UUID};

const SERV: UUID = UUID(0xd0ba200e4241433294bb2f646531afa6);

#[async_std::main]
async fn main() {
    let mac = args().nth(1);
    let mac = MAC::from_str(&mac.unwrap()).unwrap();
    let mut channel_options = ClientOptions::new(mac, SERV);
    channel_options.name = Some("io.maves.example_client");
    let client = MsgChannelClient::new(channel_options).await.unwrap();
    let stdin = stdin();
    let mut buf = String::new();
    loop {
        let inp = stdin.read_line(&mut buf);
        let recv = client.recv_msg();
        match drop_select(inp, recv).await {
            Either::Left(res) => {
                res.unwrap();
                client.send_msg(buf.as_bytes()).await.unwrap();
                buf.clear();
            }
            Either::Right(msg) => {
                let msg = msg.unwrap();
                let s = String::from_utf8_lossy(&msg);
                println!("Line received: {}", s);
            }
        }
    }
}
