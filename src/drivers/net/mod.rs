#[cfg(all(target_arch = "riscv64", feature = "gem-net"))]
pub mod gem;
#[cfg(feature = "rtl8139")]
pub mod rtl8139;
#[cfg(not(feature = "rtl8139"))]
pub mod virtio;

#[allow(unused_imports)]
use crate::arch::kernel::core_local::*;
use crate::drivers::Driver;

/// A trait for accessing the network interface
pub(crate) trait NetworkDriver: Driver + smoltcp::phy::Device {
	/// Returns the mac address of the device.
	fn get_mac_address(&self) -> [u8; 6];
	/// Enable / disable the polling mode of the network interface
	fn set_polling_mode(&mut self, value: bool);
	/// Handle interrupt and check if a packet is available
	fn handle_interrupt(&mut self);
}
