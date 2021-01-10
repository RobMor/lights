use anyhow::{anyhow, Context, Result};
use bytes::Buf;
use claxon::{Block, FlacHeader};
use futures::{pin_mut, sink::SinkExt, stream::StreamExt};
use log::{debug, info};
use mac_address::get_mac_address;
use mdns::RecordKind;
use std::{net::IpAddr, time::Duration};
use tokio::net::ToSocketAddrs;
use tokio_util::time::DelayQueue;

use crate::protocol::{SnapHello, SnapKind, SnapMessage, SnapStream};

/// The hostname of the devices we are searching for.
const SERVICE_NAME: &'static str = "_snapcast._tcp.local";

pub struct SnapClient {
    stream: SnapStream,
    queue: DelayQueue<Vec<i32>>,
    time_delay: Duration,
    latency: Duration,
    header: Option<FlacHeader>,
    // TODO settings
}

impl SnapClient {
    pub async fn discover() -> Result<SnapClient> {
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
                return SnapClient::connect((addr, port)).await;
            } else {
                info!("Failed to find addr and port from response");
            }
        }

        Err(anyhow!(
            "Discovery stopped before we found a suitable server"
        ))
    }

    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<SnapClient> {
        let mut stream = SnapStream::connect(addr).await?;

        // TODO
        let kind = SnapKind::Hello {
            payload: SnapHello {
                arch: "x86_64".to_string(),
                client_name: "Snapclient".to_string(),
                host_name: "hostname".to_string(),
                id: get_mac_address().unwrap().unwrap().to_string(), // TODO
                instance: 1,
                mac: get_mac_address().unwrap().unwrap().to_string(), // TODO
                os: "Ubuntu".to_string(),
                protocol_version: 2,
                version: "0.17.1".to_string(),
            },
        };

        stream.send(kind).await?;

        return Ok(SnapClient {
            stream: stream,
            queue: DelayQueue::new(),
            time_delay: Duration::new(0, 0),
            latency: Duration::new(0, 0),
            header: None,
        });
    }

    pub async fn next(&mut self) -> Result<Option<Vec<i32>>> {
        loop {
            tokio::select! {
                // New messages from the snapserver
                Some(msg) = self.stream.next() => {
                    let msg = msg.context("Error while retrieving message from stream")?;

                    self.process_message(msg).await.context("Error while processing SnapMessage")?;
                },
                // New frames to return to the caller
                Some(frame) = self.queue.next() => {
                    let frame = frame.context("Error while retrieving frame from queue")?;

                    // Make sure this frame isn't too old...
                    if frame.deadline().elapsed() < Duration::from_millis(500) {
                        return Ok(Some(frame.into_inner()))
                    }
                },
                else => return Ok(None),
            }
        }
    }

    async fn process_message(&mut self, msg: SnapMessage) -> Result<()> {
        match msg.kind {
            SnapKind::ServerSettings { settings } => {
                self.time_delay = settings.latency;

                Ok(())
            },
            SnapKind::Time { latency } => {
                let received = msg.base.received;
                let sent = msg.base.sent;

                let delta = received - sent;

                self.latency = (latency - delta) / 2; // ???

                Ok(())
            }
            SnapKind::CodecHeader { codec, payload } => {
                match codec.as_str() {
                    "flac" => (),
                    // TODO support the other codecs...
                    s => {
                        return Err(anyhow!(
                            "The SnapServer is using an unsupported codec: {}",
                            s
                        ))
                    }
                }

                self.header =
                    Some(FlacHeader::from_header(payload).context("Error reading FLAC header")?);

                Ok(())
            }
            SnapKind::WireChunk {
                timestamp: _,
                mut payload,
            } if self.header.is_some() => {
                // TODO block makes an allocation
                let block = Block::from_frame(&mut payload).context("Error reading FLAC block")?;

                assert!(payload.remaining() == 0);

                // TODO maybe move this out?
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

                // TODO is this the right delay?
                self.queue.insert(data, self.time_delay - self.latency);

                Ok(())
            }
            // TODO
            _ => Ok(()),
        }
    }
}
