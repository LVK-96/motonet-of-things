use defmt::{Debug2Format, info};
use esp_bootloader_esp_idf::{
    ota::OtaImageState,
    ota_updater::OtaUpdater,
    partitions::{AppPartitionSubType, Error, FlashRegion, PARTITION_TABLE_MAX_LEN},
};
use esp_storage::FlashStorage;

/// Minimal wrapper around ESP-IDF OTA boot metadata.
///
/// The OTA task and the post-reboot health confirmation task each create
/// their own instance from a flash storage handle owned by the caller
/// (currently the shared `app_bus::FLASH` mutex). The wrapper does not retain
/// the storage; the caller must keep it alive for the lifetime of the
/// `OtaBootMetadata`.
pub struct OtaBootMetadata<'a, 'd> {
    updater: OtaUpdater<'a, 'd>,
}

impl<'a, 'd> OtaBootMetadata<'a, 'd> {
    /// Create an OTA boot metadata accessor from flash storage.
    ///
    /// # Errors
    ///
    /// Returns an ESP bootloader partition/OTA error if the partition table or
    /// OTA data partition cannot be read/validated.
    pub fn new(
        flash: &'a mut FlashStorage<'d>,
        partition_table: &'a mut [u8; PARTITION_TABLE_MAX_LEN],
    ) -> Result<Self, Error> {
        Ok(Self {
            updater: OtaUpdater::new(flash, partition_table)?,
        })
    }

    /// Return the currently selected OTA image state.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the current state cannot be read.
    pub fn current_state(&mut self) -> Result<OtaImageState, Error> {
        self.updater.current_ota_state()
    }

    /// Return the currently selected OTA app partition.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the selected partition cannot be read.
    pub fn selected_partition(&mut self) -> Result<AppPartitionSubType, Error> {
        self.updater.selected_partition()
    }

    /// Log and return the currently selected OTA slot and image state.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the current slot/state cannot be read.
    pub fn log_current_slot_and_state(
        &mut self,
    ) -> Result<(AppPartitionSubType, OtaImageState), Error> {
        let slot = self.selected_partition()?;
        let state = self.current_state()?;
        info!(
            "OTA boot metadata: selected slot={:?} state={:?}",
            Debug2Format(&slot),
            Debug2Format(&state)
        );
        Ok((slot, state))
    }

    /// Log and return the currently selected OTA image state.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the current state cannot be read.
    pub fn log_current_state(&mut self) -> Result<OtaImageState, Error> {
        let (_, state) = self.log_current_slot_and_state()?;
        Ok(state)
    }

    /// Return whether the current app is pending bootloader confirmation.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the current state cannot be read.
    pub fn current_app_pending_confirmation(&mut self) -> Result<bool, Error> {
        Ok(matches!(
            self.current_state()?,
            OtaImageState::New | OtaImageState::PendingVerify
        ))
    }

    /// Return the inactive OTA app partition that would be activated next.
    ///
    /// The returned flash region is the safe target for a streamed OTA image write.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata/partition error if no inactive slot can be found.
    pub fn inactive_partition(
        &mut self,
    ) -> Result<(FlashRegion<'_, 'd>, AppPartitionSubType), Error> {
        let (region, slot) = self.updater.next_partition()?;
        info!(
            "OTA boot metadata: inactive slot selected for update={:?}",
            Debug2Format(&slot)
        );
        Ok((region, slot))
    }

    /// Activate the inactive OTA app partition selected by the boot metadata.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the next slot cannot be activated.
    pub fn activate_next_partition(&mut self) -> Result<(), Error> {
        info!("OTA boot metadata: activating next OTA partition");
        self.updater.activate_next_partition()
    }

    /// Mark the currently selected app as new in OTA metadata.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the state cannot be written.
    pub fn mark_current_app_new(&mut self) -> Result<(), Error> {
        info!("OTA boot metadata: marking current app new");
        self.updater.set_current_ota_state(OtaImageState::New)
    }

    /// Mark the current app valid in OTA metadata.
    ///
    /// # Errors
    ///
    /// Returns an OTA metadata error if the state cannot be written.
    pub fn mark_current_app_valid(&mut self) -> Result<(), Error> {
        info!("OTA boot metadata: marking current app valid");
        self.updater.set_current_ota_state(OtaImageState::Valid)
    }
}
