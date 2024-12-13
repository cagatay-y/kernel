use alloc::boxed::Box;
#[cfg(not(feature = "dhcpv4"))]
use core::str::FromStr;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, Medium};
#[cfg(feature = "dhcpv4")]
use smoltcp::socket::dhcpv4;
#[cfg(all(feature = "dns", not(feature = "dhcpv4")))]
use smoltcp::socket::dns;
#[cfg(not(feature = "dhcpv4"))]
use smoltcp::wire::Ipv4Address;
use smoltcp::wire::{EthernetAddress, HardwareAddress};
#[cfg(not(feature = "dhcpv4"))]
use smoltcp::wire::{IpAddress, IpCidr};

use super::network::{NetworkInterface, NetworkState};
use crate::arch;
#[cfg(not(feature = "pci"))]
use crate::arch::kernel::mmio as hardware;
use crate::drivers::net::NetworkDriver;
#[cfg(feature = "pci")]
use crate::drivers::pci as hardware;

impl<'a> NetworkInterface<'a> {
	#[cfg(feature = "dhcpv4")]
	pub(crate) fn create() -> NetworkState<'a> {
		use core::ops::DerefMut;

		use smoltcp::phy::DeviceCapabilities;

		let (device, mac, medium) = if let Some(device) = hardware::get_network_driver() {
			let guard = device.lock();
			let mac = guard.get_mac_address();
			let DeviceCapabilities {
				max_transmission_unit: mtu,
				checksum: checksums,
				medium,
				..
			} = guard.capabilities();
			info!("{:?}", checksums);
			info!("MTU: {} bytes", mtu);
			(device, mac, medium)
		} else {
			return NetworkState::InitializationFailed;
		};

		if hermit_var!("HERMIT_IP").is_some() {
			warn!("A static IP address is specified with the environment variable HERMIT_IP, but the device is configured to use DHCPv4!");
		}

		let ethernet_addr = EthernetAddress([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
		let hardware_addr = HardwareAddress::Ethernet(ethernet_addr);
		info!("MAC address {}", hardware_addr);

		let dhcp = dhcpv4::Socket::new();

		// use the current time based on the wall-clock time as seed
		let mut config = Config::new(hardware_addr);
		config.random_seed = (arch::kernel::systemtime::now_micros()) / 1_000_000;
		if medium == Medium::Ethernet {
			config.hardware_addr = hardware_addr;
		}

		let iface = Interface::new(
			config,
			device.lock().deref_mut(),
			crate::executor::network::now(),
		);
		let mut sockets = SocketSet::new(vec![]);
		let dhcp_handle = sockets.add(dhcp);

		NetworkState::Initialized(Box::new(Self {
			iface,
			sockets,
			device,
			dhcp_handle,
			#[cfg(feature = "dns")]
			dns_handle: None,
		}))
	}

	#[cfg(not(feature = "dhcpv4"))]
	pub(crate) fn create() -> NetworkState<'a> {
		use core::ops::DerefMut;

		use smoltcp::phy::DeviceCapabilities;

		let (device, mac, medium) = if let Some(device) = hardware::get_network_driver() {
			let guard = device.lock();
			let mac = guard.get_mac_address();
			let DeviceCapabilities {
				max_transmission_unit: mtu,
				checksum: checksums,
				medium,
				..
			} = guard.capabilities();
			info!("{:?}", checksums);
			info!("MTU: {} bytes", mtu);
			(device, mac, medium)
		} else {
			return NetworkState::InitializationFailed;
		};

		let myip = Ipv4Address::from_str(hermit_var_or!("HERMIT_IP", "10.0.5.3")).unwrap();
		let mygw = Ipv4Address::from_str(hermit_var_or!("HERMIT_GATEWAY", "10.0.5.1")).unwrap();
		let mymask = Ipv4Address::from_str(hermit_var_or!("HERMIT_MASK", "255.255.255.0")).unwrap();
		// Quad9 DNS server
		#[cfg(feature = "dns")]
		let mydns1 = Ipv4Address::from_str(hermit_var_or!("HERMIT_DNS1", "9.9.9.9")).unwrap();
		// Cloudflare DNS server
		#[cfg(feature = "dns")]
		let mydns2 = Ipv4Address::from_str(hermit_var_or!("HERMIT_DNS2", "1.1.1.1")).unwrap();

		// calculate the netmask length
		// => count the number of contiguous 1 bits,
		// starting at the most significant bit in the first octet
		let mut prefix_len = (!mymask.octets()[0]).trailing_zeros();
		if prefix_len == 8 {
			prefix_len += (!mymask.octets()[1]).trailing_zeros();
		}
		if prefix_len == 16 {
			prefix_len += (!mymask.octets()[2]).trailing_zeros();
		}
		if prefix_len == 24 {
			prefix_len += (!mymask.octets()[3]).trailing_zeros();
		}

		let ethernet_addr = EthernetAddress([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
		let hardware_addr = HardwareAddress::Ethernet(ethernet_addr);
		let ip_addrs = [IpCidr::new(
			IpAddress::v4(
				myip.octets()[0],
				myip.octets()[1],
				myip.octets()[2],
				myip.octets()[3],
			),
			prefix_len.try_into().unwrap(),
		)];

		info!("MAC address {}", hardware_addr);
		info!("Configure network interface with address {}", ip_addrs[0]);
		info!("Configure gateway with address {}", mygw);

		// use the current time based on the wall-clock time as seed
		let mut config = Config::new(hardware_addr);
		config.random_seed = (arch::kernel::systemtime::now_micros()) / 1_000_000;
		if medium == Medium::Ethernet {
			config.hardware_addr = hardware_addr;
		}

		let mut iface = Interface::new(
			config,
			device.lock().deref_mut(),
			crate::executor::network::now(),
		);
		iface.update_ip_addrs(|ip_addrs| {
			ip_addrs
				.push(IpCidr::new(
					IpAddress::v4(
						myip.octets()[0],
						myip.octets()[1],
						myip.octets()[2],
						myip.octets()[3],
					),
					prefix_len.try_into().unwrap(),
				))
				.unwrap();
		});
		iface.routes_mut().add_default_ipv4_route(mygw).unwrap();

		#[allow(unused_mut)]
		let mut sockets = SocketSet::new(vec![]);

		#[cfg(feature = "dns")]
		let dns_handle = {
			let servers = &[mydns1.into(), mydns2.into()];
			let dns_socket = dns::Socket::new(servers, vec![]);
			sockets.add(dns_socket)
		};

		NetworkState::Initialized(Box::new(Self {
			iface,
			sockets,
			device,
			#[cfg(feature = "dns")]
			dns_handle: Some(dns_handle),
		}))
	}
}
