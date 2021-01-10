use anyhow::{anyhow, Context, Error, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use chrono::{DateTime, Local, TimeZone};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::convert::TryInto;
use std::io::Cursor;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::net::ToSocketAddrs;
use tokio_util::codec::Framed;
use tokio_util::codec::{Decoder, Encoder};

const BASE_MESSAGE_SIZE: usize = 26;

pub struct SnapStream {
    stream: Framed<TcpStream, SnapCodec>,
    instant: Instant,
    current_id: u16,
}

impl SnapStream {
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<SnapStream> {
        let stream = TcpStream::connect(addr).await?;
        let instant = Instant::now();

        Ok(SnapStream {
            stream: Framed::new(stream, SnapCodec::new(instant)),
            instant: instant,
            current_id: 0,
        })
    }

    pub async fn send(&mut self, msg: SnapKind) -> Result<()> {
        let msg = SnapMessage {
            base: SnapBase {
                id: self.current_id,
                refers_to: 0,
                received: Duration::new(0, 0),
                sent: self.instant.elapsed(),
            },
            kind: msg,
        };

        self.current_id += 1;

        self.stream.send(msg).await
    }

    // pub async fn respond(&mut self, id: u16, response: SnapKind) -> Result<()> {
    //     self.current_id
    //     let msg = SnapMessage {
    //         base: SnapBase {
    //             id: self.current_id,
    //             refers_to: id,
    //             received: Duration::new(0, 0),
    //             sent: self.instant.elapsed(),
    //         },
    //         kind: response
    //     };

    //     self.stream.send(msg).await
    // }

    pub async fn next(&mut self) -> Option<Result<SnapMessage>> {
        self.stream.next().await
    }
}

#[derive(Debug)]
pub struct SnapBase {
    pub id: u16,
    pub refers_to: u16,
    pub received: Duration,
    pub sent: Duration,
}

#[derive(Debug)]
pub struct SnapMessage {
    pub base: SnapBase,
    pub kind: SnapKind,
}

impl SnapMessage {
    fn size(&self) -> usize {
        BASE_MESSAGE_SIZE + self.kind.size() as usize
    }
}

#[derive(Debug)]
pub enum SnapKind {
    CodecHeader {
        codec: String,
        payload: Bytes,
    },
    WireChunk {
        timestamp: DateTime<Local>,
        payload: Bytes,
    },
    ServerSettings {
        settings: SnapServerSettings,
    },
    Time {
        latency: Duration,
    },
    Hello {
        payload: SnapHello,
    },
    StreamTags {
        tags: HashMap<String, String>,
    },
}

impl SnapKind {
    fn id(&self) -> u16 {
        match self {
            SnapKind::CodecHeader { .. } => 1,
            SnapKind::WireChunk { .. } => 2,
            SnapKind::ServerSettings { .. } => 3,
            SnapKind::Time { .. } => 4,
            SnapKind::Hello { .. } => 5,
            SnapKind::StreamTags { .. } => 6,
        }
    }

    fn size(&self) -> u32 {
        match self {
            SnapKind::CodecHeader { codec, payload } => {
                4 + codec.len() as u32 + 4 + payload.len() as u32
            }
            SnapKind::WireChunk { payload, .. } => 8 + 4 + payload.len() as u32,
            SnapKind::ServerSettings { settings } => {
                4 + serde_json::to_vec(&settings).unwrap().len() as u32
            }
            SnapKind::Time { .. } => 8,
            SnapKind::Hello { payload } => 4 + serde_json::to_vec(&payload).unwrap().len() as u32,
            SnapKind::StreamTags { tags } => 4 + serde_json::to_vec(&tags).unwrap().len() as u32,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapServerSettings {
    pub buffer_ms: usize,
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub latency: Duration,
    pub muted: bool,
    pub volume: usize,
}

fn deserialize_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    Ok(Duration::from_millis(Deserialize::deserialize(d)?))
}

fn serialize_duration<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u128(d.as_millis())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SnapHello {
    pub arch: String,
    pub client_name: String,
    pub host_name: String,
    #[serde(rename = "ID")]
    pub id: String,
    pub instance: usize,
    #[serde(rename = "MAC")]
    pub mac: String,
    #[serde(rename = "OS")]
    pub os: String,
    #[serde(rename = "SnapStreamProtocolVersion")]
    pub protocol_version: usize,
    pub version: String,
}

pub struct SnapCodec {
    instant: Instant,
}

impl SnapCodec {
    pub fn new(instant: Instant) -> SnapCodec {
        SnapCodec { instant }
    }
}

impl Decoder for SnapCodec {
    type Item = SnapMessage;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<SnapMessage>, Error> {
        if src.len() < BASE_MESSAGE_SIZE {
            // We don't have a full base message yet
            src.reserve(BASE_MESSAGE_SIZE);
            return Ok(None);
        }

        // Create a cursor that wraps the source buffer so we can read the header
        // without advancing the source buffers internal cursor.
        let mut cursor = Cursor::new(&mut *src);

        let msg_type = cursor.get_u16_le();
        let id = cursor.get_u16_le();
        let refers_to = cursor.get_u16_le();
        let _received_sec = cursor.get_i32_le();
        let _received_usec = cursor.get_i32_le();
        let sent_sec = cursor.get_i32_le();
        let sent_usec = cursor.get_i32_le();
        let size = cursor.get_u32_le();

        if src.len() < BASE_MESSAGE_SIZE + size as usize {
            // We don't have the full message yet
            src.reserve(BASE_MESSAGE_SIZE + size as usize);
            return Ok(None);
        }

        let base = SnapBase {
            id,
            refers_to,
            received: self.instant.elapsed(),
            sent: Duration::from_secs(sent_sec as u64)
                + Duration::from_millis(sent_usec as u64 / 1000),
        };

        // We successfully read the base message so move past it
        src.advance(BASE_MESSAGE_SIZE);
        // Cut out the message data from the source buffer
        let mut data = src.split_to(size as usize);

        match msg_type {
            1 => {
                let codec_size = data.get_u32_le();
                let codec = data.split_to(codec_size as usize);
                let codec = std::str::from_utf8(&codec)?.to_string();

                let size = data.get_u32_le();
                let payload = data.split_to(size as usize).freeze();

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::CodecHeader { codec, payload },
                }))
            }
            2 => {
                let timestamp_sec = data.get_i32_le();
                let timestamp_usec = data.get_i32_le();
                let size = data.get_u32_le();
                let payload = data.split_to(size as usize).freeze();

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::WireChunk {
                        timestamp: Local
                            .timestamp(timestamp_sec as i64, timestamp_usec as u32 * 1000),
                        payload,
                    },
                }))
            }
            3 => {
                let size = data.get_u32_le();
                let payload = data.split_to(size as usize);
                let settings = serde_json::from_slice(&payload)
                    .context("Error while parsing server settings JSON")?;

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::ServerSettings { settings },
                }))
            }
            4 => {
                let latency_sec = data.get_i32_le();
                let latency_usec = data.get_i32_le();

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::Time {
                        latency: Duration::from_secs(latency_sec.try_into()?)
                            + Duration::from_micros(latency_usec.try_into()?),
                    },
                }))
            }
            5 => {
                let size = data.get_u32_le();
                let payload = data.split_to(size as usize);
                let payload =
                    serde_json::from_slice(&payload).context("Error while parsing Hello JSON")?;

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::Hello { payload },
                }))
            }
            6 => {
                let size = data.get_u32_le();
                let payload = data.split_to(size as usize);
                let tags = serde_json::from_slice(&payload)
                    .context("Error while parsing stream tags JSON")?;

                Ok(Some(SnapMessage {
                    base: base,
                    kind: SnapKind::StreamTags { tags },
                }))
            }
            id => Err(anyhow!("Unrecognized packet ID {}", id)),
        }
    }
}

