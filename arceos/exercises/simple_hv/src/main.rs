#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

#[cfg(feature = "axstd")]
extern crate axstd as std;
extern crate alloc;
#[macro_use]
extern crate axlog;

mod task;
mod vcpu;
mod regs;
mod csrs;
mod sbi;
mod loader;

use vcpu::VmCpuRegisters;
use riscv::register::{scause, sstatus, stval};
use csrs::defs::hstatus;
use tock_registers::LocalRegisterCopy;
use csrs::{RiscvCsrTrait, CSR};
use vcpu::_run_guest;
use sbi::SbiMessage;
use loader::load_vm_image;
use axhal::mem::PhysAddr;
use crate::regs::GprIndex::{A0, A1};

const VM_ENTRY: usize = 0x8020_0000;

/// `mhartid` 的 CSR 编号。它是 M 模式寄存器，VS 模式访问会触发非法指令异常。
const CSR_MHARTID: usize = 0xf14;
/// 客体内核 `ld a0, 64(zero)` 使用的客户物理地址。
const GUEST_MAGIC_ADDR: usize = 0x40;
/// 模拟返回给客体的 hart id。
const EMU_HART_ID: usize = 0x1234;
/// 模拟返回给客体的 magic 值。
const EMU_MAGIC_VAL: usize = 0x6688;

/// 从指令字里取出 CSR 编号，也就是 inst[31:20] 这一段。
#[inline]
fn insn_csr(inst: usize) -> usize {
    (inst >> 20) & 0xfff
}

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // A new address space for vm.
    let mut uspace = axmm::new_user_aspace().unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let ept_root = uspace.page_table_root();
    prepare_vm_pgtable(ept_root);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx) {
    }

    panic!("Hypervisor ok!");
}

fn prepare_vm_pgtable(ept_root: PhysAddr) {
    let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        core::arch::riscv64::hfence_gvma_all();
    }
}

fn run_guest(ctx: &mut VmCpuRegisters) -> bool {
    unsafe {
        _run_guest(ctx);
    }

    vmexit_handler(ctx)
}

#[allow(unreachable_code)]
fn vmexit_handler(ctx: &mut VmCpuRegisters) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, EMU_MAGIC_VAL);
                        assert_eq!(a1, EMU_HART_ID);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    },
                    _ => todo!(),
                }
            } else {
                panic!("bad sbi message! ");
            }
        },
        Trap::Exception(Exception::IllegalInstruction) => {
            // 客体会执行 `csrr a1, mhartid`，而 mhartid 在 VS 模式不可见。
            // 这里做一次指令模拟：写入假的 hart id，然后跳过这条指令。
            let inst = stval::read();
            if insn_csr(inst) == CSR_MHARTID {
                ctx.guest_regs.gprs.set_reg(A1, EMU_HART_ID);
                ctx.guest_regs.sepc += 4;
            } else {
                panic!(
                    "Bad instruction: {:#x} sepc: {:#x}",
                    inst,
                    ctx.guest_regs.sepc
                );
            }
        },
        Trap::Exception(Exception::LoadGuestPageFault) => {
            // 客体会从客户物理地址 0x40 读 magic 值，而那一页并没有建立映射。
            // 这里模拟这条 load：直接填好 a0，并跳过指令。
            let fault_addr = stval::read();
            if fault_addr == GUEST_MAGIC_ADDR {
                ctx.guest_regs.gprs.set_reg(A0, EMU_MAGIC_VAL);
                ctx.guest_regs.sepc += 4;
            } else {
                panic!(
                    "LoadGuestPageFault: stval {:#x} sepc: {:#x}",
                    fault_addr,
                    ctx.guest_regs.sepc
                );
            }
        },
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                stval::read()
            );
        }
    }
    false
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Set hstatus
    let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
        riscv::register::hstatus::read().bits(),
    );
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    ctx.guest_regs.sstatus = sstatus.bits();
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
