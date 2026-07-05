//! Chip-specific debug bring-up for HiSilicon RISC-V SoCs.
//!
//! WS63 (Hi3863) is a HiSilicon "riscv31" RISC-V core whose Debug Module is
//! reached through an ARM CoreSight DAP (AHB-AP 0, DM @ `0x8000_0000`) — the same
//! `ArmWithRiscv` topology probe-rs uses for RP2350. The RISC-V debug interface
//! itself is brought up by the generic mem-AP DTM (see
//! [`RiscvCoreAccessOptions::dm_base`]).
//!
//! ## Reset handling
//!
//! The WS63 boot ROM initializes the SFC (SPI Flash Controller) during normal
//! reset. If the Debug Module's `ndmreset` or `hartreset`+`resethaltreq` is used
//! to reset and halt the core, the boot ROM never gets to run its SFC init code,
//! leaving the SFC controller in a partially-initialized state. This causes
//! `Flash Init Fail! ret = 0x80001341` on subsequent boots.
//!
//! HiSilicon's official OpenOCD (`HISPARK_TRACE_MODIFIES` in `riscv-013.c`)
//! avoids this by using the system controller's software reset (`sc_sys_res`)
//! instead of `ndmreset`, then waiting 500 ms for the halt to take effect.
//!
//! We replicate that behavior here: [`Ws63`] implements both
//! [`ArmDebugSequence`] (for DAP bring-up) and [`RiscvDebugSequence`] (for
//! SFC-safe reset). The vendor returns `DebugSequence::Riscv(...)` so the
//! RISC-V core gets the custom reset; the ARM DAP bring-up path falls back to
//! `DefaultArmSequence` (see `session.rs`).
//!
//! [`RiscvCoreAccessOptions::dm_base`]: probe_rs_target::RiscvCoreAccessOptions

use std::sync::Arc;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress, sequences::ArmDebugSequence,
};
use crate::architecture::riscv::communication_interface::RiscvCommunicationInterface;
use crate::architecture::riscv::sequences::RiscvDebugSequence;
use crate::memory::MemoryInterface;

/// WS63 control register that routes the debug pads to the CoreSight DAP.
///
/// From the WS63 OpenOCD target cfg connect note: `# enable coresight-swd mode`
/// / `mww 0x40010260 1`.
const WS63_CORESIGHT_ENABLE: u64 = 0x4001_0260;

// ── System controller registers (from HiSilicon OpenOCD reset_registers_set) ──

/// Debug flag register — written with magic value during software reset.
const SC_HRST_RES: u64 = 0x1010_0200;
/// Configuration lock register — must be unlocked before writing system regs.
const SC_CFG_LOCK: u64 = 0x1010_0044;
/// Unlock magic for `SC_CFG_LOCK`.
const SC_CFG_LOCK_KEY: u32 = 0xEA51_0000;
/// Peripheral CRG register — set to HOSC clock source during reset.
const PERI_CRG: u64 = 0x1000_001C;
/// HOSC clock source value (bit1~0: 0=HOSC, 1=XTAL, 2=PLL).
const PERI_CRG_HOSC: u32 = 0x0000_0008;
/// System reset register — writing 1 triggers a full system reset.
const SC_SYS_RES: u64 = 0x1010_0004;
const SC_SYS_RES_TRIGGER: u32 = 0x0000_0001;

/// Debug sequence for the HiSilicon WS63 (Hi3863).
///
/// Implements both [`ArmDebugSequence`] (DAP bring-up) and
/// [`RiscvDebugSequence`] (SFC-safe reset). See module docs for details.
#[derive(Debug)]
pub struct Ws63;

impl Ws63 {
    /// Create a WS63 debug sequence.
    pub fn create() -> Arc<Self> {
        Arc::new(Ws63)
    }
}

// ── ARM DAP bring-up ──────────────────────────────────────────────────────────

impl ArmDebugSequence for Ws63 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        match memory.write_word_32(WS63_CORESIGHT_ENABLE, 1) {
            Ok(()) => {
                let _ = memory.flush();
                tracing::debug!(
                    "WS63: enabled CoreSight-SWD debug path (0x{WS63_CORESIGHT_ENABLE:08x} = 1)"
                );
            }
            Err(e) => tracing::warn!(
                "WS63: CoreSight-SWD enable write failed ({e:?}); continuing — the debug \
                 pads are normally enabled by the external GPIO_04 power-on strap"
            ),
        }
        Ok(())
    }
}

