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

## Stage timing and SWD speed sweep

A second timing pass enabled the flasher's debug stage events, so its absolute
CLI times include diagnostic logging overhead and should be compared only with
other rows in this section. Each accepted row is the median of three complete
download + full verify + physical nRST + UART runs. `host -> RAM` is the sum of
the program-page AP1 uploads. `RAM -> SFC/rest` is the program-operation elapsed
time less that upload sum; for double buffering it is the non-overlapped
remainder, not the sum of all target page durations. `fixed` is wall time less
the CLI download time and includes attach, loader construction, teardown, and
the external timing boundary.

| Image | SWD | Mode | Erase | host -> RAM | RAM -> SFC/rest | Verify | CLI total | Wall | Fixed |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `uart_hello` | 2 MHz | single | 0.244 s | 0.727 s | 0.181 s | 0.963 s | 3.39 s | 6.02 s | 2.63 s |
| `uart_hello` | 2 MHz | explicit AP1 double | 0.248 s | 0.731 s | 0.178 s | 0.954 s | 3.39 s | 6.01 s | 2.62 s |
| `wifi_init_smoke` | 2 MHz | single | 1.478 s | 4.350 s | 1.372 s | 5.740 s | 14.22 s | 16.84 s | 2.62 s |
| `wifi_init_smoke` | 2 MHz | explicit AP1 double | 1.486 s | 4.348 s | 1.048 s | 5.700 s | 13.88 s | 16.49 s | 2.61 s |
| `uart_hello` | 3 MHz | single | 0.240 s | 0.538 s | 0.170 s | 0.746 s | 2.88 s | 5.37 s | 2.49 s |
| `uart_hello` | 3 MHz | explicit AP1 double | 0.238 s | 0.532 s | 0.169 s | 0.742 s | 2.87 s | 5.35 s | 2.48 s |
| `wifi_init_smoke` | 3 MHz | single | 1.400 s | 3.169 s | 1.282 s | 4.462 s | 11.51 s | 13.99 s | 2.48 s |
| `wifi_init_smoke` | 3 MHz | explicit AP1 double | 1.403 s | 3.175 s | 0.969 s | 4.465 s | 11.21 s | 13.67 s | 2.46 s |

The 2 MHz data shows that the two program/verify AP1 uploads account for about
8.7 seconds, or 61% of the RF single-buffer CLI time. Raising SWD to 3 MHz
reduced the RF double-buffer host upload by 27.0%, CLI time by 19.2%, and wall
time by 17.1%. It passed 100 protected reconnect cycles, 100 protected physical
nRST/recovery cycles, and all twelve formal full-verify/UART runs. A final 2 MHz
read-only baseline still returned `0xefbeadde`, measured 85.6/85.4/85.3 KiB/s
for 4/32/64 KiB AP1 reads, and completed 1,000 matching AP0/AP1 read pairs.

This J-Link OB reports a 96 MHz base clock with minimum divisor 24, so its
maximum supported SWD setting is 4 MHz. Requests for 5, 6, 8, and 10 MHz were
rejected before target I/O. A 4 MHz RF double-buffer run initially passed and
both 100-cycle reconnect and nRST gates passed, but a later formal RF
single-buffer run lost AP0/DMI core-status communication during the second
sector erase. The process failed explicitly without fallback. The first fresh
2 MHz single-buffer recovery then programmed the image but failed full verify
at `0x00230300`; after physical nRST, a fresh 2 MHz explicit-double run
completed full verify and reached all RF UART gates. Consequently 4 MHz is
rejected for this probe/board combination despite its attractive one-run time,
and 3 MHz is the highest speed supported by the current evidence.

## Cross-TAR write batching

The generic ADI `write_32` path now preserves the mandatory 1 KiB TAR
autoincrement boundaries while submitting each ordered
`TAR, DRW..., TAR, DRW...` sequence as one low-level probe batch. Backends that
do not implement mixed-register batching retain the previous per-register
default. The optimization is not WS63-specific and applies to ordinary halted
Memory-AP writes as well as the explicit running flash-buffer path. A mock test
writes 513 words across two boundaries and verifies one batch with TAR values
`0x0`, `0x400`, and `0x800`.

At 2 MHz a protected 64 KiB AP1 write/read/restore improved to 95.7 KiB/s.
The following debug-instrumented results are medians of three full-verify,
physical-nRST, UART-checked runs and compare directly with the 2 MHz rows above:

| Image | Mode | host -> RAM | Verify | CLI total | Wall | CLI improvement |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| `uart_hello` | single | 0.661 s | 0.864 s | 3.07 s | 5.47 s | 9.4% |
| `uart_hello` | explicit AP1 double | 0.658 s | 0.866 s | 3.06 s | 5.47 s | 9.7% |
| `wifi_init_smoke` | single | 4.024 s | 5.278 s | 13.09 s | 15.49 s | 7.9% |
| `wifi_init_smoke` | explicit AP1 double | 4.024 s | 5.290 s | 12.77 s | 15.17 s | 8.0% |

All twelve formal downloads passed full verify and their UART gates. The new
path also passed 100 protected reconnect cycles and 100 protected physical
nRST/recovery cycles. Errors remain explicit: the mixed batch returns the first
failed DAP transfer, and callers do not retry a partially issued logical write
through another memory path.

## Deliberate limits

- Generic running-state AP1 access is not enabled by target metadata.
- RTT/live-variable access is not moved to AP1 while the hart is running.
- Running AP1 flash-buffer uploads are separately target-authorized and remain
  disabled unless the caller supplies the explicit experimental flag. They do
  not authorize generic running-state memory access.
- No HiSilicon image-format behavior is implemented in probe-rs.
