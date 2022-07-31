use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use simple_logger::SimpleLogger;
use tokio::runtime::Runtime;

mod color;
mod controller;
mod lights;

use controller::Controller;
use controller::music::MusicController;
use controller::blank::BlankController;
use tokio::sync::mpsc;


fn main() -> Result<()> {
    SimpleLogger::new().with_level(log::LevelFilter::Debug).init().unwrap();

    let rt = Runtime::new().unwrap();

    let _guard = rt.enter();

    let (lights_tx, lights_rx) = mpsc::channel(50);
    let lights = lights::start(lights_rx);

    let mut controllers = Vec::new();

    // Added in priority order
    controllers.push(("Music", setup_music()));
    controllers.push(("Blank", setup_blank()));

    let frame_duration = Duration::from_secs(1) / 60;

    let report_period = Duration::from_secs(5);
    let mut report_start = Instant::now();
    let mut report_sum = 0;
    let mut report_n = 0;

    let mut active_index: Option<usize> = None;

    loop {
        let frame_start = Instant::now();

        // Iterate in priority order
        for (index, (name, controller)) in controllers.iter_mut().enumerate() {
            if controller.is_active() {
                if active_index.replace(index).map_or(true, |i| index != i) {
                    log::info!("Controller {} just took over", name);
                }

                let color = controller.tick();

                lights_tx.blocking_send(color)?;

                break;
            }
        }

        let frame_elapsed = frame_start.elapsed();
        report_sum += frame_elapsed.as_millis();
        report_n += 1;

        if report_start.elapsed() > report_period {
            log::info!("Display stats [num frames in report: {}, avg frame time in ms: {:.3}]", report_n, report_sum as f64 / report_n as f64);
            report_start = Instant::now();
            report_sum = 0;
            report_n = 0;
        }

        if frame_elapsed < frame_duration {
            // Sleep until the end of the frame
            std::thread::sleep(frame_duration - frame_elapsed);
        }
    }

    Ok(())
}

fn setup_music() -> Box<dyn Controller> {
    Box::new(MusicController::start())
}

fn setup_blank() -> Box<dyn Controller> {
    Box::new(BlankController::new())
}