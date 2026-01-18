/// Display abstraction layer for UI rendering.
use core::fmt::Write;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::BinaryColor,
    prelude::*,
    text::{Alignment, Text},
};
use heapless::String;
use mini_oled::prelude::*;
use profont::{PROFONT_10_POINT, PROFONT_14_POINT, PROFONT_24_POINT};

/// Error type for display operations
#[derive(Debug, defmt::Format)]
pub enum DisplayError {
    /// I2C communication error
    I2c,
    /// Display initialization failed
    InitFailed,
    /// Drawing operation failed
    DrawFailed,
}

pub trait Display {
    /// Clear the display
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails buffer
    fn clear(&mut self) -> Result<(), DisplayError>;

    /// Show temperature, sensor ID, channel, battery status, and optional timestamp
    fn show_temperature(
        &mut self,
        temperature_c: f32,
        sensor_id: u8,
        channel: u8,
        battery_ok: bool,
        timestamp: Option<&str>,
    ) -> Result<(), DisplayError>;

    /// Flush the display buffer to the screen
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn flush(&mut self) -> Result<(), DisplayError>;

    /// Show a status message
    ///
    /// # Arguments
    ///
    /// * `message` - The message to display
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails on the display
    fn show_status(&mut self, message: &str) -> Result<(), DisplayError>;

    /// Show a dummy screen for testing
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails UI events
    fn show_dummy_screen(&mut self) -> Result<(), DisplayError>;
}

/// SH1106 OLED display driver implementation using mini-oled.
pub struct Sh1106Display<I2C: embedded_hal::i2c::I2c> {
    driver: Sh1106<I2cInterface<I2C>>,
}

impl<I2C, E> Sh1106Display<I2C>
where
    I2C: embedded_hal::i2c::I2c<Error = E>,
    E: core::fmt::Debug,
{
    pub fn new(i2c: I2C) -> Result<Self, DisplayError> {
        // The display module listens @0x3C
        let i2c_interface = I2cInterface::new(i2c, 0x3C);

        // Initialize display driver
        let mut driver = Sh1106::new(i2c_interface);

        driver.init().map_err(|_| DisplayError::InitFailed)?;
        let _ = driver.get_mut_canvas().clear(BinaryColor::Off);
        driver.flush().map_err(|_| DisplayError::I2c)?;

        Ok(Self { driver })
    }
}

impl<I2C, E> Display for Sh1106Display<I2C>
where
    I2C: embedded_hal::i2c::I2c<Error = E>,
    E: core::fmt::Debug,
{
    fn clear(&mut self) -> Result<(), DisplayError> {
        let _ = self.driver.get_mut_canvas().clear(BinaryColor::Off);
        Ok(())
    }

    fn show_temperature(
        &mut self,
        temperature_c: f32,
        sensor_id: u8,
        channel: u8,
        battery_ok: bool,
        timestamp: Option<&str>,
    ) -> Result<(), DisplayError> {
        self.clear()?;

        // Large temperature display in center
        let mut temp_str: String<16> = String::new();
        let _ = write!(temp_str, "{:.1}C", temperature_c);

        let style_large = MonoTextStyle::new(&PROFONT_24_POINT, BinaryColor::On);
        Text::with_alignment(
            temp_str.as_str(),
            Point::new(64, 32),
            style_large,
            Alignment::Center,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Sensor info at top left, timestamp at top right
        let style_small = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);

        let mut info_str: String<32> = String::new();
        let _ = write!(info_str, "S{} Ch{}", sensor_id, channel);
        Text::with_alignment(
            info_str.as_str(),
            Point::new(4, 10),
            style_small,
            Alignment::Left,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Timestamp at top right (if available)
        if let Some(ts) = timestamp {
            Text::with_alignment(ts, Point::new(124, 10), style_small, Alignment::Right)
                .draw(self.driver.get_mut_canvas())
                .map_err(|_| DisplayError::DrawFailed)?;
        }

        // Battery status at bottom
        let battery_str = if battery_ok { "BAT OK" } else { "BAT LOW!" };
        let style_status = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);
        Text::with_alignment(
            battery_str,
            Point::new(64, 58),
            style_status,
            Alignment::Center,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        self.flush()
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.driver.flush().map_err(|_| DisplayError::I2c)
    }

    fn show_status(&mut self, message: &str) -> Result<(), DisplayError> {
        self.clear()?;

        let style = MonoTextStyle::new(&PROFONT_14_POINT, BinaryColor::On);
        Text::with_alignment(message, Point::new(64, 32), style, Alignment::Center)
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;

        self.flush()
    }

    fn show_dummy_screen(&mut self) -> Result<(), DisplayError> {
        self.clear()?;

        let style = MonoTextStyle::new(&PROFONT_14_POINT, BinaryColor::On);
        Text::with_alignment("Dummy Screen", Point::new(64, 32), style, Alignment::Center)
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;

        self.flush()
    }
}

pub fn setup_sh1106_display<I2C>(i2c: I2C) -> Result<Sh1106Display<I2C>, DisplayError>
where
    I2C: embedded_hal::i2c::I2c,
    <I2C as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    Sh1106Display::new(i2c)
}
