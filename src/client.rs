use anyhow::{anyhow, Result};
use bytes::Buf;
use claxon::{Block, FlacHeader};
use futures::{pin_mut, sink::SinkExt, stream::StreamExt};
use log::{debug, info};
use mdns::RecordKind;
use std::{net::IpAddr, time::Duration};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use crate::protocol::{SnapCodec, SnapKind, SnapMessage};

/// The hostname of the devices we are searching for.
const SERVICE_NAME: &'static str = "_snapcast._tcp.local";

pub struct SnapClient {
    stream: Framed<TcpStream, SnapCodec>,
    header: Option<FlacHeader>,
    // TODO settings
}

impl SnapClient {
    pub async fn init() -> Result<SnapClient> {
        // Iterate through responses from each Cast device, asking for new devices every 15s
        let stream = mdns::discover::all(SERVICE_NAME, Duration::from_secs(15))?.listen();
        pin_mut!(stream);

        while let Some(Ok(response)) = stream.next().await {
            let mut addr: Option<IpAddr> = None;
            let mut port: Option<u16> = None;

            info!("Got response");

            for record in response.records() {
                match record.kind {
                    RecordKind::SRV { port: p, .. } => port = Some(p),
                    // We prefer A records because they're ipv4 addresses
                    RecordKind::AAAA(ip) => addr = addr.or(Some(ip.into())),
                    RecordKind::A(ip) => addr = Some(ip.into()),
                    _ => (),
                }
            }

            if let (Some(addr), Some(port)) = (addr, port) {
                info!("Got addr {} and port {} from response", addr, port);
                let stream = TcpStream::connect((addr, port)).await?;
                info!("Connected");

                let mut client = SnapClient {
                    stream: Framed::new(stream, SnapCodec::new()),
                    header: None,
                };

                client.send(SnapMessage::hello()).await?;
                client.flush().await?;

                return Ok(client);
            } else {
                info!("Failed to find addr and port from response");
            }
        }

        Err(anyhow!(
            "Discovery stopped before we found a suitable server"
        ))
    }

    pub async fn send(&mut self, msg: SnapMessage) -> Result<()> {
        self.stream.send(msg).await
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.stream.flush().await
    }

    pub async fn next(&mut self) -> Result<Option<Vec<i32>>> {
        while let Some(msg) = self.stream.next().await {
            let msg = msg?;

            debug!("Received: {:?}", msg);

            let mut block = None;

            match msg.kind {
                SnapKind::CodecHeader { codec, payload } => {
                    assert_eq!(codec, "flac".to_string());

                    self.header = Some(FlacHeader::from_header(payload)?);
                }
                SnapKind::WireChunk {
                    timestamp: _, // TODO 
                    mut payload,
                } => {
                    // TODO block allocates...
                    block = Block::from_frame(&mut payload)?;
                    assert!(payload.remaining() == 0)
                }
                _ => (),
            }

            if let Some(block) = block.take() {
                let data = if block.channels() != 1 {
                    let num_channels = block.channels() as usize;
                    let block_size = block.len() as usize / num_channels;

                    let buffer = block.into_buffer();

                    // Channels are stored sequentially, meaning the entire first channel is stored,
                    // then the entire second channel, then ...

                    (0..block_size)
                        // I think it's reasonable to assume that the number of channels will always fit in a 32 bit integer
                        .map(|i| {
                            (0..num_channels).map(|_| buffer[i]).sum::<i32>() as i32
                                / num_channels as i32
                        })
                        .collect()
                } else {
                    block.into_buffer()
                };

                debug!("Length: {}", data.len());

                return Ok(Some(data));
            }
        }

        Ok(None)
    }
}
