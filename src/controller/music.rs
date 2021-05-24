use anyhow::{anyhow, Context, Result};
use num_complex::Complex;
use num_traits::Zero;
use rustfft::{algorithm::Radix4, Fft, FftDirection};
use std::f64::consts::PI;
use std::ops::Range;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;

use crate::snap::client::SnapClient;
use crate::controller::{InMessage, OutMessage, Token};
use crate::cmap::INFERNO_DATA;
use crate::NUM_LIGHTS;
use crate::Color;

/// The number of times we try reconnecting to the snapserver before giving up
const NUM_RETRIES: usize = 5;
/// The number of samples per second (aka Hz)
const SAMPLE_RATE: usize = 44100; // TODO get this from the server
/// The number of audio samples we keep from frame to frame and use for FFT
const BUFFER_SIZE: usize = 4096; // TODO making this 8092 caused stack overflows...
/// The size of each FFT bin in Hz
const BIN_SIZE: f64 = SAMPLE_RATE as f64 / BUFFER_SIZE as f64;
/// The rate at which each bar decreases (positive means down)
const GRAVITY: f64 = 1.0; // TODO find the right value

const INTEGRAL: f64 = 0.77; // TODO

const BAS_FREQ_LOW: f64 = 20.0;
const BAS_FREQ_HIGH: f64 = 500.0;

const MID_FREQ_LOW: f64 = 250.0;
const MID_FREQ_HIGH: f64 = 2_500.0;

const TRE_FREQ_LOW: f64 = 2_500.0;
const TRE_FREQ_HIGH: f64 = 20_000.0;

// TODO these don't need to be constants...
const BAS_INDEX_LOW: f64 = BAS_FREQ_LOW / BIN_SIZE;
const BAS_INDEX_HIGH: f64 = BAS_FREQ_HIGH / BIN_SIZE;

const MID_INDEX_LOW: f64 = MID_FREQ_LOW / BIN_SIZE;
const MID_INDEX_HIGH: f64 = MID_FREQ_HIGH / BIN_SIZE;

const TRE_INDEX_LOW: f64 = TRE_FREQ_LOW / BIN_SIZE;
const TRE_INDEX_HIGH: f64 = TRE_FREQ_HIGH / BIN_SIZE;

// EQ values to balance out each set of frequencies
// TODO make these dynamic in some way
const BAS_EQ: f64 = 1.0 / 8_000.0;
const MID_EQ: f64 = 1.0 / 1_000.0;
const TRE_EQ: f64 = 1.0 / 200.0;

pub struct MusicController {
    token: Token,
    rx: mpsc::Receiver<InMessage>,
    tx: mpsc::Sender<(Token, OutMessage)>,

    hann_window: Vec<f64>,
    fft: Radix4<f64>,

    buf: [Complex<f64>; BUFFER_SIZE],
    fft_buf: [Complex<f64>; BUFFER_SIZE],
    fft_scratch: [Complex<f64>; BUFFER_SIZE],
    spectrum_state: [SpectrumState; NUM_LIGHTS],
}

#[derive(Debug)]
struct SpectrumState {
    // Working vars
    clamped_val: u8,
    val: f64,
    velocity: f64,

    sensitivity: f64,
    high_ticks: u8,
    low_ticks: u8,
    
    // Constants
    eq: f64,
    freq_range: Range<usize>,
}

impl SpectrumState {
    fn new(range: Range<usize>, eq: f64) -> SpectrumState {
        SpectrumState {
            clamped_val: 0,
            val: 0.0,
            velocity: 0.0,

            sensitivity: 1.0,
            high_ticks: 0,
            low_ticks: 0,

            eq: eq,
            freq_range: range,
        }
    }
}

// Need some way of giving one controller access at a time
// while still being responsive to other requests.

// Ownership of the transmission stream seems like a good way
// to guarantee unique access but it would also make the entire
// app pretty fragile to one failed component.

