/// Display abstraction layer for UI rendering.
use core::fmt::Write;
use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use heapless::String;
use karu_menu::{RenderItem, ScrollIndicator};
use mini_oled::prelude::*;
use profont::{PROFONT_10_POINT, PROFONT_14_POINT, PROFONT_24_POINT};

use crate::messages::SignalQuality;

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
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
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

    /// Show radio information screen
    ///
    /// # Arguments
    ///
    /// * `rssi` - Last received signal strength in dBm (or None if no signal yet)
    /// * `detection_threshold` - Configured detection threshold in dB
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn show_radio_info(
        &mut self,
        rssi: Option<i16>,
        detection_threshold: u8,
    ) -> Result<(), DisplayError>;

    /// Draw a section header.
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn draw_header(&mut self, title: &str) -> Result<(), DisplayError>;

    /// Draw a single menu item.
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn draw_menu_item(&mut self, item: &RenderItem<'_>) -> Result<(), DisplayError>;

    /// Draw menu overflow indicator.
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn draw_scroll_indicator(&mut self, indicator: ScrollIndicator) -> Result<(), DisplayError>;
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
    /// Initialize a new SH1106 display
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the I2C operation fails
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
        let _ = write!(temp_str, "{temperature_c:.1}C");

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
        let _ = write!(info_str, "S{sensor_id} Ch{channel}");
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

    fn show_radio_info(
        &mut self,
        rssi: Option<i16>,
        _detection_threshold: u8,
    ) -> Result<(), DisplayError> {
        self.clear()?;

        let style_header = MonoTextStyle::new(&PROFONT_14_POINT, BinaryColor::On);
        let style_normal = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);

        // Header
        Text::with_alignment(
            "RADIO STATUS",
            Point::new(64, 12),
            style_header,
            Alignment::Center,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Signal strength and quality
        if let Some(rssi_val) = rssi {
            let quality = SignalQuality::from_rssi(rssi_val);

            // RSSI value (shortened to fit screen)
            let mut rssi_str: String<20> = String::new();
            let _ = write!(rssi_str, "RSSI: {rssi_val}dBm");
            Text::with_alignment(
                rssi_str.as_str(),
                Point::new(64, 30),
                style_normal,
                Alignment::Center,
            )
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;

            // Quality label
            let mut quality_str: String<24> = String::new();
            let _ = write!(quality_str, "Quality: {}", quality.as_str());
            Text::with_alignment(
                quality_str.as_str(),
                Point::new(64, 44),
                style_normal,
                Alignment::Center,
            )
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;
        } else {
            Text::with_alignment(
                "No signal yet",
                Point::new(64, 37),
                style_normal,
                Alignment::Center,
            )
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;
        }

        // Hamburger menu icon (settings indicator) - bottom center
        // Draw 3 horizontal lines
        let menu_center_x = 64i32;
        let menu_top_y = 52i32;
        let line_width = 14u32;
        let line_height = 2u32;
        let line_spacing = 4i32;
        let style_filled = PrimitiveStyle::with_fill(BinaryColor::On);

        // Safe: line_width/2 = 7, fits in i32
        #[allow(clippy::cast_possible_wrap)]
        let half_width = (line_width / 2) as i32;
        let menu_x = menu_center_x - half_width;

        // Top line
        Rectangle::new(
            Point::new(menu_x, menu_top_y),
            Size::new(line_width, line_height),
        )
        .into_styled(style_filled)
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Middle line
        Rectangle::new(
            Point::new(menu_x, menu_top_y + line_spacing),
            Size::new(line_width, line_height),
        )
        .into_styled(style_filled)
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Bottom line
        Rectangle::new(
            Point::new(menu_x, menu_top_y + line_spacing * 2),
            Size::new(line_width, line_height),
        )
        .into_styled(style_filled)
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        self.flush()
    }

    fn draw_header(&mut self, title: &str) -> Result<(), DisplayError> {
        let style_header = MonoTextStyle::new(&PROFONT_14_POINT, BinaryColor::On);
        Text::with_alignment(title, Point::new(64, 12), style_header, Alignment::Center)
            .draw(self.driver.get_mut_canvas())
            .map(|_| ())
            .map_err(|_| DisplayError::DrawFailed)
    }

    fn draw_menu_item(&mut self, item: &RenderItem<'_>) -> Result<(), DisplayError> {
        let style_normal = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);
        let style_filled = PrimitiveStyle::with_fill(BinaryColor::On);

        Text::with_alignment(
            item.label,
            Point::new(8, item.y),
            style_normal,
            Alignment::Left,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        if item.is_selected && !item.is_editing {
            let label_chars =
                i32::try_from(item.label.len()).map_or(i32::MAX / 6, core::convert::identity);
            let label_width = u32::try_from(label_chars.saturating_mul(6))
                .map_or(u32::MAX, core::convert::identity);
            Rectangle::new(Point::new(8, item.y + 2), Size::new(label_width, 1))
                .into_styled(style_filled)
                .draw(self.driver.get_mut_canvas())
                .map_err(|_| DisplayError::DrawFailed)?;
        }

        if !item.value.is_empty() {
            let value_x = 120;
            Text::with_alignment(
                item.value.as_str(),
                Point::new(value_x, item.y),
                style_normal,
                Alignment::Right,
            )
            .draw(self.driver.get_mut_canvas())
            .map_err(|_| DisplayError::DrawFailed)?;

            if item.is_editing {
                let value_chars =
                    i32::try_from(item.value.len()).map_or(i32::MAX / 6, core::convert::identity);
                let value_width = u32::try_from(value_chars.saturating_mul(6))
                    .map_or(u32::MAX, core::convert::identity);
                let value_width_i32 =
                    i32::try_from(value_width).map_or(i32::MAX, core::convert::identity);
                Rectangle::new(
                    Point::new(value_x - value_width_i32, item.y + 2),
                    Size::new(value_width, 1),
                )
                .into_styled(style_filled)
                .draw(self.driver.get_mut_canvas())
                .map_err(|_| DisplayError::DrawFailed)?;
            }
        }

        Ok(())
    }

    fn draw_scroll_indicator(&mut self, indicator: ScrollIndicator) -> Result<(), DisplayError> {
        let style = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);
        let symbol = match indicator {
            ScrollIndicator::UpArrow => "^",
            ScrollIndicator::DownArrow => "v",
        };
        Text::with_alignment(symbol, Point::new(124, 60), style, Alignment::Right)
            .draw(self.driver.get_mut_canvas())
            .map(|_| ())
            .map_err(|_| DisplayError::DrawFailed)
    }
}

/// Setup SH1106 display
///
/// # Errors
///
/// Returns `DisplayError` if initialization fails
pub fn setup_sh1106_display<I2C>(i2c: I2C) -> Result<Sh1106Display<I2C>, DisplayError>
where
    I2C: embedded_hal::i2c::I2c,
    <I2C as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
{
    Sh1106Display::new(i2c)
}
