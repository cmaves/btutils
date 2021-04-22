use std::time::{Duration, Instant};

use async_std::task::sleep;

use rustable::Adapter;
use rustable::gatt::server;
use server::Application;

use btlcp::timing::TimeService;

#[async_std::main]
async fn main() {
    let hci = Adapter::new(0).await.unwrap();
    let mut app = Application::new(&hci, "/io/btlcp/example_server");
    let conn = app.get_conn();
    conn.request_name("io.maves.example_server").await.ok();
    app.set_filter(false);

    let service = TimeService::new(&mut app).await.unwrap();
    let _worker = app.register().await.unwrap();

    let start = Instant::now();
    let mut target = start + Duration::from_secs(15);
    loop {
        sleep(target.saturating_duration_since(Instant::now())).await;
        let ref_time = service.get_time();
        let elapsed = start.elapsed().as_micros();
        let diff = ref_time as i128 - elapsed as i128;
        println!(
            "Time since start: {} us, ref_time: {} us, diff: {}, us",
            elapsed, ref_time, diff
        );
        target += Duration::from_secs(10);
    }
}