impl MusicController {
    pub fn start(
        token: Token,
        rx: mpsc::Receiver<InMessage>,
        tx: mpsc::Sender<(Token, OutMessage)>,
    ) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let controller = MusicController {
                token,
                rx,
                tx,

                hann_window: (0..BUFFER_SIZE)
                    .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (BUFFER_SIZE - 1) as f64).cos()))
                    .collect(),
                fft: Radix4::new(BUFFER_SIZE, FftDirection::Forward),

                buf: [Complex::zero(); BUFFER_SIZE],
                fft_buf: [Complex::zero(); BUFFER_SIZE],
                fft_scratch: [Complex::zero(); BUFFER_SIZE],

                // TODO dynamic frequency ranges...
                spectrum_state: [
                    SpectrumState::new(
                        BAS_INDEX_LOW.round() as usize..BAS_INDEX_HIGH.round() as usize,
                        BAS_EQ,
                    ), // BASS
                    SpectrumState::new(
                        MID_INDEX_LOW.round() as usize..MID_INDEX_HIGH.round() as usize,
                        MID_EQ,
                    ), // MID
                    SpectrumState::new(
                        TRE_INDEX_LOW.round() as usize..TRE_INDEX_HIGH.round() as usize,
                        TRE_EQ,
                    ), // TREBLE
                ],
            };

            log::info!("Starting Music Controller with token {:?}", token);

            controller.run().await
        })
    }

    async fn run(mut self) -> Result<()> {
        let mut retries = 0;
        loop {
            log::info!("Connecting to SnapServer");
            match SnapClient::discover().await {
                Ok(client) => {
                    retries = 0;

                    log::info!("Successfully connected to SnapServer");

                    // TODO different actions for different mainloop errors
                    match self.mainloop(client).await {
                        Ok(()) => return Ok(()),
                        Err(e) => {
                            log::error!("Error in mainloop: {}", e);
                        }
                    }
                }
                Err(e) => {
                    retries += 1;
                    // TODO different actions for different errors
                    log::error!("Error connecting to SnapServer (Attempt #{}): {}", retries, e);
                }
            }

            if retries > NUM_RETRIES {
                return Err(anyhow!("Failed to connect to SnapServer after {} attempts", retries));
            }

            log::info!("Sleeping 5 seconds before attempting to connect to SnapServer");
            sleep(Duration::from_secs(5)).await;
        }
    }

    async fn mainloop(&mut self, mut client: SnapClient) -> Result<()> {
        let mut has_requested_access = false;
        let mut sender: Option<mpsc::Sender<[Color; NUM_LIGHTS]>> = None;

        loop {
            tokio::select! {
                // TODO make client.next() more robust
                // TODO add functionality to notice when music starts playing
                result = client.next() => {
                    match result.context("Error retrieving packet from snapclient")? {
                        Some(frame) => {
                            log::trace!("Received frame from SnapServer");

                            if let Some(sender) = sender.as_ref() {
                                log::trace!("Processing frame from SnapServer");

                                match self.process_frame(frame).await {
                                    Ok(colors) => {
                                        match sender.send(colors).await {
                                            Ok(()) => {},
                                            Err(e) => {
                                                log::error!("Error setting color: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        log::error!("Error processing frame: {}", e);
                                    },
                                }
                            } else if !has_requested_access {
                                log::debug!("Requesting access to lights channel");
                                // TODO more sophisticated is-playing detection
                                self.tx.send((self.token, OutMessage::RequestAccess)).await?;
                            }
                        },
                        None => {
                            return Err(anyhow!("SnapClient connection unexpectedly closed, attempting to reconnect"));
                        }
                    }
                },
                result = self.rx.recv() => {
                    match result {
                        Some(InMessage::GrantAccess(s)) => {
                            log::debug!("Received access to lights channel");
                            sender = Some(s);
                            has_requested_access = false;
                        },
                        Some(InMessage::RevokeAccess) => {
                            if let Some(sender) = sender.take() {
                                log::debug!("Rescinding access to lights channel");
                                self.tx.send((self.token, OutMessage::RescindAccess(sender))).await?
                            }

                            has_requested_access = false;
                        },
                        None => return Ok(()), // TODO maybe we don't need Stop
                    }
                }
            }
        }
    }

    async fn process_frame(&mut self, frame: Vec<i32>) -> Result<[Color; NUM_LIGHTS]> {
        // TODO could do this in the same step as copying it to the buffer and save memory
        let in_buf: Vec<Complex<f64>> = frame
            .iter()
            .rev() // Most recent samples first
            .map(|x| Complex::new(*x as f64, 0.0))
            .collect();

        // Push the new samples onto our buffers
        let left_over = self.buf.len() - frame.len();
        self.buf.copy_within(0..left_over, frame.len());
        self.buf[..frame.len()].copy_from_slice(&in_buf);

        // Copy samples into FFT buffer
        self.fft_buf.copy_from_slice(&self.buf[..BUFFER_SIZE]);

        // Apply hann windowing
        // TODO could do this in the same step as copy_from_slice
        // TODO is this necessary anymore?
        self.fft_buf
            .iter_mut()
            .zip(self.hann_window.iter())
            .for_each(|(v, h)| *v *= h);

        // Perform the FFT
        self.fft.process_with_scratch(&mut self.fft_buf, &mut self.fft_scratch);

        let freqs = self
            .fft_buf
            .iter()
            .take(BUFFER_SIZE / 2)
            .map(|x| x.norm())
            .collect::<Vec<f64>>();

        // Iterate through different spectrum bars
        for (i, state) in self.spectrum_state.iter_mut().enumerate() {
            // Average the range of frequencies
            let mut val = freqs[state.freq_range.clone()].iter().sum::<f64>() / state.freq_range.len() as f64;

            // Apply EQ
            val = val * state.eq;

            // // Apply sensitivity
            // val = val * state.sensitivity;

            // // Adjust sensitivity if values are too high or low for too long
            // // TODO all these thresholds are arbitrary!!
            // // TODO this reacts poorly to silence!!!
            // if state.val > 5.0 && state.val < 75.0 {
            //     state.high_ticks = state.high_ticks.saturating_sub(10);
            //     state.low_ticks = state.low_ticks.saturating_add(1);

            //     if state.low_ticks > 10 {
            //         state.sensitivity *= 1.01;
            //         state.low_ticks = state.low_ticks.saturating_sub(1);
            //         log::info!("Increased sens {} {}", i, state.sensitivity);
            //     }
            // } else if state.val > 175.0 {
            //     state.low_ticks = state.low_ticks.saturating_sub(10);
            //     state.high_ticks = state.high_ticks.saturating_add(1);

            //     if state.high_ticks > 10 {
            //         state.sensitivity *= 0.98;
            //         state.high_ticks = state.low_ticks.saturating_sub(1);
            //         log::info!("Decreased sens {} {}", i, state.sensitivity);
            //     }
            // } else {
            //     state.high_ticks = state.high_ticks.saturating_sub(10);
            //     state.low_ticks = state.low_ticks.saturating_sub(10);
            // }

            // Apply gravity
            state.velocity -= GRAVITY;

            // Did this new value make us move up?
            if val < state.val {
                // No
                // Do nothing
            } else {
                // Yes
                state.velocity = 0.0; // Stop falling

                // How fast should we move up?
                // This is an arbitrary formula that just seems to work well...
                state.velocity += (val - state.val).sqrt();
            }

            state.val += state.velocity;

            // Apply scaling
            // TODO scale based on mean and stddev
            state.clamped_val = state.val.clamp(0.0, 255.0) as u8;
        }

        let mut colors = [(0, [0; 3]); 3];
        // let mut colors = [0; 3];

        for ((intensity, color), state) in colors.iter_mut().zip(self.spectrum_state.iter()) {
            let mapped = INFERNO_DATA[state.clamped_val as usize];

            *intensity = state.clamped_val;
            *color = [(mapped[0] * 255.0) as u8, (mapped[1] * 255.0) as u8, (mapped[2] * 255.0) as u8];
            // *color = state.val as u8;
        }

        Ok(colors)
    }
}
