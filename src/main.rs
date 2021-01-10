use anyhow::Result;
use log::{info};
use num_complex::Complex;
use num_traits::Zero;
use rustfft::FFTplanner;
// use simple_logger::SimpleLogger;
use std::{f64::consts::PI, ops::Range};
// use tokio::fs::{File, OpenOptions};
// use tokio::io::{AsyncWriteExt, BufWriter};

mod client;
mod protocol;

use client::SnapClient;

const FIFO_PATH: &'static str = "/tmp/cava";

/// The number of samples per second (aka Hz)
const SAMPLE_RATE: usize = 44100; // TODO get this from the server

/// The number of bars we want to show
const NUM_BARS: usize = 10;
// const LOWER_CUTOFF: usize = 50;
// const UPPER_CUTOFF: usize = 10000;

const BASS_BUFFER_SIZE: usize = 8192; // Might be pretty extra...
const MID_BUFFER_SIZE: usize = 4096;
const TREB_BUFFER_SIZE: usize = 2048;

/// Each FFT bin should be this big (in hertz)
const BASS_BIN_SIZE: f64 = SAMPLE_RATE as f64 / BASS_BUFFER_SIZE as f64;
// const MID_BIN_SIZE: usize = SAMPLE_RATE / MID_BUFFER_SIZE;
// const TREB_BIN_SIZE: usize = SAMPLE_RATE / TREB_BUFFER_SIZE;

