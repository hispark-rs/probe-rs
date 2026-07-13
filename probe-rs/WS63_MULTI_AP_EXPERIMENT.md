# WS63 halted multi-AP experiment

This branch is an experimental follow-up to probe-rs PR #4105. It keeps the
RISC-V DMI transport on AP0 and uses AP1 only for explicitly allowed system-RAM
accesses while the hart is halted. All measurements below used the same J-Link,
the same WS63 board, 2 MHz SWD, single buffering, and full post-flash verify.

## Reproduction context

- PR #4105 baseline commit: `e458b7407b2a020f2c52481f2de99b70d9de888e`
- AP0 SRAM baseline: `0x00a00000 = efbeadde`
- AP1 allowed range: `0x00a00000..0x00a8df00`
- Destructive scratch range used for validation: `0x00a70000..0x00a80000`
- Firmware source: `/home/sanchuanhehe/Documents/hispark-rs/hisi-riscv-rs`
- RF vendor SDK: `/home/sanchuanhehe/Documents/hispark/fbb_ws63/src`
- RF compiler prefix: `riscv32-linux-musl-`

The RF image was built by the repository's guarded
`rf-build-full-init-lld-layout-patch.sh` flow with `FBB_WS63_SDK` set to the
vendor SDK above. Both images were converted with `hisi-fwpkg plan`; probe-rs
only downloaded the resulting raw image at the plan's base address.

## AP1 capability and safety

- 8-, 16-, and 32-bit reads at `0x00a00000` agreed with AP0.
- Read throughput was 84.9 KiB/s for 4 KiB, 84.3 KiB/s for 32 KiB, and
  84.2 KiB/s for 64 KiB.
- 1,000 AP0/AP1 matched read pairs completed without a mismatch.
- Halted AP1 write/read/restore passed for 4,096, 65,532, and 65,536 bytes.
  Measured AP1 write throughput was 87.9, 89.3, and 88.0 KiB/s respectively.
- Every destructive test restored the original scratch contents before resume.

## Single-buffer flash results

Commands used `--verify --disable-double-buffering --speed 2000`. Times in the
table are the CLI's download time; the value in parentheses is the surrounding
wall-clock time including attach/teardown.

| Planned image | Bytes | PR #4105 / AP0 | Halted AP1 | Median improvement |
| --- | ---: | --- | --- | ---: |
| `uart_hello` | 11,688 | 29.88 / 29.84 / 29.78 s (31.68 / 31.52 / 31.47 s) | 3.03 / 3.08 / 3.06 s (5.38 / 5.39 / 5.38 s) | 89.7% |
| `wifi_init_smoke --features full-init` | 393,156 | 173.30 / 173.11 / 173.08 s (175.11 / 174.80 / 174.78 s) | 13.20 / 13.30 / 13.32 s (15.53 / 15.59 / 15.61 s) | 92.3% |

The existing per-page timing log measured a 65,536-byte host-to-RAM transfer at
15.626 s through AP0 and 0.789 s through AP1 (19.8x faster). The verify-buffer
transfer measured 15.695 s through AP0 and 0.772 s through AP1 (20.3x faster).

All twelve measured downloads (six AP0 and six AP1 across the two images)
completed full verify successfully. After physical J-Link nRST, `uart_hello`
printed `Hello from WS63 (HAL UART driver)!` and continuous `tick` markers. The
RF image reached `RF2_INIT_OK` and `RF3_SCAN_OK count=0x0000000d`. Its later
`RF5C_PING_OK` gate is a known firmware/configuration issue and is not treated
as a flash-transport failure.

## Running-state and recovery safety gate

The destructive `ws63_multi_ap_safety` example saves and readback-verifies the
complete `0x00a70000..0x00a80000` scratch range before disconnecting. A separate
temporary CPU code slot is also saved and restored. The following tests used
2 MHz SWD:

- AP1 write/read/restore passed while the hart executed an idle loop for 4 KiB,
  32 KiB, 64 KiB, 65,532 bytes, and 65,536 bytes. Running-state AP1 write
  throughput was 85.2--89.0 KiB/s.
- A plain RISC-V `fence rw,rw` was not sufficient for CPU/AP visibility. WS63
  has a non-coherent 4 KiB data cache with 32-byte lines. AP1 write -> CPU read
  passed only after the CPU invalidated the source line through `DCINCVA`
  (`0x7c5`) and `DCMAINT` (`0x7c3`, command `0x5`). CPU write -> AP1 read passed
  only after the CPU cleaned the destination line with command `0x9`.
