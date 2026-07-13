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

## Deliberate limits

- Running-state AP1 access is not enabled by target metadata.
- RTT/live-variable access is not moved to AP1 while the hart is running.
- Double-buffer uploads remain on the legacy path while the flash algorithm is
  running. Any future opt-in must be limited to flash page-buffer uploads and
  account for the target's non-coherent D-cache; the presence of AP1 alone is
  not sufficient authorization for generic running-state memory access.
- No HiSilicon image-format behavior is implemented in probe-rs.
