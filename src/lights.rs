use anyhow::Result;
use rs_ws281x::{ChannelBuilder, ControllerBuilder, StripType};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use std::convert::TryInto;
use std::fs::File;
use std::io::Write;

use crate::Color;
use crate::NUM_LIGHTS;


use druid::widget::prelude::*;
use druid::{AppLauncher, WindowDesc, Selector, Rect, WidgetExt, Target, Affine};
use druid::piet::kurbo::{Shape, PathEl};

const SET_COLOR: Selector<[(u8, druid::Color); NUM_LIGHTS]> = Selector::new("lights.set-color");


const NUM_POINTS: usize = 1000;

#[derive(Clone, PartialEq, Data)]
struct LightState {
    colors: [druid::Color; NUM_LIGHTS],
    // Ring buffer of intensity points
    starts: [usize; NUM_LIGHTS],
    // this is not correct but it's good enough...
    #[data(ignore)]
    intensities: Vec<Vec<u8>>,
}

struct LightWidget;

impl Widget<LightState> for LightWidget {
    fn event(&mut self, _ctx: &mut EventCtx, event: &Event, data: &mut LightState, _env: &Env) {
        match event {
            // This is where we handle our command.
            Event::Command(cmd) if cmd.is(SET_COLOR) => {
                let new_data = cmd.get_unchecked(SET_COLOR);

                for (n, (intensity, color)) in new_data.iter().enumerate() {
                    data.colors[n] = color.clone();
                    data.intensities[n][data.starts[n]] = *intensity;
                    data.starts[n] = (data.starts[n] + 1) % NUM_POINTS;
                }
            },
            _ => {},
        }
    }

    fn lifecycle(&mut self, _ctx: &mut LifeCycleCtx, _event: &LifeCycle, _data: &LightState, _: &Env) {}

    fn update(&mut self, ctx: &mut UpdateCtx, old_data: &LightState, data: &LightState, _: &Env) {
        if old_data != data {
            ctx.request_paint()
        }
    }

    fn layout(&mut self, _: &mut LayoutCtx, bc: &BoxConstraints, _: &LightState, _: &Env) -> Size {
        bc.max()
    }

    fn paint(&mut self, ctx: &mut PaintCtx, data: &LightState, _env: &Env) {
        let size = ctx.size();
        let point_width = size.width / NUM_POINTS as f64;
        let light_height = size.height / NUM_LIGHTS as f64;

        for (light, color) in data.colors.iter().rev().enumerate() {
            ctx.with_save(|ctx| {
                // TODO this could probably be simplified
                ctx.transform(Affine::translate((0.0, (light + 1) as f64 * light_height)));
                ctx.transform(Affine::FLIP_Y);

                let rect = Rect::new(0.0, 0.0, size.width, light_height);
                ctx.fill(rect, color);

                let graph: Vec<PathEl> = std::iter::once(PathEl::MoveTo((0.0, 0.0).into()))
                        .chain((0..NUM_POINTS)
                            .map(|n| {
                                PathEl::LineTo((n as f64 * point_width, (data.intensities[light][(data.starts[light] + n) % NUM_POINTS] as f64 / 255.0) * light_height).into())
                            }))
                        .chain(std::iter::once(PathEl::MoveTo((0.0, 0.0).into())))
                        .chain(std::iter::once(PathEl::ClosePath))
                        .collect();

                let (r, g, b, _) = color.as_rgba8();
                let inverted_color = druid::Color::rgb8(255 - r, 255 - g, 255 - b);

                ctx.stroke(&graph[..], &inverted_color, 1.0);
            })
        }
    }
}

fn build_root_widget() -> impl Widget<LightState> {
    LightWidget
}

pub fn start(mut rx: mpsc::Receiver<[Color; NUM_LIGHTS]>) -> JoinHandle<Result<()>> {
    tokio::task::spawn_blocking(move || {
        let main_window = WindowDesc::new(build_root_widget).title("Lights Visualization");

        let launcher = AppLauncher::with_window(main_window);

        let event_sink = launcher.get_external_handle();

        tokio::task::spawn_blocking(move || {
            let mut dcolors = vec![(0u8, druid::Color::BLACK); NUM_LIGHTS];
            while let Some(data) = rx.blocking_recv() {
                for (n, (intensity, color)) in data.iter().enumerate() {
                    dcolors[n] = (*intensity, druid::Color::rgb8(color[0], color[1], color[2]));
                }

                // Wack
                if event_sink.submit_command(SET_COLOR, Box::new(dcolors.clone().try_into().expect("whatever")), Target::Auto).is_err() {
                    break;
                }
            }
        });

        let initial_state = LightState {
            colors: [druid::Color::BLACK; NUM_LIGHTS],
            starts: [0; NUM_LIGHTS],
            intensities: vec![vec![0; NUM_POINTS]; NUM_LIGHTS],
        };

        launcher
            .launch(initial_state)
            .expect("Failed to launch lights");

        Ok(())



        // log::info!("Starting Lights");

        // // let mut bas = File::create("bas.txt").unwrap();
        // // let mut mid = File::create("mid.txt").unwrap();
        // // let mut tre = File::create("tre.txt").unwrap();

        // // TODO we can't do this in some kind of setup function becase Controller doesn't implement Send...
        // let mut controller = match ControllerBuilder::new()
        //     .channel(
        //         0,
        //         ChannelBuilder::new()
        //             .pin(18) // TODO based on some config
        //             .count(12 * NUM_LIGHTS as i32)
        //             .strip_type(StripType::Ws2811Gbr)
        //             .brightness(255)
        //             .build(),
        //     )
        //     .build() {
        //         Ok(controller) => controller,
        //         Err(e) => {
        //             log::error!("Failed to build controller: {}", e);
        //             return Err(e.into())
        //         }
        //     };

        // log::trace!("Entering main loop");

        // // let mut leds = [[0; 4]; 3];

        // while let Some(color) = rx.blocking_recv() {
        //     log::trace!("Received colors {:?}", color);

        //     for (i, led) in controller.leds_mut(0).iter_mut().enumerate() {
        //         if i / 12 == 0 {
        //             *led = [color[0][0], color[0][1], color[0][2], 0];
        //         } else if i / 12 == 1 {
        //             *led = [color[1][0], color[1][1], color[1][2], 0];
        //         } else {
        //             *led = [color[2][0], color[2][1], color[2][2], 0];
        //         }
        //     }

        //     // writeln!(bas, "{}", color[0]).unwrap();
        //     // writeln!(mid, "{}", color[1]).unwrap();
        //     // writeln!(tre, "{}", color[2]).unwrap();

        //     // log::trace!("Sucessfully set color")
        //     match controller.render() {
        //         Ok(()) => log::trace!("Sucessfully set color"),
        //         Err(e) => log::error!("Failed to set color: {}", e),
        //     }
        // }

        // log::info!("Lights stopping");

        // Ok(())
    })
}
