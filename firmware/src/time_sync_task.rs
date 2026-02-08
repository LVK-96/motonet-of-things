#[embassy_executor::task]
pub async fn time_sync_task(stack: embassy_net::Stack<'static>) {
    crate::time_sync::time_sync_loop(stack).await;
}
