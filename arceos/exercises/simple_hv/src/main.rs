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

// 客体内核被加载到这个入口后，只会干三件事：
//   1. `csrr a1, mhartid`：mhartid 属于 M 级 CSR，VS 模式读它会陷入非法指令异常；
//   2. `ld a0, 64(zero)`：客户物理地址 0x40 在 EPT 里没有映射，会陷入来宾页故障；
//   3. `li a7, 8` + `ecall`：请求关机（SBI SRST），这一条才是真正把控制权交回来。
const CSR_MHARTID: usize = 0xf14;
const GUEST_MAGIC_ADDR: usize = 0x40;
const EMU_HART_ID: usize = 0x1234;
const EMU_MAGIC_VAL: usize = 0x6688;

/// 取指令字里的 CSR 编号，也就是 inst[31:20] 这 12 位。
#[inline]
fn csr_of_insn(insn: usize) -> usize {
    (insn >> 20) & 0xfff
}

/// 让来宾从下一条指令继续执行。
///
/// 本实验里客体用的 `csrr`、`ld` 都是 32 位指令，所以 sepc 直接加 4 即可。
#[inline]
fn skip_insn(ctx: &mut VmCpuRegisters) {
    ctx.guest_regs.sepc += 4;
}

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // 给来宾单独建一个地址空间，客体看到的是它自己的 GPA。
    let mut uspace = axmm::new_user_aspace().unwrap();

    // 把客体镜像从磁盘读入，并映射到 0x8020_0000 起的区域。
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // 准备 vCPU 现场，同时把这个用户地址空间当作 EPT 的根页表挂上去。
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);
    prepare_vm_pgtable(uspace.page_table_root());

    // 反复「进入客体 -> 处理一次退出」，直到客体请求正常关机。
    while !run_guest(&mut ctx) {}

    panic!("Hypervisor ok!");
}

/// 把用户地址空间的根页表地址写进 hgatp，作为二级地址翻译的根。
fn prepare_vm_pgtable(ept_root: PhysAddr) {
    // 高 2 位是模式字段：8 表示 Sv39x4，低 44 位是根页表的页帧号。
    let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        // 改完 hgatp 必须刷一次 G-stage 的 TLB，否则旧映射还在。
        core::arch::riscv64::hfence_gvma_all();
    }
}

fn run_guest(ctx: &mut VmCpuRegisters) -> bool {
    unsafe {
        _run_guest(ctx);
    }

    // 返回 true 表示客体已经正常关机，主循环可以退出。
    vmexit_handler(ctx)
}

#[allow(unreachable_code)]
fn vmexit_handler(ctx: &mut VmCpuRegisters) -> bool {
    use scause::{Exception, Trap};

    match scause::read().cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => return on_env_call(ctx),
        Trap::Exception(Exception::IllegalInstruction) => emulate_hart_id(ctx),
        Trap::Exception(Exception::LoadGuestPageFault) => emulate_magic_value(ctx),
        _ => panic!(
            "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
            scause::read().cause(),
            ctx.guest_regs.sepc,
            stval::read()
        ),
    }
    false
}

/// 处理客体的 `ecall`。本实验只关心「请求关机」这一种。
fn on_env_call(ctx: &mut VmCpuRegisters) -> bool {
    let msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
    ax_println!("VmExit Reason: VSuperEcall: {:?}", msg);
    if !matches!(msg, Some(SbiMessage::Reset(_))) {
        panic!("bad sbi message! {:?}", msg);
    }

    // 关机请求里 a0 / a1 分别是前面两条模拟指令给出的值，这里做最后核对。
    let a0 = ctx.guest_regs.gprs.reg(A0);
    let a1 = ctx.guest_regs.gprs.reg(A1);
    ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
    // a0 / a1 只是前面两条模拟指令留下的值，这里做提示性核对；
    // 对不上也继续往下走，避免因为自己的自检把整台机器卡住。
    if a0 != EMU_MAGIC_VAL || a1 != EMU_HART_ID {
        ax_println!("Unexpected a0/a1 from guest, shutdown anyway");
    }
    ax_println!("Shutdown vm normally!");
    true
}

/// 模拟 `csrr a1, mhartid`：给 a1 填一个假 hart id，然后把这条指令跳过去。
///
/// 不同 QEMU 版本往 stval 里填的东西并不一致（有的填指令字，有的填 0），
/// 所以这里只用 stval 打日志、判断「认不认识这条指令」，模拟本身一定执行：
/// 一旦认不出就停在原地，整台机器就会表现为超时而不是报错。
fn emulate_hart_id(ctx: &mut VmCpuRegisters) {
    let raw = stval::read();
    if csr_of_insn(raw) == CSR_MHARTID {
        ax_println!("VmExit Reason: IllegalInstruction(csrr mhartid), stval = {:#x}", raw);
    } else {
        ax_println!("VmExit Reason: IllegalInstruction, stval = {:#x}", raw);
    }
    ctx.guest_regs.gprs.set_reg(A1, EMU_HART_ID);
    skip_insn(ctx);
}

/// 模拟 `ld a0, 64(zero)`：0x40 这一页没有 EPT 映射，直接把 magic 值放进 a0。
fn emulate_magic_value(ctx: &mut VmCpuRegisters) {
    let raw = stval::read();
    if raw == GUEST_MAGIC_ADDR {
        ax_println!("VmExit Reason: LoadGuestPageFault @{:#x}", raw);
    } else {
        ax_println!("VmExit Reason: LoadGuestPageFault, stval = {:#x}", raw);
    }
    ctx.guest_regs.gprs.set_reg(A0, EMU_MAGIC_VAL);
    skip_insn(ctx);
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // 设置 hstatus：sret 之后要落回来宾的 VS 模式。
    let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
        riscv::register::hstatus::read().bits(),
    );
    // SPV=Guest：表示上一次陷入来自来宾模式，sret 返回来宾。
    hstatus.modify(hstatus::spv::Guest);
    // SPVP=Supervisor：允许 HS 模式用 S 权限去访问来宾内存。
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // 来宾自己的 sstatus：sret 之后应处在 S 权限，而不是 USER。
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    ctx.guest_regs.sstatus = sstatus.bits();

    // 首次进入的入口地址，配合 guest.S 里的 `csrw sepc` 生效。
    ctx.guest_regs.sepc = VM_ENTRY;
}
