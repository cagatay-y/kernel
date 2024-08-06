use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::future;
use core::task::{Poll, Waker};

use endian_num::{le16, le32};
use hermit_sync::InterruptTicketMutex;
use virtio::vsock::{Hdr, Op, Type};

#[cfg(not(feature = "pci"))]
use crate::arch::kernel::mmio as hardware;
#[cfg(feature = "pci")]
use crate::drivers::pci as hardware;
use crate::executor::spawn;
use crate::io;
use crate::io::Error::EADDRINUSE;

pub(crate) static VSOCK_MAP: InterruptTicketMutex<VsockMap> =
	InterruptTicketMutex::new(VsockMap::new());

#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum VsockState {
	Listen,
	ReceiveRequest,
	Connected,
	Connecting,
	Shutdown,
}

/// WakerRegistration is derived from smoltcp's
/// implementation.
#[derive(Debug)]
pub(crate) struct WakerRegistration {
	waker: Option<Waker>,
}

impl WakerRegistration {
	pub const fn new() -> Self {
		Self { waker: None }
	}

	/// Register a waker. Overwrites the previous waker, if any.
	pub fn register(&mut self, w: &Waker) {
		match self.waker {
			// Optimization: If both the old and new Wakers wake the same task, we can simply
			// keep the old waker, skipping the clone.
			Some(ref w2) if (w2.will_wake(w)) => {}
			// In all other cases
			// - we have no waker registered
			// - we have a waker registered but it's for a different task.
			// then clone the new waker and store it
			_ => self.waker = Some(w.clone()),
		}
	}

	/// Wake the registered waker, if any.
	pub fn wake(&mut self) {
		if let Some(w) = self.waker.take() {
			w.wake()
		}
	}
}

pub(crate) const RAW_SOCKET_BUFFER_SIZE: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) struct RawSocket {
	pub remote_cid: u32,
	pub remote_port: u32,
	pub state: VsockState,
	pub waker: WakerRegistration,
	pub buffer: Vec<u8>,
}

impl RawSocket {
	pub fn new(state: VsockState) -> Self {
		Self {
			remote_cid: 0,
			remote_port: 0,
			state,
			waker: WakerRegistration::new(),
			buffer: Vec::with_capacity(RAW_SOCKET_BUFFER_SIZE),
		}
	}
}

async fn vsock_run() {
	future::poll_fn(|_cx| {
		if let Some(driver) = hardware::get_vsock_driver() {
			const HEADER_SIZE: usize = core::mem::size_of::<Hdr>();
			let mut driver_guard = driver.lock();
			let mut hdr: Option<Hdr> = None;
			let mut fwd_cnt: Option<u32> = None;

			driver_guard.process_packet(|header, data| {
				let op = Op::try_from(header.op.to_ne()).unwrap();
				let port = header.dst_port.to_ne();
				let type_ = Type::try_from(header.type_.to_ne()).unwrap();
				let mut vsock_guard = VSOCK_MAP.lock();

				if let Some(raw) = vsock_guard.get_mut_socket(port) {
					if op == Op::Request && raw.state == VsockState::Listen && type_ == Type::Stream
					{
						raw.state = VsockState::ReceiveRequest;
						raw.remote_cid = header.src_cid.to_ne().try_into().unwrap();
						raw.remote_port = header.src_port.to_ne();
						raw.waker.wake();
					} else if (raw.state == VsockState::Connected
						|| raw.state == VsockState::Shutdown)
						&& type_ == Type::Stream
						&& op == Op::Rw
					{
						raw.buffer.extend_from_slice(data);
						raw.waker.wake();
					} else if op == Op::CreditUpdate {
						debug!("CrediteUpdate currently not supported: {:?}", header);
					} else if op == Op::Shutdown {
						raw.state = VsockState::Shutdown;
					} else {
						hdr = Some(*header);
						if op == Op::CreditRequest {
							fwd_cnt = Some(raw.buffer.len().try_into().unwrap());
						}
					}
				}
			});

			if let Some(hdr) = hdr {
				driver_guard.send_packet(HEADER_SIZE, |buffer| {
					let response = unsafe { &mut *(buffer.as_mut_ptr() as *mut Hdr) };

					response.src_cid = hdr.dst_cid;
					response.dst_cid = hdr.src_cid;
					response.src_port = hdr.dst_port;
					response.dst_port = hdr.src_port;
					response.len = le32::from_ne(0);
					response.type_ = hdr.type_;
					if let Some(fwd_cnt) = fwd_cnt {
						// update fwd_cnt
						response.op = le16::from_ne(Op::CreditUpdate.into());
						response.fwd_cnt = le32::from_ne(fwd_cnt);
					} else {
						// reset connection
						response.op = le16::from_ne(Op::Rst.into());
						response.fwd_cnt = le32::from_ne(0);
					}
					response.flags = le32::from_ne(0);
					response.buf_alloc = le32::from_ne(RAW_SOCKET_BUFFER_SIZE as u32);
				});
			}

			Poll::Pending
		} else {
			Poll::Ready(())
		}
	})
	.await
}

pub(crate) struct VsockMap {
	port_map: BTreeMap<u32, RawSocket>,
}

impl VsockMap {
	pub const fn new() -> Self {
		Self {
			port_map: BTreeMap::new(),
		}
	}

	pub fn bind(&mut self, port: u32) -> io::Result<()> {
		self.port_map
			.try_insert(port, RawSocket::new(VsockState::Listen))
			.map_err(|_| EADDRINUSE)?;
		Ok(())
	}

	pub fn get_socket(&self, port: u32) -> Option<&RawSocket> {
		self.port_map.get(&port)
	}

	pub fn get_mut_socket(&mut self, port: u32) -> Option<&mut RawSocket> {
		self.port_map.get_mut(&port)
	}

	pub fn remove_socket(&mut self, port: u32) {
		let _ = self.port_map.remove(&port);
	}
}

pub(crate) fn init() {
	info!("Try to initialize vsock interface!");

	spawn(vsock_run());
}