#[tokio::main]
async fn main() -> Result<()> {
    // Cava used this, not sure how it works
    // let frequency_constant: f64 =
    //     (LOWER_CUTOFF as f64 / UPPER_CUTOFF as f64).log10() / ((1.0 / (NUM_BARS + 1) as f64) - 1.0);

    // let BASS_BAR_CUTOFF = 4;
    // let MID_BAR_CUTOFF = 6;

    // SimpleLogger::new().init().unwrap();


    let BASS_RANGE = ((20.0 / BASS_BIN_SIZE).round() as usize..(250.0 / BASS_BIN_SIZE).round() as usize);
    let MID_RANGE = ((250.0 / BASS_BIN_SIZE).round() as usize..(4_000.0 / BASS_BIN_SIZE).round() as usize);
    let TREB_RANGE = ((4_000.0 / BASS_BIN_SIZE).round() as usize..(20_000.0 / BASS_BIN_SIZE).round() as usize);

    let BASS_EQ = 80.0 / 1_000_000.0;
    let MID_EQ = 80.0 / 100_000.0;
    let TREB_EQ = 80.0 / 100_000.0;

    // Hann Windows
    let bass_window: Vec<f64> = (0..BASS_BUFFER_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (BASS_BUFFER_SIZE - 1) as f64).cos()))
        .collect();
    let mid_window: Vec<f64> = (0..MID_BUFFER_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (MID_BUFFER_SIZE - 1) as f64).cos()))
        .collect();
    let treb_window: Vec<f64> = (0..TREB_BUFFER_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (TREB_BUFFER_SIZE - 1) as f64).cos()))
        .collect();

    // if let Err(_) = nix::unistd::access(FIFO_PATH, nix::unistd::AccessFlags::F_OK) {
    //     nix::unistd::mkfifo(FIFO_PATH, nix::sys::stat::Mode::from_bits(0o664).unwrap())?;
    // }

    // info!("Here");

    // let mut fifo = BufWriter::new(OpenOptions::new().write(true).create(true).mode(0o644).open(FIFO_PATH).await?);

    // info!("Here");

    // let mut samples = BufWriter::new(File::create("samples.txt").await?);
    // let mut bass_file = BufWriter::new(File::create("bass_freqs.txt").await?);
    // let mut treb_file = BufWriter::new(File::create("treb_freqs.txt").await?);

    // Sample buffer of BASS buffer size since it's the biggest
    let mut buf = [Complex::zero(); BASS_BUFFER_SIZE];

    // We need separate buffers because the FFT mutates the buffer
    let mut bass_fft_buf = [Complex::zero(); BASS_BUFFER_SIZE];
    let mut bass_out_buf = [Complex::zero(); BASS_BUFFER_SIZE];
    let mut mid_fft_buf = [Complex::zero(); MID_BUFFER_SIZE];
    let mut mid_out_buf = [Complex::zero(); MID_BUFFER_SIZE];
    let mut treb_fft_buf = [Complex::zero(); TREB_BUFFER_SIZE];
    let mut treb_out_buf = [Complex::zero(); TREB_BUFFER_SIZE];

    let mut planner = FFTplanner::new(false);

    let bass_fft = planner.plan_fft(BASS_BUFFER_SIZE);
    let mid_fft = planner.plan_fft(MID_BUFFER_SIZE);
    let treb_fft = planner.plan_fft(TREB_BUFFER_SIZE);

    info!("Connecting to SnapServer");
    let mut client = SnapClient::init().await?;
    info!("Successfully connected to SnapServer");

    while let Some(frame) = client.next().await? {
        let in_buf: Vec<Complex<f64>> = frame
            .iter()
            .rev() // Most recent samples first
            .map(|x| Complex::new(*x as f64, 0.0))
            .collect();

        // Push the new samples onto our buffers
        buf.rotate_right(frame.len()); // TODO not maximally efficient
        buf[..frame.len()].copy_from_slice(&in_buf); // TODO this is also not maximally efficient

        // Copy samples into FFT buffers
        bass_fft_buf.copy_from_slice(&buf[..BASS_BUFFER_SIZE]);
        mid_fft_buf.copy_from_slice(&buf[..MID_BUFFER_SIZE]);
        treb_fft_buf.copy_from_slice(&buf[..TREB_BUFFER_SIZE]);

        // if buf[buf.len() - 1].re != 0.0 {
        //     for sample in buf.iter() {
        //         samples.write(format!("{}\n", sample.re).as_bytes()).await?;
        //     }

        //     samples.write(b"-----\n").await?;
        // }

        // Apply hann windowing
        // TODO could do this in the same step as copy_from_slice
        bass_fft_buf
            .iter_mut()
            .enumerate()
            .for_each(|(i, e)| *e *= bass_window[i]);
        mid_fft_buf
            .iter_mut()
            .enumerate()
            .for_each(|(i, e)| *e *= mid_window[i]);
        treb_fft_buf
            .iter_mut()
            .enumerate()
            .for_each(|(i, e)| *e *= treb_window[i]);

        // Perform the FFT
        bass_fft.process(&mut bass_fft_buf, &mut bass_out_buf);
        mid_fft.process(&mut mid_fft_buf, &mut mid_out_buf);
        treb_fft.process(&mut treb_fft_buf, &mut treb_out_buf);

        let freqs = bass_out_buf
            .iter()
            .map(|x| x.norm())
            .take(BASS_BUFFER_SIZE / 2)
            .enumerate()
            .collect::<Vec<(usize, f64)>>();

        // if buf[buf.len() - 1].re != 0.0 {
        //     for (i, freq) in freqs.iter() {
        //         bass_file.write(format!("{}\n", freq).as_bytes()).await?;
        //     }

        //     bass_file.write(b"-----\n").await?;
        // }

        // let ffreqs = treb_out_buf.iter()
        //     .map(|x| x.norm())
        //     .take(TREB_BUFFER_SIZE / 2)
        //     .enumerate()
        //     .collect::<Vec<(usize, f64)>>();

        // if buf[buf.len() - 1].re != 0.0 {
        //     for (i, freq) in ffreqs.iter() {
        //         treb_file.write(format!("{}\n", freq).as_bytes()).await?;
        //     }

        //     treb_file.write(b"-----\n").await?;
        // }

        let bass = (freqs[BASS_RANGE.clone()].iter().map(|(_, s)| s).sum::<f64>() / BASS_RANGE.len() as f64 * BASS_EQ).min(255.0) as usize;
        let mid = (freqs[MID_RANGE.clone()].iter().map(|(_, s)| s).sum::<f64>() / MID_RANGE.len() as f64 * MID_EQ).min(255.0) as usize;
        let treb = (freqs[TREB_RANGE.clone()].iter().map(|(_, s)| s).sum::<f64>() / TREB_RANGE.len() as f64 * TREB_EQ).min(255.0) as usize;
        
        println!("({},{},{})", bass, mid, treb);

        // freqs.sort_by(|(_,x), (_,y)| x.partial_cmp(y).unwrap().reverse());

        // 250 HZ -> BIN: 39.. - 40.. FREQS: 19... - 20...
        // 400 HZ -> BIN: 146 FREQS: 735-740
        // 800 HZ -> BIN: 300 FREQS: 1505-1510
        // 1000 HZ -> BIN: 369 FREQS: 1850-1855
        // 9000 HZ -> BIN: 3342 FREQS: 16715-16720

        // for (i, f) in freqs.iter().take(5) {
        //     let low_freq = BASS_BIN_SIZE * (i + 1);
        //     let high_freq = BASS_BIN_SIZE * (i + 2);
        //     println!("BIN: {}  FREQS: {}-{} INTENSITY: {}", i, low_freq, high_freq, f);
        // }

        // let mut max: f64 = 0.0;
        // for bar in 0..NUM_BARS {
        //     let bar_coefficient = (((bar + 1) as f64 / (NUM_BARS + 1) as f64) - 1.0) * FREQUENCY_CONSTANT;
        //     let cutoff = UPPER_CUTOFF as f64 * (10 as f64).powf(bar_coefficient) / (SAMPLE_RATE as f64 / 2.0);
        //     let relative_cutoff = cutoff / (SAMPLE_RATE as f64 / 2.0);

        //     let avg = {
        //         if bar < 1 { // Bass bars
        //             let lower = relative_cutoff * (BASS_BUFFER_SIZE as f64 / 2.0);
        //             let upper = lower

        //             bass_out_buf[lower..upper].iter().map(|x| x.norm()).sum::<f64>() / (upper - lower) as f64
        //         } else if bar < 2 { // Mid bars
        //             let lower = relative_cutoff * (MID_BUFFER_SIZE as f64 / 2.0);

        //             mid_out_buf[lower..upper].iter().map(|x| x.norm()).sum::<f64>() / (upper - lower) as f64
        //         } else { // Treble bars
        //             let lower = relative_cutoff * (TREB_BUFFER_SIZE as f64 / 2.0);

        //             treb_out_buf[lower..upper].iter().map(|x| x.norm()).sum::<f64>() / (upper - lower) as f64
        //         }
        //     };

        //     println!("{}", avg);

        //     if avg > max {
        //         max = avg;
        //     }

        //     let height = ((avg / max) * 80.0) as usize;

        //     println!("{}", std::iter::repeat("X").take(height).collect::<String>());
        // }

        // println!("")
    }

    info!("Stream closed");

    Ok(())
}

// TODO internalize the fact that local librespot is just playing directly to the output on the pi

// TODO make it so the client can just be awaited on for music samples...
