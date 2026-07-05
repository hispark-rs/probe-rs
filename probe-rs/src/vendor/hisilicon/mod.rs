//! HiSilicon RISC-V SoC support (WS63 / Hi3863; BS21/BS2X to follow).
//!
//! These parts run a HiSilicon RISC-V core whose Debug Module sits behind an ARM
//! CoreSight DAP (see [`sequences`]). The RISC-V debug transport is the generic
//! mem-AP DTM; this vendor supplies both the ARM-side debug bring-up and the
//! RISC-V-side SFC-safe reset sequence.
//!
//! Because `DebugSequence` is a single enum, we return `DebugSequence::Riscv`
//! so the RISC-V core gets the SFC-safe `reset_system_and_halt`. The ARM DAP
//! bring-up path in `session.rs` then falls back to `DefaultArmSequence`. The
//! `Ws63` struct also implements `ArmDebugSequence` (including the CoreSight
//! enable write), but that path is only reached when `DebugSequence::Arm` is
//! returned — which we intentionally don't do. Empirically the WS63 board's
//! debug pads are enabled by the external GPIO_04 strap, so the
//! `debug_device_unlock` write is not required for attach.

use crate::{config::DebugSequence, vendor::Vendor};
use probe_rs_target::Chip;

use sequences::Ws63;

pub mod sequences;

/// HiSilicon
#[derive(docsplay::Display)]
pub struct HiSilicon;

impl Vendor for HiSilicon {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // `chip.name` is the variant name (e.g. "WS63"), not the family name.
        if chip.name.starts_with("WS63") {
            // Return Riscv so the RISC-V core gets the SFC-safe reset sequence.
            // The ARM DAP bring-up falls back to DefaultArmSequence (see
            // session.rs). This is the same pattern as RP235x_riscv.
            Some(DebugSequence::Riscv(Ws63::create()))
        } else {
            None
        }
    }
}
