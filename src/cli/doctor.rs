//! `walkie doctor` — check this machine.
//!
//! The product is sent to friends as a bare binary. When it does not work, this
//! is the only diagnostic they have. It must be honest about latency rather
//! than reassuring.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::{DevicePreference, Engine, NullTx, device};
use crate::config::{
    DEVICE_BUFFER_FRAMES, FRAME_MS, JITTER_START_FRAMES, MAX_PACKET_BYTES, OPUS_BITRATE,
    OPUS_COMPLEXITY, SAMPLE_RATE,
};
use crate::error::Result;
use crate::store::{Store, identity};

use super::fmt::{Table, box_line};

/// Runs the checks.
pub fn run(loopback: bool, tune: bool) -> Result<()> {
    println!();
    println!("{}", box_line::label("WALKIE DOCTOR"));

    let mut faults = Vec::new();

    identity_check(&mut faults)?;
    device_check(&mut faults);
    codec_check(&mut faults);
    let engine = engine_check(&mut faults);

    if loopback {
        loopback_check(engine.as_ref(), &mut faults);
    }

    budget_report(engine.as_ref());

    if tune {
        tune_report();
    }

    if let Some(engine) = engine {
        engine.shutdown();
    }

    println!();
    if faults.is_empty() {
        println!("{}", box_line::label("NO FAULTS"));
        println!();
    } else {
        println!("{}", box_line::label("FAULTS"));
        println!();
        for fault in &faults {
            println!("  - {fault}");
        }
        println!();
    }

    Ok(())
}

fn identity_check(faults: &mut Vec<String>) -> Result<()> {
    println!();
    println!("  IDENTITY");

    match Store::open() {
        Ok(store) => {
            let me = store.identity(&identity::default_name())?;
            println!("    name       {}", me.name);
            println!("    key        {}", me.endpoint_id().fmt_short());
            println!("    contacts   {}", store.contacts()?.len());
            println!("    waiting    {}", store.pending_knocks()?.len());
        }
        Err(e) => {
            println!("    cannot open the local database");
            faults.push(format!("the database will not open: {e}"));
        }
    }
    Ok(())
}

fn device_check(faults: &mut Vec<String>) {
    println!();
    println!("  DEVICES");
    println!();

    let all = device::describe_all();
    if all.is_empty() {
        faults.push("this machine reports no audio devices".into());
        println!("    none found");
        return;
    }

    let mut table = Table::new(["DIR", "DEVICE", "48kHz"]);
    for (direction, name, native) in &all {
        table.row([
            match direction {
                device::Direction::Input => "in".into(),
                device::Direction::Output => "out".into(),
            },
            name.clone(),
            if *native { "yes".into() } else { "NO".into() },
        ]);
    }
    table.print("    ");

    for (direction, label) in [
        (device::Direction::Input, "microphone"),
        (device::Direction::Output, "speaker"),
    ] {
        match device::choose(direction, None) {
            Ok(chosen) => {
                println!();
                println!("    default {label}");
                println!("      name      {}", chosen.name);
                println!("      rate      {} Hz", chosen.config.sample_rate);
                println!("      channels  {}", chosen.config.channels);
                println!("      buffer    {:.1} ms", chosen.buffer_ms());

                if !chosen.native_rate {
                    faults.push(format!(
                        "the default {label} does not run at 48 kHz, so the audio path \
                         must resample and the latency budget no longer holds"
                    ));
                }
                match chosen.echo {
                    device::EchoRisk::Likely => faults.push(
                        "the default output is a loudspeaker. Version 1 has no echo canceller, \
                         so the far end will hear themselves. Use headphones."
                            .into(),
                    ),
                    device::EchoRisk::Unlikely => println!("      echo      unlikely"),
                    device::EchoRisk::Unknown => {
                        println!("      echo      unknown, so use headphones to be sure")
                    }
                }
            }
            Err(e) => {
                println!();
                println!("    default {label}: none ({e})");
                faults.push(format!("there is no usable {label}"));
            }
        }
    }
}

fn codec_check(faults: &mut Vec<String>) {
    println!();
    println!("  CODEC");

    let mut encoder =
        match opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Audio) {
            Ok(e) => e,
            Err(e) => {
                faults.push(format!("Opus will not start: {e}"));
                return;
            }
        };

    let _ = encoder.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE));
    let _ = encoder.set_complexity(OPUS_COMPLEXITY);
    let _ = encoder.set_inband_fec(true);

    let frame: Vec<f32> = (0..crate::config::FRAME_SAMPLES)
        .map(|i| (i as f32 * 0.05).sin() * 0.4)
        .collect();
    let mut out = vec![0u8; MAX_PACKET_BYTES];

    // Measure the encode cost. It sits inside the input callback, so a slow
    // machine shows up here rather than as mystery dropouts.
    let runs = 200;
    let started = Instant::now();
    let mut bytes = 0;
    for _ in 0..runs {
        bytes = encoder.encode_float(&frame, &mut out).unwrap_or(0);
    }
    let per_frame = started.elapsed() / runs;

    println!(
        "    frame      {FRAME_MS} ms, {} samples",
        crate::config::FRAME_SAMPLES
    );
    println!("    packet     {bytes} bytes");
    println!("    bitrate    {} kbit/s payload", bytes * 8 * 100 / 1000);
    println!(
        "    encode     {:.2} ms per frame",
        per_frame.as_secs_f32() * 1000.0
    );

    let budget = Duration::from_millis(FRAME_MS as u64);
    if per_frame > budget / 4 {
        faults.push(format!(
            "encoding one frame takes {:.2} ms against a {FRAME_MS} ms budget. \
             Lower OPUS_COMPLEXITY.",
            per_frame.as_secs_f32() * 1000.0
        ));
    }
}

