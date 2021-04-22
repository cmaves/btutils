use async_std::io::stdin;
use futures::future::Either;

use rustable::gatt;
use rustable::Adapter;
use gatt::server::Application;
use btutils::messaging::{ServerOptions, MsgChannelServ};
use btutils::drop_select;


#[async_std::main]
async fn main() {
	let hci = Adapter::new(0).await.unwrap();
	let mut app = Application::new(&hci, "/io/btlcp/example_server");
    let server = MsgChannelServ::new(&mut app, &ServerOptions::new()).await.unwrap();
    let mut buf = String::new();
    let stdin = stdin();
    loop {
        let inp = stdin.read_line(&mut buf);
        let recv = server.recv_msg();
        match drop_select(inp, recv).await {
            Either::Left(res) => {
                res.unwrap();
                eprintln!("Sending: {}", buf);
                server.send_msg(buf.as_bytes()).await.unwrap();
                eprintln!("Sent");
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
