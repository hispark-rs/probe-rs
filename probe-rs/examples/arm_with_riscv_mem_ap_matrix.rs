//! Read-only capability matrix for a system-memory AP on an ARM-DAP RISC-V target.
//!
//! The RISC-V hart is halted before any system-memory access and its original
//! running state is restored before exit. This diagnostic never writes target
//! memory.

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
    let usage = "usage: arm_with_riscv_mem_ap_matrix <chip> <ap> <address> [--blocks <comma-separated-bytes>] [--switches <count>]";
    let chip = args.next().context(usage)?;
    let ap: u8 = args.next().context(usage)?.parse()?;
    let address = parse_u64(&args.next().context(usage)?)?;
    let mut blocks: Vec<usize> = vec![4 * 1024, 32 * 1024, 64 * 1024];
    let mut switches = 0usize;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--blocks" => {
                blocks = args
                    .next()
                    .context(usage)?
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<_, _>>()?;
            }
            "--switches" => switches = args.next().context(usage)?.parse()?,
            _ => bail!(usage),
        }
    }
    if blocks.is_empty()
        || blocks
            .iter()
            .any(|bytes| *bytes == 0 || !bytes.is_multiple_of(4))
    {
        bail!("block sizes must be non-zero multiples of four bytes");
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
    let matrix_result = (|| -> Result<()> {
        {
            let interface = session.get_arm_interface()?;
            let mut memory = interface
                .memory_interface(&ap_address)
                .with_context(|| format!("AP{ap} is not a usable Memory-AP"))?;

            let mut bytes = [0u8; 4];
            memory.read_8(address, &mut bytes)?;
            println!("AP{ap} b8  {address:#010x}: {bytes:02x?}");

            let mut halfwords = [0u16; 2];
            memory.read_16(address, &mut halfwords)?;
            println!("AP{ap} b16 {address:#010x}: {halfwords:04x?}");

            let mut words = [0u32; 1];
            memory.read_32(address, &mut words)?;
            println!("AP{ap} b32 {address:#010x}: {:#010x}", words[0]);

            for bytes in &blocks {
                let mut values = vec![0u32; bytes / 4];
                let started = Instant::now();
                memory.read_32(address, &mut values)?;
                let elapsed = started.elapsed();
                let checksum = values
                    .iter()
                    .fold(0u64, |sum, value| sum.wrapping_add(*value as u64));
                println!(
                    "AP{ap} read {bytes} bytes in {elapsed:?} ({:.1} KiB/s), checksum={checksum:#x}",
                    *bytes as f64 / 1024.0 / elapsed.as_secs_f64()
                );
            }
        }

        for index in 0..switches {
            let via_dmi = session.core(0)?.read_word_32(address)?;
            let via_ap = {
                let interface = session.get_arm_interface()?;
                let mut memory = interface.memory_interface(&ap_address)?;
                memory.read_word_32(address)?
            };
            if via_ap != via_dmi {
                return Err(anyhow!(
                    "AP0/AP{ap} mismatch after {index} switches: AP0={via_dmi:#010x}, AP{ap}={via_ap:#010x}"
                ));
            }
        }
        if switches != 0 {
            println!("AP0/AP{ap} completed {switches} matched read pairs");
        }
        Ok(())
    })();

    let resume_result: Result<()> = if was_running {
        session
            .core(0)
            .and_then(|mut core| core.run())
            .map_err(Into::into)
    } else {
        Ok(())
    };

    match (matrix_result, resume_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(matrix), Ok(())) => Err(matrix),
        (Ok(()), Err(resume)) => Err(resume.context("failed to restore running state")),
        (Err(matrix), Err(resume)) => Err(anyhow!(
            "matrix failed: {matrix:#}; failed to restore running state: {resume:#}"
        )),
    }
}
