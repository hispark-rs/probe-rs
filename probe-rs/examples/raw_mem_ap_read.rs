//! Read target memory through a selected ADIv5 Memory-AP.
//!
//! This is intentionally read-only. It is useful for diagnosing targets where
//! the CPU debug transport and the system-memory AP are distinct.

use anyhow::{Context, Result, bail};
use probe_rs::{
    architecture::arm::{FullyQualifiedApAddress, dp::DpAddress, sequences::DefaultArmSequence},
    probe::{WireProtocol, list::Lister},
};
use std::time::Instant;

fn parse_u64(value: &str) -> Result<u64> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(value.parse()?)
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: raw_mem_ap_read <ap> <b8|b16|b32> <address> [word-count] [--summary]";
    let ap: u8 = args.next().context(usage)?.parse()?;
    let width = args.next().context(usage)?;
    let address = parse_u64(&args.next().context(usage)?)?;
    let words: usize = args.next().map(|v| v.parse()).transpose()?.unwrap_or(4);
    let flags = args.collect::<Vec<_>>();
    if flags.iter().any(|flag| flag != "--summary") {
        bail!(usage);
    }
    let summary = flags.iter().any(|flag| flag == "--summary");
    let speed = std::env::var("PROBE_SPEED")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(2_000);

    let probes = Lister::new().list_all();
    let probe = probes
        .first()
        .context("no debug probe found")?
        .open()
        .context("failed to open probe")?;
    let mut probe = probe;
    probe.select_protocol(WireProtocol::Swd)?;
    probe.set_speed(speed)?;
    probe.attach_to_unspecified()?;

    let mut interface = probe
        .try_into_arm_debug_interface(DefaultArmSequence::create())
        .map_err(|(_, error)| error)?;
    interface.select_debug_port(DpAddress::Default)?;

    let ap_address = FullyQualifiedApAddress::v1_with_default_dp(ap);
    let mut memory = interface
        .memory_interface(&ap_address)
        .with_context(|| format!("AP{ap} is not a usable Memory-AP"))?;
    match width.as_str() {
        "b8" => {
            let mut values = vec![0u8; words];
            let started = Instant::now();
            memory.read_8(address, &mut values)?;
            let elapsed = started.elapsed();
            if summary {
                let checksum = values
                    .iter()
                    .fold(0u64, |sum, value| sum.wrapping_add(*value as u64));
                println!(
                    "read {} bytes in {elapsed:?} ({:.1} KiB/s), checksum={checksum:#x}",
                    values.len(),
                    values.len() as f64 / 1024.0 / elapsed.as_secs_f64()
                );
            } else {
                for (index, value) in values.iter().enumerate() {
                    println!("{:#010x}: {value:#04x}", address + index as u64);
                }
            }
        }
        "b16" => {
            let mut values = vec![0u16; words];
            let started = Instant::now();
            memory.read_16(address, &mut values)?;
            let elapsed = started.elapsed();
            if summary {
                let checksum = values
                    .iter()
                    .fold(0u64, |sum, value| sum.wrapping_add(*value as u64));
                println!(
                    "read {} bytes in {elapsed:?} ({:.1} KiB/s), checksum={checksum:#x}",
                    values.len() * 2,
                    values.len() as f64 * 2.0 / 1024.0 / elapsed.as_secs_f64()
                );
            } else {
                for (index, value) in values.iter().enumerate() {
                    println!("{:#010x}: {value:#06x}", address + (index as u64 * 2));
                }
            }
        }
        "b32" => {
            let mut values = vec![0u32; words];
            let started = Instant::now();
            memory.read_32(address, &mut values)?;
            let elapsed = started.elapsed();
            if summary {
                let checksum = values
                    .iter()
                    .fold(0u64, |sum, value| sum.wrapping_add(*value as u64));
                println!(
                    "read {} bytes in {elapsed:?} ({:.1} KiB/s), checksum={checksum:#x}",
                    values.len() * 4,
                    values.len() as f64 * 4.0 / 1024.0 / elapsed.as_secs_f64()
                );
            } else {
                for (index, value) in values.iter().enumerate() {
                    println!("{:#010x}: {value:#010x}", address + (index as u64 * 4));
                }
            }
        }
        _ => bail!(usage),
    }

    Ok(())
}