fn engine_check(faults: &mut Vec<String>) -> Option<Arc<Engine>> {
    println!();
    println!("  ENGINE");

    let engine = match Engine::start(Arc::new(NullTx), DevicePreference::default()) {
        Ok(e) => e,
        Err(e) => {
            faults.push(format!("the audio engine will not start: {e}"));
            return None;
        }
    };

    // Arming opens the microphone. This is the step that triggers the macOS
    // permission prompt on a first run.
    engine.arm();
    std::thread::sleep(Duration::from_millis(700));

    let state = engine.state();
    println!("    state      {state:?}");

    match engine.report.lock().ok().and_then(|g| g.clone()) {
        Some(report) => {
            println!("    input      {}", report.input_name);
            println!("    output     {}", report.output_name);
            println!(
                "    buffers    {:.1} ms in, {:.1} ms out",
                report.input_buffer_ms, report.output_buffer_ms
            );
        }
        None => {
            faults.push(
                "the microphone did not open. On a first run, grant the microphone \
                 permission and try again."
                    .into(),
            );
        }
    }

    // Confirm the callback is actually running by watching the counters move.
    engine.set_transmitting(true);
    std::thread::sleep(Duration::from_millis(400));
    let stats = engine.stats();
    engine.set_transmitting(false);

    println!("    encoded    {} frames", stats.encoded);
    if stats.encoded == 0 {
        faults
            .push("no audio frames were captured. The microphone permission may be denied.".into());
    }
    if stats.encode_errors > 0 {
        faults.push(format!("{} frames failed to encode", stats.encode_errors));
    }

    Some(engine)
}

/// Measures real mouth-to-ear delay through the device stack.
fn loopback_check(engine: Option<&Arc<Engine>>, faults: &mut Vec<String>) {
    println!();
    println!("  LOOPBACK");

    let Some(engine) = engine else {
        faults.push("the loopback test needs a working engine".into());
        return;
    };

    println!("    Put the microphone next to the speaker, then stay quiet.");
    println!("    Measuring for 3 seconds.");

    engine.arm();
    engine.set_transmitting(true);

    let started = Instant::now();
    let mut input_peak = 0f32;
    while started.elapsed() < Duration::from_secs(3) {
        input_peak = input_peak.max(engine.input_level());
        std::thread::sleep(Duration::from_millis(10));
    }
    engine.set_transmitting(false);

    println!("    input peak {input_peak:.3}");

    if input_peak < 0.001 {
        faults.push(
            "the microphone captured silence. Check the input device and the permission.".into(),
        );
    }

    // T-103 completes this: it must play a click, record it, and correlate the
    // two to get a real number. Reporting a guess would be worse than
    // reporting nothing.
    println!("    T-103 adds the click and correlation step.");
}

fn budget_report(engine: Option<&Arc<Engine>>) {
    println!();
    println!("  LATENCY BUDGET");
    println!();

    let report = engine
        .and_then(|e| e.report.lock().ok())
        .and_then(|g| g.clone());

    let input_ms = report
        .as_ref()
        .map(|r| r.input_buffer_ms)
        .unwrap_or(DEVICE_BUFFER_FRAMES as f32 * 1000.0 / SAMPLE_RATE as f32);
    let output_ms = report
        .as_ref()
        .map(|r| r.output_buffer_ms)
        .unwrap_or(DEVICE_BUFFER_FRAMES as f32 * 1000.0 / SAMPLE_RATE as f32);
    let jitter_ms = (JITTER_START_FRAMES * FRAME_MS as usize) as f32;

    let mut table = Table::new(["STAGE", "MS"]);
    table.row(["input buffer".into(), format!("{input_ms:.1}")]);
    table.row(["frame".into(), format!("{:.1}", FRAME_MS as f32)]);
    table.row(["encode".into(), "0.3".into()]);
    table.row(["network".into(), "rtt / 2".into()]);
    table.row(["jitter buffer".into(), format!("{jitter_ms:.1}")]);
    table.row(["decode".into(), "0.1".into()]);
    table.row(["output buffer".into(), format!("{output_ms:.1}")]);
    table.print("    ");

    let fixed = input_ms + FRAME_MS as f32 + 0.3 + jitter_ms + 0.1 + output_ms;
    println!();
    println!("    fixed total  {fixed:.1} ms, plus half the round trip time");
}

fn tune_report() {
    println!();
    println!("  TUNING");
    println!();
    println!("    These trade safety for latency. Change them in src/config.rs.");
    println!();
    println!("    DEVICE_BUFFER_FRAMES  128   saves 2.7 ms, risks dropouts under load");
    println!("    FRAME_MS              5     saves 5 ms, doubles the packet rate");
    println!("    JITTER_MIN_FRAMES     1     saves 10 ms, only safe on a LAN");
    println!();
    println!("    T-104 makes this measure the link and recommend values.");
}
