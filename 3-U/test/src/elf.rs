// TODO: remove duplicated code

use core::{mem, num::NonZero, ptr, str};

use elf::{
    ElfBytes,
    abi::{EM_RISCV, ET_EXEC, PF_R, PF_W, PF_X, PT_LOAD},
    endian::LittleEndian,
};
use tau::{Manifest, MappedRegion, ModuleId};
use dtb::Dtb;

/// Parse an ELF image in userspace, stage its segments via zero-copy page
/// handover, and bind the resulting module to `parent_module_id`.
pub fn prepare_module(
    parent: tau::ModuleHandle,
    bytes: &[u8],
    dtb: &Dtb<'_>,
) -> Result<(u16, tau::ModuleHandle), tau::ModuleError> {
    const PAGE: usize = 4096;
    const STAGE_VA: usize = 31 << 30;

    fn user_range(address: usize, bytes: usize) -> bool {
        if bytes == 0 {
            return false;
        }
        let Some(last) = address.checked_add(bytes - 1) else {
            return false;
        };
        (address < (1 << 38) && last < (1 << 38))
            || (address >= 0xffff_ffc0_4000_0000 && last >= address)
    }

    fn word(bytes: &[u8], offset: usize) -> Result<usize, tau::ModuleError> {
        Ok(usize::from_le_bytes(
            bytes
                .get(offset..offset + 8)
                .ok_or(tau::ModuleError::InvalidImage)?
                .try_into()
                .map_err(|_| tau::ModuleError::InvalidImage)?,
        ))
    }

    fn image_range<'a>(
        elf: &ElfBytes<'a, LittleEndian>,
        address: usize,
        length: usize,
    ) -> Result<&'a [u8], tau::ModuleError> {
        let end = address
            .checked_add(length)
            .ok_or(tau::ModuleError::InvalidImage)?;
        for ph in elf
            .segments()
            .ok_or(tau::ModuleError::InvalidImage)?
            .iter()
            .filter(|p| p.p_type == PT_LOAD)
        {
            let start = ph.p_vaddr as usize;
            let limit = start
                .checked_add(ph.p_filesz as usize)
                .ok_or(tau::ModuleError::InvalidImage)?;
            if address >= start && end <= limit {
                let data = elf
                    .segment_data(&ph)
                    .map_err(|_| tau::ModuleError::InvalidImage)?;
                return data
                    .get(address - start..end - start)
                    .ok_or(tau::ModuleError::InvalidImage);
            }
        }
        Err(tau::ModuleError::InvalidImage)
    }

    let elf = ElfBytes::<LittleEndian>::minimal_parse(bytes)
        .map_err(|_| tau::ModuleError::InvalidImage)?;
    if elf.ehdr.e_machine != EM_RISCV
        || elf.ehdr.e_type != ET_EXEC
        || elf.ehdr.class != elf::file::Class::ELF64
    {
        return Err(tau::ModuleError::InvalidImage);
    }
    let manifest = image_range(&elf, elf.ehdr.e_entry as usize, size_of::<Manifest>())?;
    let entry = word(manifest, mem::offset_of!(Manifest, entry))?;
    let deps = mem::offset_of!(Manifest, dependencies);
    let dependency_count = word(manifest, deps + 8)?;
    if dependency_count != 0 {
        let dep_bytes = dependency_count
            .checked_mul(size_of::<ModuleId>())
            .ok_or(tau::ModuleError::InvalidImage)?;
        image_range(&elf, word(manifest, deps)?, dep_bytes)?;
    }
    let stack_pages = mem::offset_of!(Manifest, stack_pages);
    let stack_pages = word(manifest, stack_pages)?;
    if stack_pages == 0 || stack_pages > 256 {
        return Err(tau::ModuleError::InvalidImage);
    }

    let regions_offset = mem::offset_of!(Manifest, mapped_regions);
    let regions_count = word(manifest, regions_offset + 8)?;
    let mut resolved_mmio = [const { None }; 16];
    let mut resolved_count = 0;
    if regions_count != 0 {
        if regions_count > 16 {
            return Err(tau::ModuleError::InvalidImage);
        }
        let region_bytes = regions_count
            .checked_mul(size_of::<MappedRegion>())
            .ok_or(tau::ModuleError::InvalidImage)?;
        let regions_data = image_range(&elf, word(manifest, regions_offset)?, region_bytes)?;
        for i in 0..regions_count {
            let offset = i * size_of::<MappedRegion>();
            let path_addr = word(regions_data, offset)?;
            let path_len = word(regions_data, offset + 8)?;
            let ptr = word(regions_data, offset + 16)?;
            let manifest_pages = word(regions_data, offset + 24)?;

            if manifest_pages == 0 || manifest_pages > 256 {
                return Err(tau::ModuleError::InvalidImage);
            }
            if ptr & (PAGE - 1) != 0 {
                return Err(tau::ModuleError::InvalidImage);
            }

            let path_bytes = image_range(&elf, path_addr, path_len)?;
            let dtb_path =
                str::from_utf8(path_bytes).map_err(|_| tau::ModuleError::InvalidImage)?;

            let area = dtb
                .iter()
                .find_map(|(props, path)| {
                    let mut target_parts = dtb_path.split('/').filter(|s| !s.is_empty());
                    let mut node_parts = path.iter().filter(|s| !s.is_empty());
                    loop {
                        match (target_parts.next(), node_parts.next()) {
                            (Some(target), Some(node)) => {
                                if target == node
                                    || (node.starts_with(target)
                                        && node[target.len()..].starts_with('@'))
                                {
                                    continue;
                                }
                                return None;
                            }
                            (None, None) => return props.find_reg(),
                            _ => return None,
                        }
                    }
                })
                .ok_or(tau::ModuleError::InvalidImage)?;

            let phys_base = area.base;
            let dtb_pages = area.len / PAGE;
            if phys_base & (PAGE - 1) != 0 || dtb_pages == 0 {
                return Err(tau::ModuleError::InvalidImage);
            }

            let pages = manifest_pages.min(dtb_pages);
            if pages == 0 {
                return Err(tau::ModuleError::InvalidImage);
            }

            let byte_len = pages
                .checked_mul(PAGE)
                .ok_or(tau::ModuleError::InvalidImage)?;
            if !user_range(ptr, byte_len) {
                return Err(tau::ModuleError::InvalidImage);
            }

            resolved_mmio[resolved_count] = Some((ptr, phys_base, pages));
            resolved_count += 1;
        }
    }

    let segments = elf.segments().ok_or(tau::ModuleError::InvalidImage)?;
    let mut root_index = None;
    let mut executable_entry = false;
    let mut total = 0usize;
    for (seg_idx, ph) in segments.iter().enumerate() {
        if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
            continue;
        }
        let address = ph.p_vaddr as usize;
        let end = address
            .checked_add(ph.p_memsz as usize)
            .ok_or(tau::ModuleError::InvalidImage)?;
        let index = (address >> 30) & 511;
        total = total
            .checked_add(ph.p_memsz as usize)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if total > 64 * 1024 * 1024
            || ph.p_filesz > ph.p_memsz
            || !user_range(address, ph.p_memsz as usize)
            || index == 511
            || index == 256
            || ((end - 1) >> 30) & 511 != index
            || root_index.is_some_and(|old| old != index)
            || ph.p_flags & PF_R == 0
            || ph.p_flags & (PF_W | PF_X) == PF_W | PF_X
            || (ph.p_align > 1
                && (!ph.p_align.is_power_of_two()
                    || ph.p_vaddr % ph.p_align != ph.p_offset % ph.p_align))
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        let first = address & !0xfff;
        let page_end = first + (end - first).div_ceil(PAGE) * PAGE;
        for prev in segments.iter().take(seg_idx) {
            if prev.p_type != PT_LOAD || prev.p_memsz == 0 {
                continue;
            }
            let prev_addr = prev.p_vaddr as usize;
            let prev_end = prev_addr + prev.p_memsz as usize;
            let prev_first = prev_addr & !0xfff;
            let prev_page_end = prev_first + (prev_end - prev_first).div_ceil(PAGE) * PAGE;
            if first < prev_page_end && prev_first < page_end {
                return Err(tau::ModuleError::InvalidImage);
            }
        }
        root_index = Some(index);
        executable_entry |=
            entry >= address && entry < end && ph.p_flags & PF_X != 0 && entry & 1 == 0;
        elf.segment_data(&ph)
            .map_err(|_| tau::ModuleError::InvalidImage)?;
    }
    if !executable_entry {
        return Err(tau::ModuleError::InvalidImage);
    }
    let root_index = root_index.ok_or(tau::ModuleError::InvalidImage)?;
    for &(virt, _, pages) in resolved_mmio[..resolved_count].iter().flatten() {
        let end = virt + pages * PAGE;
        if (virt >> 30) & 511 != root_index || ((end - 1) >> 30) & 511 != root_index {
            return Err(tau::ModuleError::InvalidImage);
        }
    }
    for i in 0..resolved_count {
        let (virt1, _, pages1) = resolved_mmio[i].unwrap();
        let end1 = virt1 + pages1 * PAGE;
        for x in resolved_mmio.iter().take(resolved_count).skip(i + 1) {
            let (virt2, _, pages2) = x.unwrap();
            let end2 = virt2 + pages2 * PAGE;
            if virt1 < end2 && virt2 < end1 {
                return Err(tau::ModuleError::InvalidImage);
            }
        }
    }
    let wait_base = word(manifest, mem::offset_of!(Manifest, wait_slots))?;
    let wait_count = word(
        manifest,
        mem::offset_of!(Manifest, wait_slots) + size_of::<usize>(),
    )?;
    if wait_count != 0 {
        let wait_bytes = wait_count
            .checked_mul(64)
            .ok_or(tau::ModuleError::InvalidImage)?;
        let wait_end = wait_base
            .checked_add(wait_bytes)
            .ok_or(tau::ModuleError::InvalidImage)?;
        if wait_base == 0
            || wait_base & 63 != 0
            || !user_range(wait_base, wait_bytes)
            || (wait_base >> 30) & 511 != root_index
            || ((wait_end - 1) >> 30) & 511 != root_index
        {
            return Err(tau::ModuleError::InvalidImage);
        }
        for page_addr in (wait_base & !(PAGE - 1)..wait_end).step_by(PAGE) {
            let mut writable = false;
            for ph in segments
                .iter()
                .filter(|p| p.p_type == PT_LOAD && p.p_memsz != 0)
            {
                let first = (ph.p_vaddr as usize) & !0xfff;
                let end = (ph.p_vaddr as usize) + (ph.p_memsz as usize);
                let page_end = first + (end - first).div_ceil(PAGE) * PAGE;
                if page_addr >= first
                    && page_addr < page_end
                    && ph.p_flags & (PF_R | PF_W) == PF_R | PF_W
                {
                    writable = true;
                    break;
                }
            }
            if !writable {
                return Err(tau::ModuleError::InvalidImage);
            }
        }
    }

    let handle = tau::Ubi::create_module(
        entry,
        stack_pages,
        wait_base,
        wait_count,
        tau::ModuleCapabilities::empty(),
    )?;
    for &(virt, phys, pages) in resolved_mmio[..resolved_count].iter().flatten() {
        for p in 0..pages {
            let va = virt + p * PAGE;
            let pa = phys + p * PAGE;
            if tau::Ubi::map_module(handle, NonZero::new(pa), va, 1, tau::PageFlags::RW).is_err() {
                return Err(tau::ModuleError::OutOfMemory);
            }
        }
    }
    for ph in segments
        .iter()
        .filter(|p| p.p_type == PT_LOAD && p.p_memsz != 0)
    {
        let address = ph.p_vaddr as usize;
        let end = address + ph.p_memsz as usize;
        let first = address & !0xfff;
        let pages = (end - first).div_ceil(PAGE);
        let file = elf
            .segment_data(&ph)
            .map_err(|_| tau::ModuleError::InvalidImage)?;
        let mut flags = tau::PageFlags::READ;
        if ph.p_flags & PF_W != 0 {
            flags = flags.union(tau::PageFlags::WRITE);
        }
        if ph.p_flags & PF_X != 0 {
            flags = flags.union(tau::PageFlags::EXEC);
        }
        let file_end = address + file.len();
        for i in 0..pages {
            let page_va = first + i * PAGE;
            let is_mmio = resolved_mmio[..resolved_count]
                .iter()
                .flatten()
                .any(|&(virt, _, p_count)| page_va >= virt && page_va < virt + p_count * PAGE);
            if is_mmio {
                continue;
            }
            let phys = tau::Ubi::alloc_physical(0, tau::MemoryZone::Normal)
                .map_err(|_| tau::ModuleError::OutOfMemory)?;
            let copy_start = page_va.max(address);
            let copy_end = (page_va + PAGE).min(file_end);
            if copy_start < copy_end {
                if tau::Ubi::map_module(
                    tau::ModuleHandle::ROOT,
                    NonZero::new(phys),
                    STAGE_VA,
                    1,
                    tau::PageFlags::RW,
                )
                .is_err()
                {
                    let _ = tau::Ubi::free_physical(phys, 0, tau::MemoryZone::Normal);
                    return Err(tau::ModuleError::OutOfMemory);
                }
                unsafe {
                    ptr::copy_nonoverlapping(
                        file.as_ptr().add(copy_start - address),
                        ptr::with_exposed_provenance_mut::<u8>(STAGE_VA + (copy_start - page_va)),
                        copy_end - copy_start,
                    );
                }
                let _ = tau::Ubi::unmap_module(tau::ModuleHandle::ROOT, STAGE_VA, 1);
            }
            if tau::Ubi::map_module(handle, NonZero::new(phys), page_va, 1, flags).is_err() {
                let _ = tau::Ubi::free_physical(phys, 0, tau::MemoryZone::Normal);
                return Err(tau::ModuleError::OutOfMemory);
            }
        }
    }
    let slot = tau::Ubi::bind_dependency(parent, handle)?;
    Ok((slot, handle))
}
