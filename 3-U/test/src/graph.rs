//! Exercise loader-created shared dependencies through every graph path.

use core::mem;

use tau::{ModuleError, ModuleHandle, Ubi};

use super::{tau_test_passed, tau_test_progress};

/// Debugger checkpoint data: four descriptor PPNs followed by five local slots.
#[repr(C)]
struct Graph {
    handles: [usize; 4],
    slots: [usize; 5],
}

// Give application fixtures a real declared dependency, backed by the image's
// own ModuleId. Services use the original dependency-free image. Resolution
// stays loader-owned; supervisor preparation only validates the slice backing.
fn application_image(image: &[u8], buffer: &mut [u8]) {
    buffer.copy_from_slice(image);
    let word = |offset| usize::from_le_bytes(image[offset..offset + 8].try_into().unwrap());
    let entry = word(24);
    let headers = word(32);
    let stride = u16::from_le_bytes(image[54..56].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
    for index in 0..count {
        let header = headers + index * stride;
        if image[header..header + 4] != 1u32.to_le_bytes() {
            continue;
        }
        let address = word(header + 16);
        if entry < address || entry + size_of::<tau::Manifest>() > address + word(header + 32) {
            continue;
        }
        let manifest = word(header + 8) + entry - address;
        let dependency = manifest + mem::offset_of!(tau::Manifest, dependencies);
        let module_id = entry + mem::offset_of!(tau::Manifest, this);
        buffer[dependency..dependency + 8].copy_from_slice(&module_id.to_le_bytes());
        buffer[dependency + 8..dependency + 16].copy_from_slice(&1usize.to_le_bytes());
        return;
    }
    panic!("missing fixture manifest");
}

fn walk(slot: u16, route: usize, depth: usize, value: usize, expected: usize) {
    assert_eq!(
        Ubi::invoke::<3, 2>(slot, 7, [value, route, depth]).unwrap(),
        [value.rotate_left(7) ^ 0x5344_494f, expected],
    );
}

pub(super) fn run(case: usize, info: usize, pages: usize) -> ! {
    let raw = tau::Area::new(info, pages << 12).sl();
    let (dtb, _) = tau::Dtb::new(raw).unwrap();
    let image = dtb.boot_module("dummy-test").unwrap().sl::<u8>();
    let mut buffer = [0u8; 0x8000];
    let application = &mut buffer[..image.len()];
    application_image(image, application);

    let (a_slot, a) = Ubi::prepare_module(ModuleHandle::ROOT, application).unwrap();
    let (b_slot, b) = Ubi::prepare_module(ModuleHandle::ROOT, application).unwrap();
    let (a_service, service) = Ubi::prepare_module(a, image).unwrap();
    let b_service = Ubi::bind_dependency(b, service).unwrap();
    let (service_leaf, leaf) = Ubi::prepare_module(service, image).unwrap();
    let handles = [a, b, service, leaf];
    for (index, handle) in handles.iter().enumerate() {
        assert_ne!(*handle, ModuleHandle::ROOT);
        for other in &handles[..index] {
            assert_ne!(handle, other);
        }
    }
    let graph = Graph {
        handles: handles.map(ModuleHandle::as_raw),
        slots: [a_slot, b_slot, a_service, b_service, service_leaf].map(usize::from),
    };

    assert_eq!(
        Ubi::bind_dependency(a, a),
        Err(ModuleError::InvalidDependency)
    );
    assert_eq!(
        Ubi::bind_dependency(a, ModuleHandle::ROOT),
        Err(ModuleError::InvalidDependency)
    );
    assert_eq!(
        Ubi::bind_dependency(ModuleHandle::ROOT, ModuleHandle::from_raw(usize::MAX)),
        Err(ModuleError::InvalidDependency),
    );
    assert_eq!(
        Ubi::prepare_module(ModuleHandle::from_raw(usize::MAX), application),
        Err(ModuleError::InvalidDependency),
    );
    Ubi::unmap(image.as_ptr().addr(), image.len().div_ceil(4096)).unwrap();
    tau_test_progress(case, (&raw const graph).addr(), 0);

    // Identical ELF bytes prepared twice have independent counters.
    walk(a_slot, 0, 0, 1, 1);
    walk(b_slot, 0, 0, 2, 1);
    for slot in [a_slot, b_slot] {
        assert_eq!(
            Ubi::invoke::<2, 1>(slot, 8, [a.as_raw(), service.as_raw()]).unwrap(),
            [1]
        );
    }
    let a_route = usize::from(a_service) | (usize::from(service_leaf) << 16);
    let b_route = usize::from(b_service) | (usize::from(service_leaf) << 16);
    walk(a_slot, a_route, 2, 3, 1 | (1 << 16) | (2 << 32));
    walk(b_slot, b_route, 2, 4, 2 | (2 << 16) | (2 << 32));
    tau_test_progress(case, (&raw const graph).addr(), 1);

    // Both paths share service/leaf counters while A and B remain independent.
    for round in 1..=64 {
        let branch_count = round + 2;
        let shared_count = round * 2 + 1;
        walk(
            a_slot,
            a_route,
            2,
            round * 2,
            shared_count | (shared_count << 16) | (branch_count << 32),
        );
        let shared_count = shared_count + 1;
        walk(
            b_slot,
            b_route,
            2,
            round * 2 + 1,
            shared_count | (shared_count << 16) | (branch_count << 32),
        );
    }

    // Appending bindings after contexts exist preserves their cached roots.
    // Binding itself allocates no pages; failing preparation must reclaim all
    // of its newly allocated descriptor, tables, stem and temporary storage.
    for slot in 2..0x800 {
        assert_eq!(Ubi::bind_dependency(b, service).unwrap(), slot);
    }
    assert_eq!(Ubi::bind_dependency(b, service), Err(ModuleError::NoSlots));
    assert_eq!(
        Ubi::prepare_module(b, application),
        Err(ModuleError::NoSlots)
    );
    tau_test_passed(case, 128)
}
