//! Destructively verify a system-memory AP while the RISC-V hart is halted.
//!
//! The original words are restored before the hart resumes. This command is
//! intentionally opt-in and should only be used on a proven scratch range.

use anyhow::{Context, Result, anyhow, bail};
use probe_rs::{
    CoreStatus, MemoryInterface, Permissions,
    architecture::arm::FullyQualifiedApAddress,
    probe::{WireProtocol, list::Lister},
};
use std::time::{Duration, Instant};

fn parse_u64(value: &str) -> Result<u64> {
    if let Some(hex) = value.trim().strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(value.parse()?)
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: arm_with_riscv_mem_ap_roundtrip --i-know-this-writes-target <chip> <ap> <address> <bytes>";
    if args.next().as_deref() != Some("--i-know-this-writes-target") {
        bail!(usage);
    }
    let chip = args.next().context(usage)?;
    let ap: u8 = args.next().context(usage)?.parse()?;
    let address = parse_u64(&args.next().context(usage)?)?;
    let bytes: usize = args.next().context(usage)?.parse()?;
    if args.next().is_some() || bytes == 0 || !bytes.is_multiple_of(4) {
        bail!("{usage}; bytes must be a non-zero multiple of 4");
    }
    let speed = std::env::var("PROBE_SPEED")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(2_000);

    let probes = Lister::new().list_all();
    let mut probe = probes
        .first()
        .context("no debug probe found")?
        .open()
        .context("failed to open probe")?;
    probe.select_protocol(WireProtocol::Swd)?;
    probe.set_speed(speed)?;
    let mut session = probe.attach(chip, Permissions::default())?;

    let was_running = {
        let mut core = session.core(0)?;
        let running = !matches!(core.status()?, CoreStatus::Halted(_));
        if running {
            core.halt(Duration::from_secs(1))?;
        }
        running
    };

    let ap_address = FullyQualifiedApAddress::v1_with_default_dp(ap);
    let words = bytes / 4;
    let mut original = vec![0u32; words];
    let mut readback = vec![0u32; words];
    let pattern = (0..words)
        .map(|index| 0x5753_0000u32 ^ (index as u32).wrapping_mul(0x9E37_79B9))
        .collect::<Vec<_>>();

    let mut original_read = false;
    let transfer_result = (|| -> Result<()> {
        let interface = session.get_arm_interface()?;
        let mut memory = interface.memory_interface(&ap_address)?;
        memory.read_32(address, &mut original)?;
        original_read = true;

        let write_started = Instant::now();
        memory.write_32(address, &pattern)?;
        let write_elapsed = write_started.elapsed();
        memory.read_32(address, &mut readback)?;
        if readback != pattern {
            return Err(anyhow!("pattern readback mismatch"));
        }

        memory.write_32(address, &original)?;
        memory.read_32(address, &mut readback)?;
        if readback != original {
            return Err(anyhow!("original-data restore mismatch"));
        }

        println!(
            "AP{ap} wrote, verified, and restored {bytes} bytes in {write_elapsed:?} ({:.1} KiB/s write)",
            bytes as f64 / 1024.0 / write_elapsed.as_secs_f64()
        );
        Ok(())
    })();

    let recovery_result = if transfer_result.is_err() && original_read {
        // Best-effort recovery through the target's configured core-memory path.
        session
            .core(0)
            .and_then(|mut core| core.write_32(address, &original))
            .context("failed to restore original data through the configured core-memory path")
    } else {
        Ok(())
    };
    let resume_result: Result<()> = if was_running {
        session
            .core(0)
            .and_then(|mut core| core.run())
            .map_err(Into::into)
    } else {
        Ok(())
    };

    let mut failures = Vec::new();
    if let Err(error) = transfer_result {
        failures.push(format!("transfer failed: {error:#}"));
    }
    if let Err(error) = recovery_result {
        failures.push(format!("recovery failed: {error:#}"));
    }
    if let Err(error) = resume_result {
        failures.push(format!("resume failed: {error:#}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        bail!(failures.join("; "))
    }
}
