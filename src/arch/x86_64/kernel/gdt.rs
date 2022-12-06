use alloc::boxed::Box;
use core::mem;
use core::sync::atomic::Ordering;

use x86::bits64::segmentation::*;
use x86::bits64::task::*;
use x86::dtables::{self, DescriptorTablePointer};
use x86::segmentation::*;
use x86::task::*;
use x86::Ring;

use super::interrupts::{IST_ENTRIES, IST_SIZE};
use super::scheduler::TaskStacks;
use super::CURRENT_STACK_ADDRESS;
use crate::arch::x86_64::kernel::percore::*;
use crate::config::*;

pub const GDT_NULL: u16 = 0;
pub const GDT_KERNEL_CODE: u16 = 1;
pub const GDT_KERNEL_DATA: u16 = 2;
pub const GDT_FIRST_TSS: u16 = 3;

const GDT_ENTRIES: usize = 5;

#[repr(align(4096))]
struct Gdt {
	entries: [Descriptor; GDT_ENTRIES],
}

impl Gdt {
	pub const fn new() -> Self {
		Gdt {
			entries: [Descriptor::NULL; GDT_ENTRIES],
		}
	}
}

fn init(gdt: &mut Gdt) {
	// The NULL descriptor is always the first entry.
	gdt.entries[GDT_NULL as usize] = Descriptor::NULL;

	// The second entry is a 64-bit Code Segment in kernel-space (Ring 0).
	// All other parameters are ignored.
	gdt.entries[GDT_KERNEL_CODE as usize] =
		DescriptorBuilder::code_descriptor(0, 0, CodeSegmentType::ExecuteRead)
			.present()
			.dpl(Ring::Ring0)
			.l()
			.finish();

	// The third entry is a 64-bit Data Segment in kernel-space (Ring 0).
	// All other parameters are ignored.
	gdt.entries[GDT_KERNEL_DATA as usize] =
		DescriptorBuilder::data_descriptor(0, 0, DataSegmentType::ReadWrite)
			.present()
			.dpl(Ring::Ring0)
			.finish();
}

pub fn add_current_core() {
	let gdt = Box::leak(Box::new(Gdt::new()));
	init(gdt);

	// Dynamically allocate memory for a Task-State Segment (TSS) for this core.
	let mut boxed_tss = Box::new(TaskStateSegment::new());

	// Every task later gets its own stack, so this boot stack is only used by the Idle task on each core.
	// When switching to another task on this core, this entry is replaced.
	boxed_tss.rsp[0] = CURRENT_STACK_ADDRESS.load(Ordering::Relaxed) + KERNEL_STACK_SIZE as u64
		- TaskStacks::MARKER_SIZE as u64;
	set_kernel_stack(boxed_tss.rsp[0]);

	// Allocate all ISTs for this core.
	// Every task later gets its own IST1, so the IST1 allocated here is only used by the Idle task.
	for i in 0..IST_ENTRIES {
		let ist = crate::mm::allocate(IST_SIZE, true);
		boxed_tss.ist[i] = ist.as_u64() + IST_SIZE as u64 - TaskStacks::MARKER_SIZE as u64;
	}

	unsafe {
		// Add this TSS to the GDT.
		let tss = Box::into_raw(boxed_tss);
		{
			let base = tss as u64;
			let tss_descriptor: Descriptor64 =
				<DescriptorBuilder as GateDescriptorBuilder<u64>>::tss_descriptor(
					base,
					mem::size_of::<TaskStateSegment>() as u64 - 1,
					true,
				)
				.present()
				.dpl(Ring::Ring0)
				.finish();
			gdt.entries[GDT_FIRST_TSS as usize..GDT_FIRST_TSS as usize + 2].copy_from_slice(
				&mem::transmute::<Descriptor64, [Descriptor; 2]>(tss_descriptor),
			);
		}

		// Store it in the PerCoreVariables structure for further manipulation.
		PERCORE.tss.set(tss);
	}

	unsafe {
		// Load the GDT for the current core.
		let gdtr = DescriptorTablePointer::new_from_slice(&(gdt.entries[0..GDT_ENTRIES]));
		dtables::lgdt(&gdtr);

		// Reload the segment descriptors
		load_cs(SegmentSelector::new(GDT_KERNEL_CODE, Ring::Ring0));
		load_ds(SegmentSelector::new(GDT_KERNEL_DATA, Ring::Ring0));
		load_es(SegmentSelector::new(GDT_KERNEL_DATA, Ring::Ring0));
		load_ss(SegmentSelector::new(GDT_KERNEL_DATA, Ring::Ring0));
		load_tr(SegmentSelector::new(GDT_FIRST_TSS, Ring::Ring0));
	}
}

pub extern "C" fn set_current_kernel_stack() {
	core_scheduler().set_current_kernel_stack();
}
