//! Destructive WS63 multi-AP running-state and recovery safety experiments.
//!
//! The complete scratch range and the temporary CPU test code slot are saved
//! before use and restored with readback verification before exit. This is a
//! hardware-lab diagnostic, not a target auto-detection mechanism.

use anyhow::{Context, Result, anyhow, bail, ensure};
use probe_rs::{
    CoreStatus, Permissions, Session,
    architecture::arm::{
        FullyQualifiedApAddress, dp::DpAddress, memory::ArmMemoryInterface,
        sequences::DefaultArmSequence,
    },
    probe::{Probe, WireProtocol, list::Lister},
};
use std::{
    thread,
    time::{Duration, Instant},
};

const SCRATCH_BYTES: usize = 64 * 1024;
// Use a dedicated slot away from the flash algorithm entry point. Every run
// saves and restores this slot; changing it also avoids stale I-cache lines
// from earlier revisions of this diagnostic.
const CODE_ADDRESS: u64 = 0x00a0_0100;
const TEST_CODE: [u32; 20] = [
    0x0050_0693, // li a3, 5 (DCACHE_INV_BY_VA)
    0x7c55_1073, // csrw DCINCVA, a0
    0x7c36_9073, // csrw DCMAINT, a3
    0x0330_000f, // fence rw, rw
    0x0005_2603, // lw a2, 0(a0)
    0xfe06_08e3, // beqz a2, invalidate-and-load
    0x00c5_a023, // sw a2, 0(a1)
    0x0090_0693, // li a3, 9 (DCACHE_CLEAN_BY_VA)
    0x7c55_9073, // csrw DCINCVA, a1
    0x7c36_9073, // csrw DCMAINT, a3
    0x0330_000f, // fence rw, rw
    0x0010_0073, // ebreak
    0x0090_0693, // li a3, 9 (DCACHE_CLEAN_BY_VA)
    0x00c5_2023, // sw a2, 0(a0)
    0x7c55_1073, // csrw DCINCVA, a0
    0x7c36_9073, // csrw DCMAINT, a3
    0x0330_000f, // fence rw, rw
    0x0010_0073, // ebreak
    0x0330_000f, // fence rw, rw
    0xffdf_f06f, // j -4
];
const AP_TO_CPU_OFFSET: u64 = 0;
const CPU_TO_AP_OFFSET: u64 = 48;
const IDLE_OFFSET: u64 = 72;

fn parse_u64(value: &str) -> Result<u64> {
    value
        .strip_prefix("0x")
        .map(|hex| u64::from_str_radix(hex, 16))
        .unwrap_or_else(|| value.parse())
        .map_err(Into::into)
}

fn open_probe(speed: u32) -> Result<Probe> {
    let probes = Lister::new().list_all();
    let mut probe = probes.first().context("no debug probe found")?.open()?;
    probe.select_protocol(WireProtocol::Swd)?;
    probe.set_speed(speed)?;
    Ok(probe)
}

fn attach(chip: &str, speed: u32) -> Result<Session> {
    open_probe(speed)?
        .attach(chip, Permissions::default())
        .map_err(Into::into)
}

fn with_ap<T>(
    session: &mut Session,
    ap: &FullyQualifiedApAddress,
    f: impl FnOnce(&mut dyn ArmMemoryInterface) -> Result<T>,
) -> Result<T> {
    let interface = session.get_arm_interface()?;
    let mut memory = interface.memory_interface(ap)?;
    f(memory.as_mut())
}

fn read_ap(
    session: &mut Session,
    ap: &FullyQualifiedApAddress,
    address: u64,
    data: &mut [u32],
) -> Result<()> {
    with_ap(session, ap, |memory| {
        memory.read_32(address, data).map_err(Into::into)
    })
}

fn write_ap(
    session: &mut Session,
    ap: &FullyQualifiedApAddress,
    address: u64,
    data: &[u32],
) -> Result<()> {
    with_ap(session, ap, |memory| {
        memory.write_32(address, data).map_err(Into::into)
    })
}

fn halt(session: &mut Session) -> Result<()> {
    let mut core = session.core(0)?;
    if !matches!(core.status()?, CoreStatus::Halted(_)) {
        core.halt(Duration::from_secs(1))?;
    }
    Ok(())
}

