#![no_std]
#![no_main]

extern crate alloc;

use defmt::error;
use embassy_executor::Spawner;

use esp_println as _;

use esp32_rust_project::startup;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    error!("PANIC: {}", defmt::Debug2Format(info));
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    startup::run(spawner).await
}
