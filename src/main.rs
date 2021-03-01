use anyhow::Result;
use simple_logger::SimpleLogger;
use std::collections::{BinaryHeap, HashMap};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

mod snap;
mod cmap;
mod controller;
mod lights;

use controller::{
    InMessage, OutMessage, Token,
    music::MusicController,
    blank::BlankController,
};

pub const NUM_LIGHTS: usize = 3;

struct ControllerHandle {
    pub tx: mpsc::Sender<InMessage>,
    pub handle: JoinHandle<Result<()>>,
}

impl ControllerHandle {
    fn new(tx: mpsc::Sender<InMessage>, handle: JoinHandle<Result<()>>) -> ControllerHandle {
        ControllerHandle { tx, handle: handle }
    }
}

fn setup_music(token: Token, out_tx: mpsc::Sender<(Token, OutMessage)>) -> ControllerHandle {
    // TODO good size bound?
    let (tx, in_rx) = mpsc::channel(50);

    let handle = MusicController::start(token, in_rx, out_tx);

    ControllerHandle::new(tx, handle)
}

fn setup_blank(token: Token, out_tx: mpsc::Sender<(Token, OutMessage)>) -> ControllerHandle {
    // TODO good size bound?
    let (tx, in_rx) = mpsc::channel(50);

    let handle = BlankController::start(token, in_rx, out_tx);

    ControllerHandle::new(tx, handle)
}

#[tokio::main]
async fn main() -> Result<()> {
    SimpleLogger::new().with_level(log::LevelFilter::Debug).init().unwrap();

    // TODO what is a good size bound?
    // TODO make the tx un-cloneable
    let (lights_tx, lights_rx) = mpsc::channel(50);

    let lights = lights::start(lights_rx);

    // TODO what is a good size bound?
    let (tx, mut rx) = mpsc::channel(50);

    // TODO jank
    let mut controllers = HashMap::new();

    let music_token = Token::new(10);
    controllers.insert(music_token, setup_music(music_token, tx.clone()));

    let blank_token = Token::new(0);
    controllers.insert(blank_token, setup_blank(blank_token, tx.clone()));

    let default = blank_token;
    let mut active = default;
    let mut pending_access = BinaryHeap::with_capacity(controllers.len());

    controllers[&default].tx.send(InMessage::GrantAccess(lights_tx)).await?;

    while let Some((token, msg)) = rx.recv().await {
        // TODO this feels fragile...
        // and over-complicated...
        match msg {
            OutMessage::RequestAccess => {
                log::info!("Received access request from {:?}", token);

                if active < token {
                    log::info!("Revoked access from active {:?}", active);
                    // TODO some kind of timeout?
                    controllers[&active].tx.send(InMessage::RevokeAccess).await?
                }
                pending_access.push(token);
            }
            OutMessage::RescindAccess(s) => {
                assert!(active == token);

                log::info!("Received rescind access from {:?}", token);

                active = pending_access.pop().unwrap_or(default);

                log::info!("Granting access to {:?}", active);

                controllers[&active].tx.send(InMessage::GrantAccess(s)).await?
            }
        }
    }

    Ok(())
}
