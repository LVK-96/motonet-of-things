use core::future::pending;

use embassy_executor::Spawner;

mod hw_context;
mod hardware;
mod spawn;

pub async fn run(spawner: Spawner) -> ! {
    let hw_context = hw_context::hw_setup(&spawner).await;
    spawn::spawn_tasks(&spawner, hw_context);

    loop {
        pending::<()>().await;
    }
}
