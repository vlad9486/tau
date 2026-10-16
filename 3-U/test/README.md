# OS test payload

`system-test` is packed alongside `system` and `dummy-test`. `tau test` enables the
loader’s `system-test` feature to choose the initial U-mode program.
It uses the normal manifest/entry convention and supervisor. All host code lives
in the separate `tau-tool` repository, keeping this workspace RISC-V-only.

Run from the OS repository with the `tau` command from `tau-tool` installed:

```sh
tau test
tau test timer
tau test --list
```

The runner builds in the default `target` directory and starts a fresh four-hart
QEMU VM for each scenario. `system` and `system-test` are separate binaries, while
`target/riscv64imac-unknown-none-elf/release/tau` and `tau-qemu` contain all
static modules, with the most recently selected loader. Run
`tau build --qemu` to rebuild normal system firmware. Test logs stay under
`target/tau-test`. The runner reports results and diagnostic paths and returns a
nonzero exit status on failure.

## Protocol v1

`TAU_TEST_CONTROL` contains three little-endian `u64` words: magic
`0x5441555445535431`, protocol version `1`, and scenario ID `0`. The host writes
the scenario ID while stopped at `tau_test_ready`; the guest reads it using a
volatile load after continuing. This is a debugger control block, not an S-mode
interface.

Checkpoint functions use the C calling convention and naked assembly, preserving
the argument registers at their exact symbol addresses:

| Checkpoint | `a0` | `a1` | `a2` |
| --- | --- | --- | --- |
| `tau_test_ready` | boot hart | — | — |
| `tau_test_progress` | scenario ID | value | extra |
| `tau_test_passed` | scenario ID | result | — |
| `tau_test_failed` | scenario ID | failure code | detail |

Readiness and progress return; pass and failure loop until the host stops the VM.
No guest shutdown syscall is required. Failure codes are `0` unknown scenario,
`1` panic (detail is source line), `2` invalid initial event, `3` invalid boot
boot resources/arguments, `4` map error, `5` unexpected wait event, and `6` missing expected
fault, and `7` module invocation validation.

| ID | Scenario | Progress values |
| --- | --- | --- |
| 1 | `boot` | None; validates invocation and DTB arguments, then discovers and maps all three U-mode ELFs, then passes with hart ID |
| 2 | `mapping` | Mapped address, page count; host checks contents and PTEs |
| 3 | `timer` | Requested deadline, observed time; uses QEMU virt's 10 MHz timebase |
| 5 | `invocation` | Two prepared dependency slots, rollback and warm checkpoints; verifies lazy construction, stable roots/ASIDs, private dependency caches and mappings retained across 128 calls, nested context paths sharing ELF globals, fresh entry registers, caller restoration, permissions, source unmapping, failed-construction/preparation rollback, and both switch policies |

| 4 | `fault` | Fault address, zero; host validates S-mode trap entry instead of waiting for pass |
| 7 | `scheduling` | Wait-table address and count; software waits, errors, pending/coalescing/reuse, module isolation, competing waiters, idle deadlines, CPU-bound preemption/registers, abandoned reservations, canceled timers, and waiting in an invoked module |
| 8 | `spawn-rollback` | Phases 0 and 1; host forces ASID exhaustion, checks page/ID/ASID rollback, then restores capacity for successful creation |
| 6 | `graph` | Graph snapshot address and phase (0 prepared, 1 warm); prepares two applications with declared dependencies and a shared service with its own leaf, walks both paths repeatedly, checks separate instances and shared counters, verifies loader-only operations, exhausts binding slots, and checks failed-preparation rollback |

The invocation payload prepares an acyclic `A -> B` binding through
`Ubi::bind_dependency`; the host temporarily exhausts the ASID allocator to
exercise construction rollback. Direct and nested B calls share module globals
while using different retained roots. Dependency cycle validation belongs to
the trusted userspace loader at module installation and uninstallation.

The graph payload prepares four instances from `dummy-test` ELF bytes:
`system-test -> A -> Service -> Leaf` and
`system-test -> B -> Service -> Leaf`. A and B have independent globals;
both paths reach the same service and leaf descriptors. It keeps the returned
handles and local slots in a checkpoint snapshot. The host reads the protected
bindings to verify those handles and checks shared ELF/table backing, separate
private roots/stacks, retained roots, and allocator counts across 128 warm walks.
No host writes are used to create graph bindings. Run `tau test graph` alone or
include it in the full `tau test` suite.

The fault scenario performs a load at the exported
`tau_test_fault_instruction` label. The host checks load page fault cause `13`,
`stval`, `sepc`, and that the previous privilege was U-mode. It does not resume
the fault handler or require supervisor recovery.

Add scenario code here and corresponding selection/assertions in the host's
`src/testing.rs`. Keep protocol IDs consistent across repositories and increment
the version when changing existing meanings. The payload is not a Rust unit-test
runner and does not automatically cover drivers private to `system`.

Host checks for the software-wait ABI, table validation, and parking interleavings
can also run without QEMU from the sibling `tau-tool` checkout:

```sh
cargo test --test scheduling -- --ignored --nocapture
```
