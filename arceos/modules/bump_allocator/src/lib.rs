#![no_std]

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::alloc::Layout;
use core::ptr::NonNull;

/// 早期内存分配器（bump allocator）。
///
/// 在正式的字节分配器、页分配器就绪之前使用，负责一段连续内存：
/// 字节从低地址向高地址增长，页从高地址向低地址增长，两者在中间相遇。
///
/// ```text
/// start        byte_ptr        page_ptr        end
/// |<- 已分配 ->|<-  空闲区  ->|<- 已分配页 ->|
/// ```
///
/// 字节块不能单独归还，只统计“仍在使用的块数”，当块数归零时整体回卷；
/// 页则完全不回收。
pub struct EarlyAllocator<const SIZE: usize> {
    /// 管理区间的起始地址。
    region_start: usize,
    /// 管理区间的结束地址（不含）。
    region_end: usize,
    /// 字节区的当前位置，向上生长。
    byte_ptr: usize,
    /// 页区的当前位置，向下生长。
    page_ptr: usize,
    /// 仍然存活的字节分配数量。
    live_allocs: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        Self {
            region_start: 0,
            region_end: 0,
            byte_ptr: 0,
            page_ptr: 0,
            live_allocs: 0,
        }
    }
}

/// 把地址向上对齐到 `align`（要求 `align` 是 2 的幂）。
const fn round_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// 把地址向下对齐到 `align`（要求 `align` 是 2 的幂）。
const fn round_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        self.region_start = start;
        self.region_end = start + size;
        self.byte_ptr = start;
        self.page_ptr = self.region_end;
        self.live_allocs = 0;
    }

    fn add_memory(&mut self, _start: usize, _size: usize) -> AllocResult {
        // 只认最初那一整段内存，额外区间无法并入。
        Err(AllocError::NoMemory)
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let align = layout.align().max(1);
        let begin = round_up(self.byte_ptr, align);
        let end = begin.checked_add(layout.size()).ok_or(AllocError::NoMemory)?;
        // 不能越过页区已经占走的部分。
        if end > self.page_ptr {
            return Err(AllocError::NoMemory);
        }

        self.byte_ptr = end;
        self.live_allocs += 1;
        NonNull::new(begin as *mut u8).ok_or(AllocError::NoMemory)
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        // 单块无法归还，等所有分配都释放后一次性回卷整个字节区。
        self.live_allocs = self.live_allocs.saturating_sub(1);
        if self.live_allocs == 0 {
            self.byte_ptr = self.region_start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.region_end - self.region_start
    }

    fn used_bytes(&self) -> usize {
        self.byte_ptr - self.region_start
    }

    fn available_bytes(&self) -> usize {
        self.page_ptr - self.byte_ptr
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        // `align_pow2` 是字节单位的对齐要求，必须是 2 的幂。
        if num_pages == 0 || !align_pow2.is_power_of_two() {
            return Err(AllocError::InvalidParam);
        }

        let bytes = num_pages.checked_mul(SIZE).ok_or(AllocError::NoMemory)?;
        let low = self.page_ptr.checked_sub(bytes).ok_or(AllocError::NoMemory)?;
        // 除对齐要求外，还必须按页对齐。
        let begin = round_down(low, align_pow2.max(SIZE));
        if begin < self.byte_ptr {
            return Err(AllocError::NoMemory);
        }

        self.page_ptr = begin;
        Ok(begin)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // 页不回收。
    }

    fn total_pages(&self) -> usize {
        (self.region_end - self.region_start) / SIZE
    }

    fn used_pages(&self) -> usize {
        (self.region_end - self.page_ptr) / SIZE
    }

    fn available_pages(&self) -> usize {
        (self.page_ptr - self.byte_ptr) / SIZE
    }
}