- 100 independent attach/AP0/AP1/disconnect cycles passed. Each cycle saved all
  64 KiB, wrote and verified 4 KiB, then restored and verified all 64 KiB.
- 100 physical nRST/reattach cycles passed with the same per-cycle protection
  before reset.
- AP access while nRST was asserted returned a DAP communication error; AP1
  access recovered immediately after reset deassert and reconnect.
- A reset after half of a logical 64 KiB transfer retained 0/8,192 committed
  words and only 438/8,192 words from the nominally untouched suffix. The reset
  boot path therefore makes the complete logical transfer untrustworthy. The
  harness reattached and restored the whole protected range; software must not
  resume the transfer or silently fall back after such a reset.

The first reset-during-transfer harness revision incorrectly asserted that the
committed prefix must survive reset and exited before its cleanup path when that
assertion failed. This affected only the documented reserved scratch range; the
test was corrected to treat all post-reset RAM as invalid and to unconditionally
restore the complete range. Subsequent recovery and 100-cycle tests used the
corrected behavior.

The final post-stress baseline still read `0xefbeadde` through both AP0 and AP1.
AP1 read throughput was 85.8 KiB/s for 4 KiB and 85.1 KiB/s for both 32 KiB and
64 KiB; 1,000 AP0/AP1 matched read pairs completed without mismatch.

## Explicit double-buffer experiment

WS63 declares the narrow `system_memory_flash_buffers_while_running` target
capability, but use still requires the explicit
`--enable-riscv-system-memory-double-buffering` download flag. Without that
flag, probe-rs behavior is unchanged. The specialized entry point is write-only
and is called only for flash page buffers; generic running-state `MemoryInterface`
traffic remains on the legacy path.

The host tracks each buffer as `Empty`, `Ready`, `Busy`, or `Consumed`. It loads
the ready buffer through AP1 while AP0 controls the active algorithm, waits with
the flash algorithm's bounded page timeout before reusing the busy buffer, and
marks it consumed only after successful completion. An AP1 error, including a
partial write, is returned with AP and operation context and triggers a best-
effort halt; no AP0 retry occurs.

Aligned page buffers use 32-bit AP transfers. An initial byte-transfer revision
was functionally correct but took about 2.68 s per 64 KiB page and was rejected
for performance regression. The final path took 0.719--0.729 s per 64 KiB page.
The multi-page RF result demonstrates a useful, though small, overlap window
between the host upload and target SFC operation.

All formal runs used full verify, 2 MHz SWD, physical nRST, and UART capture:

| Planned image | Mode | CLI times | Wall times | Median result |
| --- | --- | --- | --- | --- |
| `uart_hello` | explicit AP1 double | 3.04 / 3.05 / 3.03 s | 5.38 / 5.68 / 5.66 s | 3.04 s |
| `uart_hello` | single | 3.06 / 3.04 / 3.09 s | 5.67 / 5.65 / 5.71 s | 3.06 s |
| `wifi_init_smoke --features full-init` | explicit AP1 double | 12.97 / 12.91 / 12.95 s | 15.59 / 15.47 / 15.57 s | 12.95 s |
| `wifi_init_smoke --features full-init` | single | 13.30 / 13.22 / 13.29 s | 15.94 / 15.83 / 15.90 s | 13.29 s |

The small image occupies one 64 KiB flash page, so its 0.02 s median difference
is effectively noise. The six-page RF image improved median CLI time by 2.6%
and wall time by 2.1%, without regressing the single-buffer path. Every measured
run completed full verify. `uart_hello` printed its greeting and ticks after each
reset. Every extended RF capture reached `RF1_IMAGE_OK`, `RF2_INIT_OK`, and
`RF3_SCAN_OK`; the later `RF5B_CONFIG_ERR:0x00000005` / missing
`RF5C_PING_OK` remains the known firmware/configuration issue.

## Deliberate limits

- Generic running-state AP1 access is not enabled by target metadata.
- RTT/live-variable access is not moved to AP1 while the hart is running.
- Running AP1 flash-buffer uploads are separately target-authorized and remain
  disabled unless the caller supplies the explicit experimental flag. They do
  not authorize generic running-state memory access.
- No HiSilicon image-format behavior is implemented in probe-rs.