// ── RISC-V SFC-safe reset ─────────────────────────────────────────────────────

impl RiscvDebugSequence for Ws63 {
    /// Reset the WS63 using the system controller's software reset, mirroring
    /// HiSilicon's official OpenOCD `HISPARK_TRACE_MODIFIES` flow.
    ///
    /// OpenOCD splits this into `assert_reset` (`reset_registers_set`) and
    /// `deassert_reset`. The full sequence is:
    ///
    /// 1. Write `SC_HRST_RES` = `0xA5A5A5A5` (debug flag)
    /// 2. Unlock `SC_CFG_LOCK` = `0xEA510000`
    /// 3. Set `PERI_CRG` = `0x8` (HOSC clock)
    /// 4. Write `SC_SYS_RES` = `0x1` (trigger system reset)
    /// 5. Wait 5 ms for reset to take effect
    /// 6. Read `SC_HRST_RES` to add clocks (ensure writes committed)
    /// 7. Wait 10 ms for chip to complete reset
    /// 8. `target_halt()` — request halt
    /// 9. Wait 500 ms for halt to take effect
    /// 10. Set PC to program entry (`0x3000004`)
    ///
    /// Steps 1–7 mirror `assert_reset` / `reset_registers_set`. Steps 8–10
    /// mirror `deassert_reset`. We don't set PC here — that's left to the
    /// caller (probe-rs `reset` resumes after `reset_and_halt`).
    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        _timeout: std::time::Duration,
    ) -> Result<(), crate::Error> {
        tracing::info!("WS63: SFC-safe system reset via system controller");

        // --- assert_reset: reset_registers_set ---

        // 1. Debug flag register
        let _ = interface.write_word_32(SC_HRST_RES, 0xA5A5_A5A5);
        // 2. Unlock config lock
        let _ = interface.write_word_32(SC_CFG_LOCK, SC_CFG_LOCK_KEY);
        // 3. HOSC clock source
        let _ = interface.write_word_32(PERI_CRG, PERI_CRG_HOSC);
        // 4. Trigger system reset
        tracing::debug!("WS63: writing SC_SYS_RES to trigger system reset");
        let _ = interface.write_word_32(SC_SYS_RES, SC_SYS_RES_TRIGGER);

        // 5. Wait 5 ms for reset to take effect
        std::thread::sleep(std::time::Duration::from_millis(5));

        // 6. Read SC_HRST_RES to add clocks (OpenOCD: "ensure the last write
        //    operations takes effect")
        let _ = interface.read_word_32(SC_HRST_RES);

        // 7. Wait 10 ms for chip to complete reset
        std::thread::sleep(std::time::Duration::from_millis(10));

        // --- deassert_reset ---

        // 8. Deferred halt: write haltreq to DM but do NOT poll immediately.
        //    The system reset has just released; the CPU is running through
        //    Boot ROM → flashboot → app.  Waiting before checking allhalted
        //    lets Boot ROM complete SFC init.  OpenOCD's "deassert_reset"
        //    does the same: target_halt + 500 ms wait.
        //
        //    We can't use interface.halt() here — it's synchronous (write
        //    haltreq + immediate poll), which catches the CPU too early in
        //    Boot ROM, before SFC is initialised.
        
        tracing::debug!("WS63: requesting deferred halt after system reset");

        // Write haltreq without waiting
        let mut dmcontrol: crate::architecture::riscv::Dmcontrol =
            interface.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_haltreq(true);
        interface.write_dm_register(dmcontrol)?;

        // 9. Wait 500 ms for Boot ROM + flashboot to complete (OpenOCD: deassert_reset).
        //    After this, the CPU should be in the app (or flashboot) region.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Now confirm the hart is halted (synchronous poll).
        dmcontrol.set_haltreq(false);
        interface.write_dm_register(dmcontrol)?;
        interface.wait_for_core_halted(std::time::Duration::from_secs(2))?;
        tracing::debug!("WS63: core halted after system reset");

        Ok(())
    }
}
