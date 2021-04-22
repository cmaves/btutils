use std::env::args;
use std::str::FromStr;
use std::time::{Duration, Instant};

use async_std::task::sleep;

use btlcp::timing::{Client, ClientOptions};
use btlcp::MAC;

#[async_std::main]
async fn main() {
    let mac = args().nth(1);
    let mac = MAC::from_str(&mac.unwrap()).unwrap();
    let mut client_options = ClientOptions::new(mac);
    client_options.name = Some("io.maves.example_client");
    let client = Client::new(client_options).await.unwrap();
    let mut target = Instant::now() + Duration::from_secs(10);
    loop {
        client.do_client_sync().await.unwrap();
        println!("Time: {}", client.get_time());
        target += Duration::from_secs(10);
        sleep(target.saturating_duration_since(Instant::now())).await
    }
}
