//! HiSilicon RISC-V SoC support (WS63 / Hi3863; BS21/BS2X to follow).
//!
//! These parts run a HiSilicon RISC-V core whose Debug Module sits behind an ARM
//! CoreSight DAP (see [`sequences`]). The RISC-V debug transport is the generic
//! mem-AP DTM; this vendor supplies both the ARM-side debug bring-up and the
//! RISC-V-side SFC-safe reset sequence.
//!
//! `Ws63` provides separate ARM DAP and RISC-V core sequences via
//! `DebugSequence::ArmRiscv`, so DAP bring-up keeps the CoreSight enable write
//! while RISC-V reset uses the SFC-safe system-controller reset sequence.

use crate::{config::DebugSequence, vendor::Vendor};
use probe_rs_target::Chip;

use sequences::Ws63;

pub mod sequences;

/// HiSilicon
#[derive(docsplay::Display)]
pub struct HiSilicon;

impl Vendor for HiSilicon {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("WS63") {
            let sequence = Ws63::create();
            Some(DebugSequence::ArmRiscv {
                arm: sequence.clone(),
                riscv: sequence,
            })
        } else {
            None
        }
    }
}
