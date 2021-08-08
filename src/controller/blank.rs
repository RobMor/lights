use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::controller::{InMessage, OutMessage, Token};

pub struct BlankController {
    token: Token,
    rx: mpsc::Receiver<InMessage>,
    tx: mpsc::Sender<(Token, OutMessage)>,
}

impl BlankController {
    pub fn start(
        token: Token,
        rx: mpsc::Receiver<InMessage>,
        tx: mpsc::Sender<(Token, OutMessage)>,
    ) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let controller = BlankController { token, rx, tx };

            log::info!("Starting Blank Controller with token {:?}", token);

            controller.run().await
        })
    }

    async fn run(mut self) -> Result<()> {
        let mut sender = None;

        while let Some(msg) = self.rx.recv().await {
            match msg {
                InMessage::GrantAccess(s) => {
                    log::debug!("Received access to lights controller");

                    match s.send([(0, [255, 0, 0]); 3]).await {
                    // match s.send([0; 3]).await {
                        Ok(()) => {
                            log::trace!("Successfully made lights blank");
                        }
                        Err(e) => {
                            log::error!("Failed to make lights blank: {}", e);
                        }
                    }

                    sender = Some(s);
                }
                InMessage::RevokeAccess => {
                    log::debug!("Received revoke access request");

                    if let Some(sender) = sender.take() {
                        log::debug!("Rescinded access to lights controller");
                        self.tx.send((self.token, OutMessage::RescindAccess(sender))).await?
                    } else {
                        log::error!("Didn't have access to lights controller");
                    }
                }
            }
        }

        Ok(())
    }
}
