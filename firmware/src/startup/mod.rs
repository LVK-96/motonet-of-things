use core::future::pending;
use embassy_executor::Spawner;

#[cfg(not(feature = "ota-rollback-test"))]
mod hardware;
#[cfg(not(feature = "ota-rollback-test"))]
mod hw_context;
#[cfg(not(feature = "ota-rollback-test"))]
mod sha_self_test;
#[cfg(not(feature = "ota-rollback-test"))]
mod spawn;

#[cfg(not(feature = "ota-rollback-test"))]
pub async fn run(spawner: Spawner) -> ! {
    let hw_context = hw_context::hw_setup(&spawner).await;
    spawn::spawn_tasks(&spawner, hw_context);

    loop {
        pending::<()>().await;
    }
}
