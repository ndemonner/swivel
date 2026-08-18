//! The audio engine.
//!
//! Read `ARCHITECTURE.md` §5 before changing anything here. The two rules that
//! matter most:
//!
//! 1. The CoreAudio callbacks are real-time. No allocation, no locks, no input
//!    or output, no logging.
//! 2. Every millisecond added here is a millisecond the user hears.

pub mod capture;
pub mod chirp;
pub mod device;
pub mod jitter;
pub mod packet;
pub mod playback;
pub mod resample;
pub mod vpio;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use iroh::EndpointId;
use tracing::{info, warn};

use crate::config::MAX_PEERS;
use crate::error::{Error, Result};
use crate::net::audio_wire::AudioPacket;

pub use chirp::Chirp;
use chirp::ChirpPlayer;
use jitter::Estimator;
use packet::{Packet, SlotTable};

/// Where received audio goes.
///
/// The network layer calls this from a tokio task. The implementation copies
/// the payload into a lock-free queue that the output callback drains. It never
/// blocks and never touches the callback's own state.
pub trait AudioSink: Send + Sync {
    /// Delivers one parsed datagram from a peer.
    fn deliver(&self, peer: EndpointId, packet: &AudioPacket<'_>);

    /// Marks a peer as a member of the live session, so its audio is mixed.
    ///
    /// Returns false when every peer slot is in use.
    fn activate(&self, peer: EndpointId) -> bool;

    /// Removes a peer from the live session.
    fn deactivate(&self, peer: EndpointId);
}

/// How encoded audio leaves the machine.
///
/// The application implements this over the warm peer connections. The audio
/// layer does not know what a connection is.
pub trait AudioTx: Send + Sync {
    /// Sends one encoded frame to every member of the live session.
    ///
    /// Returns how many peers it reached. It must not block.
    fn send_frame(&self, wire: &[u8]) -> usize;
}

/// An audio sink that drops everything. Used by tests and by `swivel doctor`.
#[derive(Debug, Default)]
pub struct NullSink;

impl AudioSink for NullSink {
    fn deliver(&self, _peer: EndpointId, _packet: &AudioPacket<'_>) {}
    fn activate(&self, _peer: EndpointId) -> bool {
        true
    }
    fn deactivate(&self, _peer: EndpointId) {}
}

/// A transmitter that drops everything.
#[derive(Debug, Default)]
pub struct NullTx;

impl AudioTx for NullTx {
    fn send_frame(&self, _wire: &[u8]) -> usize {
        0
    }
}

/// What the engine is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EngineState {
    /// Nothing is running. Reaching this state after start up means a fault.
    Idle = 0,
    /// The speaker is open, so a contact can be heard. The microphone is shut.
    Listening = 1,
    /// The microphone is open and transmitting.
    Live = 2,
}

/// The devices the user chose, by name.
///
/// Empty means "use the system default". The audio thread reads this whenever
/// it opens a device, so a change takes effect on the next open.
#[derive(Debug, Clone, Default)]
pub struct DevicePreference {
    pub input: Option<String>,
    pub output: Option<String>,
}

/// A command for the audio thread.
///
/// `cpal::Stream` is not `Send` on macOS, so the streams live on one thread and
/// everything else talks to it through this channel.
enum Command {
    /// Open the microphone without transmitting.
    ///
    /// Arming ahead of time is what makes a talk session start instantly. A
    /// CoreAudio device takes tens of milliseconds to start, which would
    /// otherwise clip the first word. The panel arms on open, so the cost is
    /// paid while the user is still choosing who to talk to.
    Arm {
        /// Fresh read sides for the peer queues.
        ///
        /// Starting the voice unit replaces the whole audio path, and the
        /// output callback owns the read side of every queue, so a new path
        /// needs a new table. The caller swaps the table before sending this,
        /// so anything it activates afterwards lands on the new one.
        consumers: Vec<packet::PacketConsumer>,
    },
    /// Start transmitting. The microphone must already be armed.
    Transmit(bool),
    /// Close the microphone.
    ///
    /// The speaker is never closed. This is an intercom: a contact must be able
    /// to reach you at any moment, so the output stream runs for the life of
    /// the process.
    Disarm {
        consumers: Vec<packet::PacketConsumer>,
    },
    Chirp(Chirp),
    /// Close and reopen the devices, picking up a changed preference.
    Reopen {
        consumers: Vec<packet::PacketConsumer>,
    },
    Shutdown,
}

/// The audio engine handle.
///
/// Cloning is cheap. Dropping the last one stops the audio thread.
pub struct Engine {
    commands: crossbeam_channel::Sender<Command>,
    estimators: Arc<std::sync::Mutex<Vec<Estimator>>>,
    capture: Arc<capture::CaptureShared>,
    chirps: Arc<ChirpPlayer>,
    /// The report from the last device open. Used by `swivel doctor`.
    pub report: Arc<std::sync::Mutex<Option<DeviceReport>>>,
    state: Arc<std::sync::atomic::AtomicU8>,
    /// True while the input device is open, whether or not it is transmitting.
    ///
    /// `state` cannot answer this. An armed but silent microphone still reports
    /// `Listening`, and the difference is exactly what the idle timer needs.
    armed: Arc<AtomicBool>,
    /// The chosen devices. Read by the audio thread on every open.
    preference: Arc<std::sync::Mutex<DevicePreference>>,
    /// The peer slots.
    ///
    /// This is swapped whenever the audio path is rebuilt. The output callback
    /// owns the read side of every packet queue, so a new path needs new
    /// queues, and therefore a new table. Everything else reads it through the
    /// swap.
    slots: Arc<arc_swap::ArcSwap<SlotTable>>,
    /// True while the voice unit is running and cancelling echo.
    cancelling: Arc<AtomicBool>,
    /// Whether echo cancellation should be used at all.
    want_cancelling: Arc<AtomicBool>,
}

/// What the engine found when it opened the devices.
#[derive(Debug, Clone)]
pub struct DeviceReport {
    pub input_name: String,
    pub output_name: String,
    pub input_buffer_ms: f32,
    pub output_buffer_ms: f32,
    pub native_rate: bool,
    /// Whether the far end is likely to hear itself.
    pub echo: device::EchoRisk,
}

impl Engine {
    /// Starts the audio thread. The devices stay closed until `arm` is called.
    pub fn start(
        tx: Arc<dyn AudioTx>,
        preference: DevicePreference,
        echo_cancellation: bool,
    ) -> Result<Arc<Self>> {
        let (slot_table, consumers) = SlotTable::new();
        let preference = Arc::new(std::sync::Mutex::new(preference));
        let slots: Arc<arc_swap::ArcSwap<SlotTable>> =
            Arc::new(arc_swap::ArcSwap::new(Arc::new(slot_table)));
        let chirps = Arc::new(ChirpPlayer::new());
        let report = Arc::new(std::sync::Mutex::new(None));
        let state = Arc::new(std::sync::atomic::AtomicU8::new(EngineState::Idle as u8));
        let armed = Arc::new(AtomicBool::new(false));
        let cancelling = Arc::new(AtomicBool::new(false));
        let want_cancelling = Arc::new(AtomicBool::new(echo_cancellation));

        let estimators = Arc::new(std::sync::Mutex::new(
            (0..MAX_PEERS).map(|_| Estimator::new()).collect::<Vec<_>>(),
        ));

        let (commands, rx) = crossbeam_channel::unbounded();

        // A placeholder until the thread reports the real capture state.
        let capture_probe = Arc::new(std::sync::Mutex::new(None));

        {
            let slots = slots.clone();
            let chirps = chirps.clone();
            let report = report.clone();
            let capture_probe = capture_probe.clone();
            let state = state.clone();
            let armed = armed.clone();
            let preference = preference.clone();
            let slots = slots.clone();
            let cancelling = cancelling.clone();
            let want_cancelling = want_cancelling.clone();

            std::thread::Builder::new()
                .name("swivel-audio".into())
                .spawn(move || {
                    audio_thread(
                        rx,
                        ThreadContext {
                            slots,
                            consumers,
                            chirps,
                            tx,
                            report,
                            capture_probe,
                            state,
                            armed,
                            preference,
                            cancelling,
                            want_cancelling,
                        },
                    );
                })
                .map_err(|e| Error::Audio(format!("cannot start the audio thread: {e}")))?;
        }

        // Wait briefly for the thread to publish its capture state. It is
        // created before any device is opened, so this is fast.
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let capture = loop {
            if let Ok(guard) = capture_probe.lock()
                && let Some(shared) = guard.as_ref()
            {
                break shared.clone();
            }
            if Instant::now() > deadline {
                return Err(Error::Audio("the audio thread did not start".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        Ok(Arc::new(Engine {
            commands,
            slots,
            preference,
            estimators,
            capture,
            chirps,
            report,
            state,
            armed,
            cancelling,
            want_cancelling,
        }))
    }

    /// Opens both devices without transmitting.
    ///
    /// Call this when the user opens the panel, before any contact is chosen.
    /// The device start cost is then paid while the user is still deciding.
    pub fn arm(&self) {
        let _ = self.commands.send(Command::Arm {
            consumers: self.swap_slots(),
        });
    }

    /// Closes the microphone and returns to listening only.
    pub fn disarm(&self) {
        let _ = self.commands.send(Command::Disarm {
            consumers: self.swap_slots(),
        });
    }

    /// Installs a fresh slot table and returns the read sides for the audio
    /// thread.
    ///
    /// This runs on the caller's thread, before the command is sent, so that a
    /// caller which activates a peer straight afterwards lands on the new
    /// table rather than the one about to be thrown away.
    fn swap_slots(&self) -> Vec<packet::PacketConsumer> {
        let (table, consumers) = SlotTable::new();
        self.slots.store(Arc::new(table));
        consumers
    }

    /// True when the audio path is cancelling echo.
    pub fn echo_cancelling(&self) -> bool {
        self.cancelling.load(Ordering::Acquire)
    }

    /// Turns echo cancellation on or off. It takes effect on the next
    /// conversation.
    pub fn set_echo_cancellation(&self, on: bool) {
        self.want_cancelling.store(on, Ordering::Release);
    }

    /// True when echo cancellation is wanted, whether or not it is running.
    pub fn wants_echo_cancellation(&self) -> bool {
        self.want_cancelling.load(Ordering::Acquire)
    }

    /// Opens or closes the microphone. The devices must already be armed.
    pub fn set_transmitting(&self, on: bool) {
        let _ = self.commands.send(Command::Transmit(on));
    }

    /// True when the microphone is transmitting.
    pub fn transmitting(&self) -> bool {
        self.capture.transmitting.load(Ordering::Relaxed)
    }

    /// True while the input device is open, transmitting or not.
    pub fn armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    /// Plays a confirmation tone locally.
    pub fn chirp(&self, chirp: Chirp) {
        let _ = self.commands.send(Command::Chirp(chirp));
    }

    /// The local microphone level, from 0.0 to 1.0.
    pub fn input_level(&self) -> f32 {
        self.capture.peak()
    }

    /// True while the local voice is above the speaking threshold.
    pub fn speaking(&self) -> bool {
        self.capture.speaking.load(Ordering::Relaxed)
    }

    /// The slot table, for the session manager.
    ///
    /// Reading it is a pointer swap. Hold the result only for as long as one
    /// operation, because a device change replaces the table.
    pub fn slots(&self) -> Arc<SlotTable> {
        self.slots.load_full()
    }

    /// Replaces the chosen devices and reopens.
    ///
    /// The output stream is rebuilt, which replaces the slot table, so the
    /// caller must re-activate whoever is in the session afterwards.
    pub fn set_devices(&self, preference: DevicePreference) {
        if let Ok(mut guard) = self.preference.lock() {
            *guard = preference;
        }
        let _ = self.commands.send(Command::Reopen {
            consumers: self.swap_slots(),
        });
    }

    /// The chosen devices.
    pub fn devices(&self) -> DevicePreference {
        self.preference
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    /// Counters for `swivel doctor`.
    pub fn stats(&self) -> Stats {
        let mut played = 0;
        let mut concealed = 0;
        let mut late = 0;
        let mut overrun = 0;

        let slots = self.slots();
        for index in 0..MAX_PEERS {
            let slot = slots.slot(index);
            played += slot.played.load(Ordering::Relaxed);
            concealed += slot.concealed.load(Ordering::Relaxed);
            late += slot.late.load(Ordering::Relaxed);
            overrun += slot.overrun.load(Ordering::Relaxed);
        }

        Stats {
            encoded: self.capture.encoded.load(Ordering::Relaxed),
            send_dropped: self.capture.dropped.load(Ordering::Relaxed),
            encode_errors: self.capture.encode_errors.load(Ordering::Relaxed),
            played,
            concealed,
            late,
            overrun,
        }
    }

    /// What the engine is doing now.
    pub fn state(&self) -> EngineState {
        match self.state.load(Ordering::Acquire) {
            x if x == EngineState::Live as u8 => EngineState::Live,
            x if x == EngineState::Listening as u8 => EngineState::Listening,
            _ => EngineState::Idle,
        }
    }

    /// True while a confirmation tone is sounding.
    pub fn chirping(&self) -> bool {
        self.chirps.is_playing()
    }

    /// Stops the audio thread and closes the devices.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// Counters that describe how the stream is behaving.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub encoded: u64,
    pub send_dropped: u64,
    pub encode_errors: u64,
    /// Frames decoded and heard. The positive signal that audio is arriving.
    pub played: u64,
    pub concealed: u64,
    pub late: u64,
    pub overrun: u64,
}

impl AudioSink for Engine {
    fn deliver(&self, peer: EndpointId, packet: &AudioPacket<'_>) {
        let slots = self.slots();
        let Some(index) = slots.index_of(peer) else {
            // The peer is not in the session. Their audio is not wanted, and
            // dropping it here keeps it out of the mix entirely.
            return;
        };

        let Some(stored) = Packet::from_wire(packet) else {
            return;
        };

        let slot = slots.slot(index);

        // Measure arrival spacing here, on the network task, where reading the
        // clock is safe. The callback only reads the published target.
        if let Ok(mut estimators) = self.estimators.lock()
            && let Some(estimator) = estimators.get_mut(index)
        {
            estimator.on_arrival(packet.seq, Instant::now(), &slot.target_frames);
        }

        slot.push(stored);
    }

    fn activate(&self, peer: EndpointId) -> bool {
        let slots = self.slots();
        match slots.activate(peer) {
            Some(index) => {
                if let Ok(mut estimators) = self.estimators.lock()
                    && let Some(estimator) = estimators.get_mut(index)
                {
                    estimator.reset(&slots.slot(index).target_frames);
                }
                true
            }
            None => {
                warn!("every audio slot is in use, so a peer could not be added");
                false
            }
        }
    }

    fn deactivate(&self, peer: EndpointId) {
        self.slots().deactivate(peer);
    }
}

/// Owns the cpal streams for the life of the process.
///
/// `cpal::Stream` is not `Send` on macOS, so a stream can never leave this
/// function. Everything else reaches the devices through the command channel.
///
/// The speaker opens once and stays open. The microphone opens and closes.
/// That asymmetry is deliberate:
///
/// - A contact must be able to reach you at any moment, so the output stream
///   can never be closed.
/// - The macOS microphone indicator should mean something. If the input stream
///   ran all the time, the indicator would be permanently lit and would stop
///   telling the user anything.
fn audio_thread(commands: crossbeam_channel::Receiver<Command>, ctx: ThreadContext) {
    let ThreadContext {
        slots,
        consumers,
        chirps,
        tx,
        report,
        capture_probe,
        state,
        armed,
        preference,
        cancelling,
        want_cancelling,
    } = ctx;

    // The shared capture state outlives any single device open, so its counters
    // survive an arm and disarm cycle.
    let shared = Arc::new(capture::CaptureShared::new_shared());
    if let Ok(mut guard) = capture_probe.lock() {
        *guard = Some(shared.clone());
    }

    // Open the speaker now and keep it open.
    let mut speaker: Option<playback::Playback> = None;
    let mut output_report: Option<(String, f32, bool, device::EchoRisk)> = None;

    open_speaker(
        &slots,
        consumers,
        &chirps,
        &preference,
        &state,
        &mut speaker,
        &mut output_report,
    );

    if speaker.is_none() {
        warn!("swivel is running without audio output. Run `swivel doctor`.");
    }

    // The audio path is in one of two shapes.
    //
    //   Listening: a plain output stream. The speaker is open and the
    //   microphone is shut, which is the idle state of an intercom.
    //
    //   Talking: one Voice Processing unit doing both directions. It cancels
    //   echo, which it can only do by owning the speaker as well, because the
    //   canceller needs the signal that was played in order to subtract it.
    //
    // Switching shape replaces the whole path, which is why every command that
    // switches carries a fresh set of queue consumers.
    let mut microphone: Option<OpenMicrophone> = None;
    let mut voice: Option<vpio::VoiceUnit> = None;

    while let Ok(command) = commands.recv() {
        match command {
            Command::Arm { consumers } => {
                if voice.is_some() || microphone.is_some() {
                    // Already talking. The caller swapped the table, so the
                    // path has to be rebuilt on the new one regardless.
                    drop(voice.take());
                    if let Some(mic) = microphone.take() {
                        mic.close();
                    }
                }
                drop(speaker.take());

                let want = want_cancelling.load(Ordering::Acquire);
                let outcome = start_talking(
                    want,
                    consumers,
                    &slots,
                    &chirps,
                    &shared,
                    &preference,
                    &tx,
                    &mut voice,
                    &mut microphone,
                    &mut speaker,
                    &mut output_report,
                    &report,
                    &state,
                );

                match outcome {
                    Ok(cancelled) => {
                        cancelling.store(cancelled, Ordering::Release);
                        armed.store(true, Ordering::Release);
                        state.store(EngineState::Listening as u8, Ordering::Release);
                    }
                    Err(e) => {
                        warn!("cannot open the microphone: {e}");
                        cancelling.store(false, Ordering::Release);
                        chirps.play(Chirp::Fault);
                    }
                }
            }

            Command::Transmit(on) => {
                shared.transmitting.store(on, Ordering::Relaxed);
                state.store(
                    if on {
                        EngineState::Live as u8
                    } else if speaker.is_some() || voice.is_some() {
                        EngineState::Listening as u8
                    } else {
                        EngineState::Idle as u8
                    },
                    Ordering::Release,
                );
            }

            Command::Disarm { consumers } => {
                shared.transmitting.store(false, Ordering::Relaxed);

                drop(voice.take());
                if let Some(mic) = microphone.take() {
                    mic.close();
                }
                drop(speaker.take());

                cancelling.store(false, Ordering::Release);
                armed.store(false, Ordering::Release);

                // Back to listening. The speaker never stays shut, because a
                // contact must be able to reach this machine at any moment.
                open_speaker(
                    &slots,
                    consumers,
                    &chirps,
                    &preference,
                    &state,
                    &mut speaker,
                    &mut output_report,
                );
            }

            Command::Chirp(chirp) => chirps.play(chirp),

            Command::Reopen { consumers } => {
                let was_talking = voice.is_some() || microphone.is_some();

                drop(voice.take());
                if let Some(mic) = microphone.take() {
                    mic.close();
                }
                drop(speaker.take());

                if was_talking {
                    let want = want_cancelling.load(Ordering::Acquire);
                    match start_talking(
                        want,
                        consumers,
                        &slots,
                        &chirps,
                        &shared,
                        &preference,
                        &tx,
                        &mut voice,
                        &mut microphone,
                        &mut speaker,
                        &mut output_report,
                        &report,
                        &state,
                    ) {
                        Ok(cancelled) => cancelling.store(cancelled, Ordering::Release),
                        Err(e) => {
                            warn!("cannot reopen the microphone: {e}");
                            cancelling.store(false, Ordering::Release);
                        }
                    }
                } else {
                    cancelling.store(false, Ordering::Release);
                    open_speaker(
                        &slots,
                        consumers,
                        &chirps,
                        &preference,
                        &state,
                        &mut speaker,
                        &mut output_report,
                    );
                }

                info!("reopened the audio devices");
            }

            Command::Shutdown => {
                shared.transmitting.store(false, Ordering::Relaxed);
                drop(voice.take());
                if let Some(mic) = microphone.take() {
                    mic.close();
                }
                armed.store(false, Ordering::Release);
                cancelling.store(false, Ordering::Release);
                slots.load().clear();
                drop(speaker);
                state.store(EngineState::Idle as u8, Ordering::Release);
                return;
            }
        }
    }
}

/// Opens both directions for a conversation.
///
/// Returns whether echo cancellation is running.
///
/// The voice unit is tried first, and the plain pair of streams is the fallback.
/// A machine where the voice unit will not start still gets a working
/// conversation; it just echoes on speakers. Refusing to talk at all would be
/// a worse answer.
#[allow(clippy::too_many_arguments)]
fn start_talking(
    want_cancelling: bool,
    consumers: Vec<packet::PacketConsumer>,
    slots: &Arc<arc_swap::ArcSwap<SlotTable>>,
    chirps: &Arc<ChirpPlayer>,
    shared: &Arc<capture::CaptureShared>,
    preference: &Arc<std::sync::Mutex<DevicePreference>>,
    tx: &Arc<dyn AudioTx>,
    voice: &mut Option<vpio::VoiceUnit>,
    microphone: &mut Option<OpenMicrophone>,
    speaker: &mut Option<playback::Playback>,
    output_report: &mut Option<(String, f32, bool, device::EchoRisk)>,
    report: &Arc<std::sync::Mutex<Option<DeviceReport>>>,
    state: &Arc<std::sync::atomic::AtomicU8>,
) -> Result<bool> {
    let wanted = preference.lock().map(|p| p.clone()).unwrap_or_default();

    if want_cancelling {
        let (frames_producer, frames) =
            ringbuf::traits::Split::split(ringbuf::HeapRb::<capture::OutgoingFrame>::new(
                crate::config::PEER_QUEUE_PACKETS,
            ));

        let mixer = playback::Mixer::new(slots.load_full(), consumers, chirps.clone());
        let encoder = capture::Encoder::new(shared.clone(), frames_producer);

        match (mixer, encoder) {
            (Ok(mixer), Ok(encoder)) => {
                match vpio::start(vpio::resolve(&wanted), mixer, encoder, shared.clone()) {
                    Ok(unit) => {
                        *voice = Some(unit);
                        *microphone = Some(OpenMicrophone::sender_only(frames, tx.clone())?);

                        let input_name = wanted
                            .input
                            .clone()
                            .or_else(|| device::default_name(device::Direction::Input))
                            .unwrap_or_else(|| "voice unit".into());
                        let output_name = wanted
                            .output
                            .clone()
                            .or_else(|| device::default_name(device::Direction::Output))
                            .unwrap_or_else(|| "voice unit".into());

                        *output_report =
                            Some((output_name.clone(), 0.0, true, device::EchoRisk::Cancelled));

                        if let Ok(mut guard) = report.lock() {
                            *guard = Some(DeviceReport {
                                input_name,
                                output_name,
                                // The voice unit does not expose its buffer
                                // size. Reporting a guess would be worse than
                                // reporting nothing.
                                input_buffer_ms: f32::NAN,
                                output_buffer_ms: f32::NAN,
                                native_rate: unit_at_48k(voice.as_ref()),
                                echo: device::EchoRisk::Cancelled,
                            });
                        }

                        state.store(EngineState::Listening as u8, Ordering::Release);
                        return Ok(true);
                    }
                    Err(e) => {
                        warn!(
                            "the voice unit would not start, so this conversation has no echo \
                             cancellation: {e}"
                        );
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                warn!("cannot build the voice path: {e}");
            }
        }

        // The consumers were moved into the failed mixer, so the caller has to
        // start again with a fresh table.
        return Err(Error::Audio(
            "the voice unit failed and its queues went with it".into(),
        ));
    }

    // Plain pair of streams: speaker open, microphone open, no cancellation.
    open_speaker(
        slots,
        consumers,
        chirps,
        preference,
        state,
        speaker,
        output_report,
    );
    let (mic, input_summary) = open_microphone(shared, tx.clone(), preference)?;
    *microphone = Some(mic);

    if let Ok(mut guard) = report.lock() {
        *guard = build_report(&input_summary, output_report.as_ref());
    }

    Ok(false)
}

/// True when the voice unit settled on 48 kHz on both sides.
fn unit_at_48k(unit: Option<&vpio::VoiceUnit>) -> bool {
    unit.map(|u| {
        u.input_rate == crate::config::SAMPLE_RATE && u.output_rate == crate::config::SAMPLE_RATE
    })
    .unwrap_or(false)
}

/// What the audio thread needs to run. Bundled so the signature stays legible.
struct ThreadContext {
    slots: Arc<arc_swap::ArcSwap<SlotTable>>,
    consumers: Vec<packet::PacketConsumer>,
    chirps: Arc<ChirpPlayer>,
    tx: Arc<dyn AudioTx>,
    report: Arc<std::sync::Mutex<Option<DeviceReport>>>,
    capture_probe: Arc<std::sync::Mutex<Option<Arc<capture::CaptureShared>>>>,
    state: Arc<std::sync::atomic::AtomicU8>,
    armed: Arc<AtomicBool>,
    preference: Arc<std::sync::Mutex<DevicePreference>>,
    cancelling: Arc<AtomicBool>,
    want_cancelling: Arc<AtomicBool>,
}

/// Opens the speaker and records what was chosen.
///
/// A machine with no usable output still runs. Seeing who is online is useful
/// even when nothing can be heard, and the fault is reported by `doctor`.
#[allow(clippy::too_many_arguments)]
fn open_speaker(
    slots: &Arc<arc_swap::ArcSwap<SlotTable>>,
    consumers: Vec<packet::PacketConsumer>,
    chirps: &Arc<ChirpPlayer>,
    preference: &Arc<std::sync::Mutex<DevicePreference>>,
    state: &Arc<std::sync::atomic::AtomicU8>,
    speaker: &mut Option<playback::Playback>,
    report: &mut Option<(String, f32, bool, device::EchoRisk)>,
) {
    let wanted = preference.lock().ok().and_then(|p| p.output.clone());

    match device::choose(device::Direction::Output, wanted.as_deref()) {
        Ok(output) => {
            let summary = (
                output.name.clone(),
                output.buffer_ms(),
                output.native_rate,
                output.echo,
            );
            match playback::open(&output, slots.load_full(), consumers, chirps.clone()) {
                Ok(p) => {
                    *speaker = Some(p);
                    *report = Some(summary);
                    state.store(EngineState::Listening as u8, Ordering::Release);
                }
                Err(e) => warn!("cannot open the speaker: {e}"),
            }
        }
        Err(e) => warn!("cannot choose a speaker: {e}"),
    }
}

/// The microphone while it is open.
struct OpenMicrophone {
    /// `None` when the voice unit owns the microphone instead of `cpal`.
    _capture: Option<capture::Capture>,
    sender: Option<std::thread::JoinHandle<()>>,
    sender_running: Arc<AtomicBool>,
}

impl OpenMicrophone {
    /// Builds the sender thread only, for the voice unit path.
    ///
    /// The voice unit owns the microphone itself, so there is no `cpal` stream
    /// here. What is still needed is the thread that carries encoded frames to
    /// the network.
    fn sender_only(
        frames: ringbuf::HeapCons<capture::OutgoingFrame>,
        tx: Arc<dyn AudioTx>,
    ) -> Result<Self> {
        let sender_running = Arc::new(AtomicBool::new(true));
        let running = sender_running.clone();

        let sender = std::thread::Builder::new()
            .name("swivel-audio-tx".into())
            .spawn(move || capture::sender_loop(frames, tx, running))
            .map_err(|e| Error::Audio(format!("cannot start the sender thread: {e}")))?;

        Ok(OpenMicrophone {
            _capture: None,
            sender: Some(sender),
            sender_running,
        })
    }

    fn close(mut self) {
        self.sender_running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.sender.take() {
            // The sender parks between frames. Wake it so it sees the flag
            // rather than waiting out its timeout.
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// Opens the microphone and starts the thread that sends encoded frames.
fn open_microphone(
    shared: &Arc<capture::CaptureShared>,
    tx: Arc<dyn AudioTx>,
    preference: &Arc<std::sync::Mutex<DevicePreference>>,
) -> Result<(OpenMicrophone, (String, f32, bool))> {
    let wanted = preference.lock().ok().and_then(|p| p.input.clone());
    let input = device::choose(device::Direction::Input, wanted.as_deref())?;
    let summary = (input.name.clone(), input.buffer_ms(), input.native_rate);

    let (capture, frames) = capture::open(&input, shared.clone())?;

    let sender_running = Arc::new(AtomicBool::new(true));
    let sender = {
        let running = sender_running.clone();
        std::thread::Builder::new()
            .name("swivel-audio-tx".into())
            .spawn(move || capture::sender_loop(frames, tx, running))
            .map_err(|e| Error::Audio(format!("cannot start the sender thread: {e}")))?
    };

    Ok((
        OpenMicrophone {
            _capture: Some(capture),
            sender: Some(sender),
            sender_running,
        },
        summary,
    ))
}

fn build_report(
    input: &(String, f32, bool),
    output: Option<&(String, f32, bool, device::EchoRisk)>,
) -> Option<DeviceReport> {
    let (output_name, output_buffer_ms, output_native, echo) = output?;

    Some(DeviceReport {
        input_name: input.0.clone(),
        output_name: output_name.clone(),
        input_buffer_ms: input.1,
        output_buffer_ms: *output_buffer_ms,
        native_rate: input.2 && *output_native,
        echo: *echo,
    })
}

/// Logs what the engine found, once, at start up.
pub fn log_report(report: &DeviceReport) {
    info!(
        input = %report.input_name,
        output = %report.output_name,
        "audio devices open"
    );

    if !report.native_rate {
        warn!(
            "a device does not run at 48 kHz. The pipeline must resample, and \
             the latency budget no longer holds."
        );
    }
    if report.echo == device::EchoRisk::Likely {
        warn!("the output is a loudspeaker. Use headphones, or the far end hears an echo.");
    }
}
