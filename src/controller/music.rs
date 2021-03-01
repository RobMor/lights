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

/// The number of times we try reconnecting to the snapserver before giving up
const NUM_RETRIES: usize = 5;
/// The number of samples per second (aka Hz)
const SAMPLE_RATE: usize = 44100; // TODO get this from the server
/// The number of audio samples we keep from frame to frame and use for FFT
const BUFFER_SIZE: usize = 4096; // TODO making this 8092 caused stack overflows...
/// The size of each FFT bin in Hz
const BIN_SIZE: f64 = SAMPLE_RATE as f64 / BUFFER_SIZE as f64;
/// The rate at which each bar decreases (positive means down)
const GRAVITY: f64 = 9.8; // TODO find the right value

// EQ values to balance out each set of frequencies
// TODO make these dynamic in some way
const BAS_EQ: f64 = 80.0 / 1_000_000.0;
const MID_EQ: f64 = 80.0 / 500_000.0;
const TRE_EQ: f64 = 80.0 / 10_000.0;

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
    val: f64,
    prev_val: f64,
    falling_ticks: u8,
    eq: f64,
    freq_range: Range<usize>,
}

impl SpectrumState {
    fn new(range: Range<usize>, eq: f64) -> SpectrumState {
        SpectrumState {
            val: 0.0,
            prev_val: 0.0,
            falling_ticks: 0,
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
                        (20.0 / BIN_SIZE).round() as usize..(250.0 / BIN_SIZE).round() as usize,
                        BAS_EQ,
                    ), // BASS
                    SpectrumState::new(
                        (250.0 / BIN_SIZE).round() as usize..(4_000.0 / BIN_SIZE).round() as usize,
                        MID_EQ,
                    ), // MID
                    SpectrumState::new(
                        (4_000.0 / BIN_SIZE).round() as usize..(20_000.0 / BIN_SIZE).round() as usize,
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
        let mut sender: Option<mpsc::Sender<[[u8; 3]; NUM_LIGHTS]>> = None;

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

    async fn process_frame(&mut self, frame: Vec<i32>) -> Result<[[u8; 3]; 3]> {
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
            state.val = freqs[state.freq_range.clone()].iter().sum::<f64>() / state.freq_range.len() as f64;

            // Apply EQ
            state.val = state.val * state.eq;

            // Apply smoothing
            // TODO might not be necessary with three bars

            // Apply gravity
            state.prev_val = state.prev_val - (GRAVITY * state.falling_ticks.pow(2) as f64);
            state.val = if state.val < state.prev_val {
                // log::debug!("[{}] Kept falling (val: {}, prev_val: {}, falling ticks: {})", i, state.val, state.prev_val, state.falling_ticks);
                state.falling_ticks += 1;
                state.prev_val
            } else {
                // log::debug!("[{}] Stopped falling (val: {}, prev_val: {}, falling ticks: {})", i, state.val, state.prev_val, state.falling_ticks);
                state.prev_val = state.val;
                state.falling_ticks = 0;
                state.val
            };

            // Apply scaling
            // TODO scale based on mean and stddev
            state.val = state.val.clamp(0.0, 255.0);
        }

        let mut colors = [[0; 3]; 3];

        for (color, state) in colors.iter_mut().zip(self.spectrum_state.iter()) {
            let mapped = INFERNO_DATA[state.val as usize];

            *color = [(mapped[0] * 255.0) as u8, (mapped[1] * 255.0) as u8, (mapped[2] * 255.0) as u8];
        }

        Ok(colors)
    }
}
