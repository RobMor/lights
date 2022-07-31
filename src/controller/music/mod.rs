use anyhow::{anyhow, Context, Result};
use num_complex::Complex;
use num_traits::Zero;
use rustfft::{algorithm::Radix4, Fft, FftDirection};
use std::f64::consts::PI;
use std::ops::Range;
use std::time::Duration;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::controller::Controller;
use crate::color::{Color, NUM_LIGHTS, OFF};
use crate::color::cmap::INFERNO_DATA;

mod snap;

use snap::client::SnapClient;

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

const BAS_FREQ_LOW: f64 = 1.0;
const BAS_FREQ_HIGH: f64 = 600.0;

const MID_FREQ_LOW: f64 = 500.0;
const MID_FREQ_HIGH: f64 = 2_500.0;

const TRE_FREQ_LOW: f64 = 2_000.0;
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
const BAS_EQ: f64 = 1.0 / 5_000.0;
const MID_EQ: f64 = 1.0 / 1_500.0;
const TRE_EQ: f64 = 1.0 / 200.0;

pub struct MusicController {
    frame: Arc<Mutex<Option<Vec<i32>>>>,
    current_color: [Color; NUM_LIGHTS],
    ticks_since_new_frame: usize,

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

impl MusicController {
    pub fn start() -> Self {
        let frame = Arc::new(Mutex::new(None));

        tokio::spawn(run(frame.clone()));

        MusicController {
            frame,
            current_color: OFF,
            ticks_since_new_frame: usize::MAX,

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
        }
    }

    fn has_new_frame(&self) -> bool {
        self.frame.blocking_lock().is_some()
    }

    fn get_new_frame(&mut self) -> Option<Vec<i32>> {
        // We are using a (possibly innefficient) mutex for this.
        // Maybe there's a faster way of implementing a single reader single writer
        // optional value.
        self.frame.blocking_lock().take()
    }

    fn process_frame(&mut self, frame: Vec<i32>) -> [Color; NUM_LIGHTS] {
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

            // Apply gravity
            state.velocity -= GRAVITY;

            // Did this new value make us move up?
            if val > state.val {
                // How fast should we move up?
                // This is an arbitrary formula that just seems to work well...
                state.velocity = (val - state.val).sqrt();
            }

            state.val += state.velocity;

            // Apply scaling
            // TODO scale based on mean and stddev
            state.clamped_val = state.val.clamp(0.0, 255.0) as u8;
        }

        let mut colors = OFF;

        for (color, state) in colors.iter_mut().zip(self.spectrum_state.iter()) {
            let mapped = INFERNO_DATA[state.clamped_val as usize];

            *color = Color {
                i: state.clamped_val,
                r: (mapped[0] * 255.0) as u8,
                g: (mapped[1] * 255.0) as u8,
                b: (mapped[2] * 255.0) as u8,
            }
        }

        colors
    }
}

impl Controller for MusicController {
    fn is_active(&self) -> bool {
        // Snapcast produces some empty frames for a while after the music stops.
        // This buffer is more than enough.
        // However if our display is moving faster than our new frames we don't want to give up access.
        self.has_new_frame() || self.ticks_since_new_frame < 10
    }

    fn tick(&mut self) -> [Color; NUM_LIGHTS] {
        match self.get_new_frame() {
            Some(frame) => {
                self.ticks_since_new_frame = 0;
                self.current_color = self.process_frame(frame)
            },
            None => self.ticks_since_new_frame += 1
        }

        self.current_color
    }
}

async fn run(mut output: Arc<Mutex<Option<Vec<i32>>>>) -> Result<()> {
    let mut retries = 0;
    loop {
        log::info!("Connecting to SnapServer");
        match SnapClient::discover().await {
            Ok(client) => {
                retries = 0;

                log::info!("Successfully connected to SnapServer");

                // TODO different actions for different mainloop errors
                match mainloop(client, &mut output).await {
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

async fn mainloop(mut client: SnapClient, output: &mut Arc<Mutex<Option<Vec<i32>>>>) -> Result<()> {
    while let Some(frame) = client.next().await.context("Error retrieving packet from snapclient")? {
        log::trace!("Received frame from SnapServer");
        
        output.lock().await.replace(frame);
    }

    Err(anyhow!("SnapClient connection unexpectedly closed, attempting to reconnect"))
}