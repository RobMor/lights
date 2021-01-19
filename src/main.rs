use anyhow::Result;
use log::info;
use num_complex::Complex;
use num_traits::Zero;
use rustfft::FFTplanner;
// use simple_logger::SimpleLogger;
use std::{f64::consts::PI, ops::Range};
// use tokio::fs::{File, OpenOptions};
// use tokio::io::{AsyncWriteExt, BufWriter};
use rs_ws281x::{ControllerBuilder, ChannelBuilder, StripType};
use scarlet::{prelude::*, colormap::{ColorMap, ListedColorMap}};

mod client;
mod protocol;

use client::SnapClient;

/// The number of samples per second (aka Hz)
const SAMPLE_RATE: usize = 44100; // TODO get this from the server

const BUFFER_SIZE: usize = 8192; // Might be pretty extra...

/// Each FFT bin should be this big (in hertz)
const BIN_SIZE: f64 = SAMPLE_RATE as f64 / BUFFER_SIZE as f64;

#[tokio::main]
async fn main() -> Result<()> {
    // SimpleLogger::new().init().unwrap();

    info!("Connecting to SnapServer");
    let mut client = SnapClient::discover().await?;
    info!("Successfully connected to SnapServer");


    let mut controller = ControllerBuilder::new()
        .channel(
            0,
            ChannelBuilder::new()
                .pin(18)
                .count(36)
                .strip_type(StripType::Ws2811Rgb)
                .brightness(255)
                .build()
        )
        .build()?;

    let cmap = ListedColorMap::inferno();

    
    let BAS_RANGE = ((20.0 / BIN_SIZE).round() as usize..(250.0 / BIN_SIZE).round() as usize);
    let MID_RANGE = ((250.0 / BIN_SIZE).round() as usize..(4_000.0 / BIN_SIZE).round() as usize);
    let TRE_RANGE = ((4_000.0 / BIN_SIZE).round() as usize..(20_000.0 / BIN_SIZE).round() as usize);

    let BAS_EQ = 80.0 / 1_000_000.0;
    let MID_EQ = 80.0 / 500_000.0;
    let TRE_EQ = 80.0 / 100_000.0;

    let gravity: f64 = 9.8; // TODO

    let mut bas_prev: f64 = 0.0;
    let mut mid_prev: f64 = 0.0;
    let mut tre_prev: f64 = 0.0;

    let mut bas_time: f64 = 0.0;
    let mut mid_time: f64 = 0.0;
    let mut tre_time: f64 = 0.0;

    let hann_window: Vec<f64> = (0..BUFFER_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (BUFFER_SIZE - 1) as f64).cos()))
        .collect();

    // Sample buffer of BASS buffer size since it's the biggest
    let mut buf = [Complex::zero(); BUFFER_SIZE];

    // We need separate buffers because the FFT mutates the buffer
    let mut fft_buf = [Complex::zero(); BUFFER_SIZE];
    let mut out_buf = [Complex::zero(); BUFFER_SIZE];

    let mut planner = FFTplanner::new(false);

    let fft = planner.plan_fft(BUFFER_SIZE);

    while let Some(frame) = client.next().await? {
        //                  [Pipeline]
        //
        //                 Raw Audio Data
        //                       V
        //                 Hann Windowing
        //                       V
        //              Fast Fourier Transform
        //                       V
        // Take the average value of each range of frequencies
        //                       V
        //                   Apply EQ
        //                       V
        //                Apply smoothing
        //                       V
        //                 Apply Gravity
        //                       V
        //                 Apply Scaling
        //                       V
        //                 Map to colors

        let in_buf: Vec<Complex<f64>> = frame
            .iter()
            .rev() // Most recent samples first
            .map(|x| Complex::new(*x as f64, 0.0))
            .collect();

        // Push the new samples onto our buffers
        buf.rotate_right(frame.len()); // TODO not maximally efficient
        buf[..frame.len()].copy_from_slice(&in_buf); // TODO this is also not maximally efficient

        // Copy samples into FFT buffer
        fft_buf.copy_from_slice(&buf[..BUFFER_SIZE]);

        // Apply hann windowing
        // TODO could do this in the same step as copy_from_slice
        // TODO is this necessary anymore?
        fft_buf
            .iter_mut()
            .enumerate()
            .for_each(|(i, e)| *e *= hann_window[i]);

        // Perform the FFT
        fft.process(&mut fft_buf, &mut out_buf);

        let freqs = out_buf
            .iter()
            .take(BUFFER_SIZE / 2)
            .map(|x| x.norm())
            .collect::<Vec<f64>>();

        // Average the ranges
        let mut bas = freqs[BAS_RANGE.clone()].iter().sum::<f64>() / BAS_RANGE.len() as f64;
        let mut mid = freqs[MID_RANGE.clone()].iter().sum::<f64>() / MID_RANGE.len() as f64;
        let mut tre = freqs[TRE_RANGE.clone()].iter().sum::<f64>() / TRE_RANGE.len() as f64;

        // Apply EQ
        bas = bas * BAS_EQ;
        mid = mid * MID_EQ;
        tre = tre * TRE_EQ;

        // Apply smoothing
        // TODO might not be necessary with three bars

        // Apply gravity
        bas_prev = bas_prev - (gravity * bas_time * bas_time);
        bas = if bas < bas_prev {
            bas_time += 1.0;
            bas_prev
        } else {
            bas_prev = bas;
            bas_time = 0.0;
            bas
        };

        mid_prev = mid_prev - (gravity * mid_time * mid_time);
        mid = if mid < mid_prev {
            mid_time += 1.0;
            mid_prev
        } else {
            mid_prev = mid;
            mid_time = 0.0;
            mid
        };

        tre_prev = tre_prev - (gravity * tre_time * tre_time);
        tre = if tre < tre_prev {
            tre_time += 1.0;
            tre_prev
        } else {
            tre_prev = tre;
            tre_time = 0.0;
            tre
        };

        // Apply scaling
        // TODO scale based on mean and stddev
        let bas = bas.min(255.0);
        let mid = mid.min(255.0);
        let tre = tre.min(255.0);

        // println!("{}", "X".repeat(bas));
        // println!("{}", "X".repeat(mid));
        // println!("{}", "X".repeat(tre));
        // println!("")

        let bas_color: RGBColor = cmap.transform_single(bas / 255.0);
        let mid_color: RGBColor = cmap.transform_single(mid / 255.0);
        let tre_color: RGBColor = cmap.transform_single(tre / 255.0);

        for (i, led) in controller.leds_mut(0).iter_mut().enumerate() {
            if i / 12 == 0 {
                *led = [bas_color.int_r(), bas_color.int_g(), bas_color.int_b(), 0];
            } else if i / 12 == 1 {
                *led = [mid_color.int_r(), mid_color.int_g(), mid_color.int_b(), 0];
            } else {
                *led = [tre_color.int_r(), tre_color.int_r(), tre_color.int_r(), 0];
            }
        }

        controller.render()?;
    }

    info!("Stream closed");

    Ok(())
}