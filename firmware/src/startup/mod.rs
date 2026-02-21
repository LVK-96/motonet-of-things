use core::future::pending;

use embassy_executor::Spawner;

mod composition;
mod hardware;
mod spawn;

pub async fn run(spawner: Spawner) -> ! {
    let startup_context = composition::compose(&spawner).await;
    spawn::spawn_tasks(&spawner, startup_context);

    loop {
        pending::<()>().await;
    }
}
