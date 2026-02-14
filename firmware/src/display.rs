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
use mini_oled::prelude::*;
use profont::{PROFONT_10_POINT, PROFONT_14_POINT, PROFONT_24_POINT};

use crate::messages::{channel_bandwidth_hz, SignalQuality};

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

    /// Show settings menu
    ///
    /// # Arguments
    ///
    /// * `nav_index` - Index of currently navigated item
    /// * `editing` - Whether we are editing the currently selected item
    /// * `detection_threshold` - Current detection threshold value in dB
    /// * `magn_target` - Current magnitude target level (0-7)
    /// * `channel_bandwidth_index` - Current channel bandwidth option index (0-3)
    /// * `carrier_sense` - Current carrier sense threshold (0-7)
    ///
    /// # Errors
    ///
    /// Returns `DisplayError` if the operation fails
    fn show_settings_menu(
        &mut self,
        nav_index: u8,
        editing: bool,
        detection_threshold: u8,
        magn_target: u8,
        channel_bandwidth_index: u8,
        carrier_sense: u8,
    ) -> Result<(), DisplayError>;
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

    fn show_settings_menu(
        &mut self,
        nav_index: u8,
        editing: bool,
        detection_threshold: u8,
        magn_target: u8,
        channel_bandwidth_index: u8,
        carrier_sense: u8,
    ) -> Result<(), DisplayError> {
        self.clear()?;

        let style_header = MonoTextStyle::new(&PROFONT_14_POINT, BinaryColor::On);
        let style_normal = MonoTextStyle::new(&PROFONT_10_POINT, BinaryColor::On);
        let style_filled = PrimitiveStyle::with_fill(BinaryColor::On);

        // Header
        Text::with_alignment(
            "SETTINGS",
            Point::new(64, 12),
            style_header,
            Alignment::Center,
        )
        .draw(self.driver.get_mut_canvas())
        .map_err(|_| DisplayError::DrawFailed)?;

        // Convert magn_target (0-7) to dB value per CC1101 datasheet
        let magn_target_db: u8 = match magn_target {
            0 => 24,
            1 => 27,
            2 => 30,
            3 => 33,
            4 => 36,
            5 => 38,
            6 => 40,
            _ => 42, // 7 or higher
        };
        let bandwidth_khz = channel_bandwidth_hz(channel_bandwidth_index) / 1000;

        // Menu items: Threshold, Magn Tgt, Bandwidth, Carrier Sense, Save
        // Store (label, Option<(value, unit)>)
        let items: [(&str, Option<(u32, &str)>); 5] = [
            ("Threshold", Some((u32::from(detection_threshold), "dB"))),
            ("Magn Tgt", Some((u32::from(magn_target_db), "dB"))),
            ("Bandwidth", Some((bandwidth_khz, "kHz"))),
            ("CS Thrsh", Some((u32::from(carrier_sense), "dB"))),
            ("Save", None),
        ];

        for (i, (label, value_unit)) in items.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let y = 24 + (i as i32 * 10);
            let is_nav = i == nav_index as usize;
            let is_editing = is_nav && editing && value_unit.is_some();

            // Draw label
            Text::with_alignment(label, Point::new(8, y), style_normal, Alignment::Left)
                .draw(self.driver.get_mut_canvas())
                .map_err(|_| DisplayError::DrawFailed)?;

            // Draw underline under label if navigating to this item (not editing)
            if is_nav && !editing {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let label_width = (label.len() as i32) * 6; // Approximate char width for PROFONT_10
                #[allow(clippy::cast_sign_loss)]
                Rectangle::new(Point::new(8, y + 2), Size::new(label_width as u32, 1))
                    .into_styled(style_filled)
                    .draw(self.driver.get_mut_canvas())
                    .map_err(|_| DisplayError::DrawFailed)?;
            }

            // Draw value with unit if present
            if let Some((val, unit)) = value_unit {
                let mut value_str: String<12> = String::new();
                let _ = write!(value_str, "{val}{unit}");
                let value_x = 120;
                Text::with_alignment(
                    value_str.as_str(),
                    Point::new(value_x, y),
                    style_normal,
                    Alignment::Right,
                )
                .draw(self.driver.get_mut_canvas())
                .map_err(|_| DisplayError::DrawFailed)?;

                // Draw underline under value if editing this item
                if is_editing {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                    let val_width = (value_str.len() as i32) * 6;
                    #[allow(clippy::cast_sign_loss)]
                    Rectangle::new(
                        Point::new(value_x - val_width, y + 2),
                        Size::new(val_width as u32, 1),
                    )
                    .into_styled(style_filled)
                    .draw(self.driver.get_mut_canvas())
                    .map_err(|_| DisplayError::DrawFailed)?;
                }
            }
        }

        self.flush()
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