impl Encoder<SnapMessage> for SnapCodec {
    type Error = Error;

    fn encode(&mut self, item: SnapMessage, dst: &mut BytesMut) -> Result<(), Error> {
        dst.reserve(item.size());

        // Write the base message
        dst.put_u16_le(item.kind.id());
        dst.put_u16_le(item.base.id);
        dst.put_u16_le(item.base.refers_to);
        dst.put_i32_le(item.base.received.as_secs().try_into()?);
        dst.put_i32_le(item.base.received.subsec_micros().try_into()?);
        dst.put_i32_le(item.base.sent.as_secs().try_into()?);
        dst.put_i32_le(item.base.sent.subsec_micros().try_into()?);
        dst.put_u32_le(item.kind.size());

        match item.kind {
            SnapKind::CodecHeader { codec, payload } => {
                let bytes = codec.as_bytes();
                dst.put_u32_le(bytes.len().try_into()?);
                dst.put_slice(bytes);
                dst.put_u32_le(payload.len().try_into()?);
                dst.put_slice(&payload);
            }
            SnapKind::WireChunk { timestamp, payload } => {
                dst.put_i32_le(timestamp.timestamp().try_into()?);
                dst.put_i32_le(timestamp.timestamp_subsec_micros().try_into()?);
                dst.put_u32_le(payload.len() as u32);
                dst.put_slice(&payload);
            }
            SnapKind::ServerSettings { settings } => {
                let payload = serde_json::to_vec(&settings)?;
                dst.put_u32_le(payload.len().try_into()?);
                dst.put_slice(&payload);
            }
            SnapKind::Time { latency } => {
                dst.put_u32_le(latency.as_secs().try_into()?);
                dst.put_u32_le(latency.subsec_micros());
            }
            SnapKind::Hello { payload } => {
                let payload = serde_json::to_vec(&payload)?;
                dst.put_u32_le(payload.len().try_into()?);
                dst.put_slice(&payload);
            }
            SnapKind::StreamTags { tags } => {
                let payload = serde_json::to_vec(&tags)?;
                dst.put_u32_le(payload.len().try_into()?);
                dst.put_slice(&payload);
            }
        }

        Ok(())
    }
}
