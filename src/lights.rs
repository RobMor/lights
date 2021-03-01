use anyhow::Result;
use rs_ws281x::{ChannelBuilder, ControllerBuilder, StripType};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::NUM_LIGHTS;

pub fn start(mut rx: mpsc::Receiver<[[u8; 3]; NUM_LIGHTS]>) -> JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || {
        log::info!("Starting Lights");

        // TODO we can't do this in some kind of setup function becase Controller doesn't implement Send...
        let mut controller = match ControllerBuilder::new()
            .channel(
                0,
                ChannelBuilder::new()
                    .pin(18) // TODO based on some config
                    .count(12 * NUM_LIGHTS as i32)
                    .strip_type(StripType::Ws2811Gbr)
                    .brightness(255)
                    .build(),
            )
            .build() {
                Ok(controller) => controller,
                Err(e) => {
                    log::error!("Failed to build controller: {}", e);
                    return Err(e.into())
                }
            };

        log::trace!("Entering main loop");

        while let Some(color) = rx.blocking_recv() {
            log::trace!("Received colors {:?}", color);

            for (i, led) in controller.leds_mut(0).iter_mut().enumerate() {
                if i / 12 == 0 {
                    *led = [color[0][0], color[0][1], color[0][2], 0];
                } else if i / 12 == 1 {
                    *led = [color[1][0], color[1][1], color[1][2], 0];
                } else {
                    *led = [color[2][0], color[2][1], color[2][2], 0];
                }
            }

            match controller.render() {
                Ok(()) => log::trace!("Sucessfully set color"),
                Err(e) => log::error!("Failed to set color: {}", e),
            }
        }

        log::info!("Lights stopping");

        Ok(())
    })
}
