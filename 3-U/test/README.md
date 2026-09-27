# OS test payload

`system-test` replaces the `system` ELF when `tau test` builds test firmware.
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
`target/tau` and `tau-qemu` contain the most recently selected payload. Run
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
DTB/arguments, `4` map error, `5` unexpected wait event, and `6` missing expected
fault.

| ID | Scenario | Progress values |
| --- | --- | --- |
| 1 | `boot` | None; validates invocation and DTB, then passes with hart ID |
| 2 | `mapping` | Mapped address, page count; host checks contents and PTEs |
| 3 | `timer` | Requested deadline, observed time; uses QEMU virt's 10 MHz timebase |
| 4 | `fault` | Fault address, zero; host validates S-mode trap entry instead of waiting for pass |

The fault scenario performs a load at the exported
`tau_test_fault_instruction` label. The host checks load page fault cause `13`,
`stval`, `sepc`, and that the previous privilege was U-mode. It does not resume
the fault handler or require supervisor recovery.

Add scenario code here and corresponding selection/assertions in the host's
`src/testing.rs`. Keep protocol IDs consistent across repositories and increment
the version when changing existing meanings. The payload is not a Rust unit-test
runner and does not automatically cover drivers private to `system`.