fn verify_restore(
    session: &mut Session,
    ap: &FullyQualifiedApAddress,
    address: u64,
    original: &[u32],
) -> Result<()> {
    write_ap(session, ap, address, original)?;
    let mut readback = vec![0; original.len()];
    read_ap(session, ap, address, &mut readback)?;
    ensure!(readback == original, "restore mismatch at {address:#010x}");
    Ok(())
}

fn pattern(words: usize, salt: u32) -> Vec<u32> {
    (0..words)
        .map(|index| salt ^ (index as u32).wrapping_mul(0x9e37_79b9))
        .collect()
}

fn wait_halted(session: &mut Session, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        if matches!(session.core(0)?.status()?, CoreStatus::Halted(_)) {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            bail!("CPU test did not halt within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn start_code(session: &mut Session, offset: u64, args: &[u32]) -> Result<()> {
    let mut core = session.core(0)?;
    let registers = core.registers();
    for (index, value) in args.iter().enumerate() {
        core.write_core_reg(registers.argument_register(index), *value)?;
    }
    core.write_core_reg(core.program_counter(), (CODE_ADDRESS + offset) as u32)?;
    core.run()?;
    Ok(())
}

fn running_gate(
    chip: &str,
    speed: u32,
    ap: &FullyQualifiedApAddress,
    scratch: u64,
    sizes: &[usize],
) -> Result<()> {
    ensure!(
        sizes
            .iter()
            .all(|size| *size > 0 && size.is_multiple_of(4) && *size <= SCRATCH_BYTES)
    );
    ensure!(
        CODE_ADDRESS + (TEST_CODE.len() * 4) as u64 <= scratch
            || scratch + SCRATCH_BYTES as u64 <= CODE_ADDRESS
    );

    let mut session = attach(chip, speed)?;
    halt(&mut session)?;
    let mut original = vec![0; SCRATCH_BYTES / 4];
    let mut original_code = vec![0; TEST_CODE.len()];
    read_ap(&mut session, ap, scratch, &mut original)?;
    read_ap(&mut session, ap, CODE_ADDRESS, &mut original_code)?;

    let experiment = (|| -> Result<()> {
        write_ap(&mut session, ap, CODE_ADDRESS, &TEST_CODE)?;
        start_code(&mut session, IDLE_OFFSET, &[])?;
        thread::sleep(Duration::from_millis(10));
        ensure!(
            matches!(session.core(0)?.status()?, CoreStatus::Running),
            "idle test code is not running"
        );

        for (index, bytes) in sizes.iter().copied().enumerate() {
            let values = pattern(bytes / 4, 0x5255_4e00 ^ index as u32);
            let started = Instant::now();
            write_ap(&mut session, ap, scratch, &values)?;
            let elapsed = started.elapsed();
            let mut readback = vec![0; values.len()];
            read_ap(&mut session, ap, scratch, &mut readback)?;
            ensure!(
                readback == values,
                "running AP write/read mismatch for {bytes} bytes"
            );
            write_ap(&mut session, ap, scratch, &original[..values.len()])?;
            println!(
                "running AP1 {bytes} bytes write/read/restore passed in {elapsed:?} ({:.1} KiB/s write)",
                bytes as f64 / 1024.0 / elapsed.as_secs_f64()
            );
        }

        halt(&mut session)?;
        let src = scratch;
        let dst = scratch + 4;
        write_ap(&mut session, ap, src, &[0, 0])?;
        start_code(&mut session, AP_TO_CPU_OFFSET, &[src as u32, dst as u32])?;
        thread::sleep(Duration::from_millis(5));
        write_ap(&mut session, ap, src, &[0x4150_3243])?;
        let started = Instant::now();
        loop {
            let mut observed = [0];
            read_ap(&mut session, ap, dst, &mut observed)?;
            if observed[0] == 0x4150_3243 {
                break;
            }
            let status = session.core(0)?.status()?;
            if matches!(status, CoreStatus::Halted(_))
                || started.elapsed() >= Duration::from_secs(1)
            {
                let mut core = session.core(0)?;
                if !matches!(status, CoreStatus::Halted(_)) {
                    core.halt(Duration::from_secs(1))?;
                }
                let pc: u32 = core.read_core_reg(core.program_counter())?;
                bail!(
                    "CPU did not observe AP1 write: status={status:?}, pc={pc:#010x}, dst={:#010x}",
                    observed[0]
                );
            }
        }
        wait_halted(&mut session, Duration::from_secs(1))?;
        println!("AP1 write -> CPU load/store visibility passed with per-line D-cache invalidate");

        write_ap(&mut session, ap, dst, &[0])?;
        start_code(
            &mut session,
            CPU_TO_AP_OFFSET,
            &[dst as u32, 0, 0x4350_5531],
        )?;
        wait_halted(&mut session, Duration::from_secs(1))?;
        let mut observed = [0];
        read_ap(&mut session, ap, dst, &mut observed)?;
        ensure!(
            observed[0] == 0x4350_5531,
            "AP1 did not observe CPU write: {:#010x}",
            observed[0]
        );
        println!("CPU store -> AP1 read visibility passed with per-line D-cache clean");
        Ok(())
    })();

    let halt_result = halt(&mut session);
    let scratch_restore = verify_restore(&mut session, ap, scratch, &original);
    let code_restore = verify_restore(&mut session, ap, CODE_ADDRESS, &original_code);
    let reboot = session
        .core(0)
        .and_then(|mut core| core.reset())
        .and_then(|_| session.core(0)?.run());

    experiment
        .and(halt_result)
        .and(scratch_restore)
        .and(code_restore)
        .and(reboot.map_err(Into::into))
}

fn reconnect_gate(
    chip: &str,
    speed: u32,
    ap: &FullyQualifiedApAddress,
    scratch: u64,
    cycles: usize,
    hard_reset: bool,
) -> Result<()> {
    ensure!(cycles > 0);
    for cycle in 0..cycles {
        let mut session = attach(chip, speed).with_context(|| format!("attach cycle {cycle}"))?;
        halt(&mut session)?;
        let mut original = vec![0; SCRATCH_BYTES / 4];
        read_ap(&mut session, ap, scratch, &mut original)?;
        let values = pattern(1024, 0x4359_0000 ^ cycle as u32);
        write_ap(&mut session, ap, scratch, &values)?;
        let mut readback = vec![0; values.len()];
        read_ap(&mut session, ap, scratch, &mut readback)?;
        ensure!(readback == values, "cycle {cycle} pattern mismatch");
        verify_restore(&mut session, ap, scratch, &original)?;
        drop(session);

        if hard_reset {
            let mut probe = open_probe(speed)?;
            probe
                .target_reset()
                .with_context(|| format!("nRST cycle {cycle}"))?;
            thread::sleep(Duration::from_millis(20));
        }
        if (cycle + 1).is_multiple_of(10) || cycle + 1 == cycles {
            println!(
                "{} completed {}/{} protected cycles",
                if hard_reset {
                    "nRST/recovery"
                } else {
                    "reconnect"
                },
                cycle + 1,
                cycles
            );
        }
    }
    Ok(())
}

fn reset_during_transfer(
    chip: &str,
    speed: u32,
    ap: &FullyQualifiedApAddress,
    scratch: u64,
) -> Result<()> {
    let mut session = attach(chip, speed)?;
    halt(&mut session)?;
    let mut original = vec![0; SCRATCH_BYTES / 4];
    read_ap(&mut session, ap, scratch, &mut original)?;
    let values = pattern(SCRATCH_BYTES / 4, 0x5253_5400);
    write_ap(&mut session, ap, scratch, &values[..values.len() / 2])?;
    drop(session);

    let mut probe = open_probe(speed)?;
    probe.target_reset()?;
    thread::sleep(Duration::from_millis(20));
    drop(probe);

    let mut recovered = attach(chip, speed).context("reattach after mid-transfer nRST")?;
    halt(&mut recovered)?;
    let mut readback = vec![0; values.len()];
    read_ap(&mut recovered, ap, scratch, &mut readback)?;
    let split = values.len() / 2;
    let committed_words = readback[..split]
        .iter()
        .zip(&values[..split])
        .filter(|(actual, expected)| actual == expected)
        .count();
    let untouched_words = readback[split..]
        .iter()
        .zip(&original[split..])
        .filter(|(actual, expected)| actual == expected)
        .count();

    // RAM contents are explicitly untrusted after reset. Restoration must run
    // regardless of how much of the logical transfer survived.
    verify_restore(&mut recovered, ap, scratch, &original)?;
    println!(
        "mid-logical-transfer nRST retained {committed_words}/{split} committed words and {untouched_words}/{} untouched words; the whole range was treated as invalid and restored after reattach without fallback",
        values.len() - split
    );
    Ok(())
}

fn reset_ack_gate(speed: u32, ap: &FullyQualifiedApAddress, scratch: u64) -> Result<()> {
    let mut probe = open_probe(speed)?;
    probe.attach_to_unspecified()?;
    probe.target_reset_assert()?;
    let interface_result = probe.try_into_arm_debug_interface(DefaultArmSequence::create());
    let (mut probe, access_result) = match interface_result {
        Ok(mut interface) => {
            let result = interface
                .select_debug_port(DpAddress::Default)
                .and_then(|_| {
                    interface
                        .memory_interface(ap)
                        .and_then(|mut memory| memory.read_word_32(scratch))
                })
                .map(|value| format!("AP stayed responsive under nRST: {value:#010x}"))
                .map_err(|error| format!("AP access under nRST returned: {error}"));
            (interface.close(), result)
        }
        Err((probe, error)) => (
            probe,
            Err(format!("ADI initialization under nRST returned: {error}")),
        ),
    };
    probe.target_reset_deassert()?;
    thread::sleep(Duration::from_millis(20));
    drop(probe);
    println!("{}", access_result.unwrap_or_else(|message| message));

    let mut probe = open_probe(speed)?;
    probe.attach_to_unspecified()?;
    let mut interface = probe
        .try_into_arm_debug_interface(DefaultArmSequence::create())
        .map_err(|(_, error)| error)?;
    interface.select_debug_port(DpAddress::Default)?;
    let value = interface.memory_interface(ap)?.read_word_32(scratch)?;
    println!("AP1 recovered after nRST deassert: {value:#010x}");
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: ws63_multi_ap_safety --i-know-this-writes-target <running|reconnect|nrst|reset-transfer|reset-ack> <chip> <ap> <scratch> [cycles|comma-separated-sizes]";
    ensure!(
        args.next().as_deref() == Some("--i-know-this-writes-target"),
        "{usage}"
    );
    let mode = args.next().context(usage)?;
    let chip = args.next().context(usage)?;
    let ap: u8 = args.next().context(usage)?.parse()?;
    let scratch = parse_u64(&args.next().context(usage)?)?;
    ensure!(
        scratch == 0x00a7_0000,
        "this diagnostic is locked to the proven WS63 scratch base 0x00a70000"
    );
    let final_arg = args.next();
    ensure!(args.next().is_none(), "{usage}");
    let speed = std::env::var("PROBE_SPEED")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(2_000);
    let ap = FullyQualifiedApAddress::v1_with_default_dp(ap);

    match mode.as_str() {
        "running" => {
            let sizes = final_arg
                .as_deref()
                .unwrap_or("4096,32768,65536,65532")
                .split(',')
                .map(str::parse)
                .collect::<Result<Vec<_>, _>>()?;
            running_gate(&chip, speed, &ap, scratch, &sizes)
        }
        "reconnect" => reconnect_gate(
            &chip,
            speed,
            &ap,
            scratch,
            final_arg.as_deref().unwrap_or("100").parse()?,
            false,
        ),
        "nrst" => reconnect_gate(
            &chip,
            speed,
            &ap,
            scratch,
            final_arg.as_deref().unwrap_or("100").parse()?,
            true,
        ),
        "reset-transfer" => {
            ensure!(final_arg.is_none(), "{usage}");
            reset_during_transfer(&chip, speed, &ap, scratch)
        }
        "reset-ack" => {
            ensure!(final_arg.is_none(), "{usage}");
            reset_ack_gate(speed, &ap, scratch)
        }
        _ => Err(anyhow!(usage)),
    }
}
