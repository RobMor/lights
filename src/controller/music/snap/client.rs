use anyhow::{anyhow, Context, Result};
use bytes::Buf;
use claxon::{Block, FlacHeader};
use futures::{pin_mut, stream::StreamExt};
use mac_address::get_mac_address;
use mdns::RecordKind;
use std::convert::TryInto;
use std::net::IpAddr;
use time::{Duration, Instant, NumericalDuration};
use tokio::net::ToSocketAddrs;
use tokio_util::time::DelayQueue;

use crate::controller::music::snap::protocol::{SnapHello, SnapKind, SnapMessage, SnapStream};

/// The MDNS service name that the snapserver uses
const SERVICE_NAME: &'static str = "_snapcast._tcp.local";

pub struct SnapClient {
    /// The actual stream of messages coming in
    stream: SnapStream,
    /// Queue for raw audio frames
    queue: DelayQueue<Vec<i32>>,
    /// Base timestamp from which all other timestamps are derived
    instant: Instant,
    /// Difference in time between the client and server
    time_diff: Duration,
    /// The amount of time to wait after the timestamp before playing a frame
    delay: Duration,
    /// The codec header
    header: Option<FlacHeader>,
}

impl SnapClient {
    pub async fn discover() -> Result<SnapClient> {
        // Iterate through responses from each Cast device, asking for new devices every 15s
        let stream = mdns::discover::all(SERVICE_NAME, Duration::seconds(15).try_into()?)?.listen();
        pin_mut!(stream);

        while let Some(Ok(response)) = stream.next().await {
            let mut addr: Option<IpAddr> = None;
            let mut port: Option<u16> = None;

            log::info!("Got response");

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
                log::info!("Got addr {} and port {} from response", addr, port);
                return SnapClient::connect((addr, port)).await;
            } else {
                log::info!("Failed to find addr and port from response");
            }
        }

        Err(anyhow!("Discovery stopped before we found a suitable server"))
    }

    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<SnapClient> {
        let instant = Instant::now();
        let mut stream = SnapStream::connect(addr, instant).await?;

        // TODO fill this out with the correct information
        let hello = SnapKind::Hello {
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

        stream.send(hello).await?;

        let time = SnapKind::Time {
            delta: Duration::new(0, 0),
        };

        stream.send(time).await?;

        return Ok(SnapClient {
            stream: stream,
            queue: DelayQueue::new(),
            instant: instant,

            // TODO maybe just wait for the necessary information here rather than initializing with fake data
            delay: Duration::new(0, 0),
            time_diff: Duration::new(0, 0),
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

                    // TODO we are consistently about a millisecond or two late...
                    let time_over = frame.deadline().elapsed();

                    let frame = frame.into_inner();

                    // Make sure this frame isn't too old...
                    let length = self.header.as_ref().map_or(
                        20.milliseconds(),
                        |header| (frame.len() as f64 / header.streaminfo().sample_rate as f64).seconds()
                    );

                    if time_over < length {
                        return Ok(Some(frame))
                    }
                },
                else => return Ok(None),
            }
        }
    }

    async fn process_message(&mut self, msg: SnapMessage) -> Result<()> {
        match msg.kind {
            SnapKind::ServerSettings { settings } => {
                // TODO are the other fields of any use here?
                self.delay = settings.buffer_ms;

                Ok(())
            }
            SnapKind::Time {
                delta: client_to_server,
            } => {
                // This is like NTP
                let server_to_client = msg.base.sent - msg.base.received;
                self.time_diff = (client_to_server + server_to_client) / 2;

                Ok(())
            }
            SnapKind::CodecHeader { codec, payload } => {
                match codec.as_str() {
                    "flac" => (),
                    // TODO support the other codecs...
                    s => return Err(anyhow!("The SnapServer is using an unsupported codec: {}", s)),
                }

                self.header = Some(FlacHeader::from_header(payload).context("Error reading FLAC header")?);

                Ok(())
            }
            SnapKind::WireChunk { timestamp, mut payload } if self.header.is_some() => {
                // TODO block makes an allocation
                let block = Block::from_frame(&mut payload).context("Error reading FLAC block")?;

                assert!(payload.remaining() == 0);

                let data = if block.channels() != 1 {
                    let num_channels = block.channels() as usize;
                    let block_size = block.len() as usize / num_channels;

                    let buffer = block.into_buffer();

                    // Channels are stored sequentially, meaning the entire first channel is stored,
                    // then the entire second channel, and so on.
                    //
                    // 0 1 2 3 4 5 6 7 8 0 1 2 3 4 5 6 7 8 ...
                    // [   channel 1   ] [   channel 2   ] ...
                    //
                    // Here we are just taking the average of all the channels to produce one channel.

                    (0..block_size)
                        .map(|i| {
                            (0..num_channels).map(|j| buffer[i + block_size * j]).sum::<i32>() as i32
                                / num_channels as i32
                        })
                        .collect()
                } else {
                    // Small optimization, just return the block of data if it only has one channel.
                    // TODO the frame might be guaranteed to have more than 1 channel...
                    block.into_buffer()
                };

                // Compute the delay before the frame should be 'played'. This is based on the server
                // provided value of the amount of buffer time and the timestamp of the frame.
                //
                // We want the frame to play when the server time hits timestamp + delay.

                let server_now = self.instant.elapsed() + self.time_diff;
                let delay = (timestamp - server_now) + self.delay;

                if delay.is_positive() {
                    self.queue.insert(data, delay.try_into()?);
                }

                Ok(())
            }
            // TODO
            _ => Ok(()),
        }
    }
}
