use alloc::vec::Vec;

use crate::bits::{BitReader, BitWriter, Exhausted};
use crate::config::{Config, Unsupported};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bad {
    Exhausted,
    NoConfig,
    Version,
    Layout,
    Config(Unsupported),
}

impl From<Exhausted> for Bad {
    fn from(_: Exhausted) -> Bad {
        Bad::Exhausted
    }
}

fn value(reader: &mut BitReader) -> Result<u32, Bad> {
    let bytes = reader.read(2)? + 1;
    let mut value = 0u32;
    for _ in 0..bytes {
        value = value << 8 | reader.read(8)?;
    }
    Ok(value)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Mux {
    config: Config,
    other_bits: u32,
}

#[derive(Default)]
pub struct Demuxer {
    mux: Option<Mux>,
    fallback: Option<Config>,
}

impl Demuxer {
    pub fn new(fallback: Option<Config>) -> Demuxer {
        Demuxer { mux: None, fallback }
    }

    pub fn config(&self) -> Option<Config> {
        self.mux.map(|mux| mux.config)
    }

    fn stream_mux_config(reader: &mut BitReader) -> Result<Mux, Bad> {
        let version = reader.bit()?;
        if version && reader.bit()? {
            return Err(Bad::Version);
        }
        if version {
            value(reader)?;
        }
        let same_framing = reader.bit()?;
        let sub_frames = reader.read(6)?;
        let programs = reader.read(4)?;
        let layers = reader.read(3)?;
        if !same_framing || sub_frames != 0 || programs != 0 || layers != 0 {
            return Err(Bad::Layout);
        }
        let config = if version {
            let length = value(reader)? as usize;
            let start = reader.position();
            let config = Config::read(reader).map_err(Bad::Config)?;
            let used = reader.position() - start;
            reader.skip(length.saturating_sub(used))?;
            config
        } else {
            Config::read(reader).map_err(Bad::Config)?
        };
        if reader.read(3)? != 0 {
            return Err(Bad::Layout);
        }
        reader.read(8)?;
        let mut other_bits = 0u32;
        if reader.bit()? {
            if version {
                other_bits = value(reader)?;
            } else {
                loop {
                    let more = reader.bit()?;
                    other_bits = other_bits << 8 | reader.read(8)?;
                    if !more {
                        break;
                    }
                }
            }
        }
        if reader.bit()? {
            reader.read(8)?;
        }
        Ok(Mux { config, other_bits })
    }

    pub fn payload(&mut self, element: &[u8], in_band: bool) -> Result<Vec<u8>, Bad> {
        let mut reader = BitReader::new(element);
        if in_band && !reader.bit()? {
            self.mux = Some(Demuxer::stream_mux_config(&mut reader)?);
        }
        let mux = match (self.mux, self.fallback) {
            (Some(mux), _) => mux,
            (None, Some(config)) => Mux { config, other_bits: 0 },
            (None, None) => return Err(Bad::NoConfig),
        };
        self.mux = Some(mux);
        let mut length = 0usize;
        loop {
            let piece = reader.read(8)? as usize;
            length += piece;
            if piece != 255 {
                break;
            }
        }
        let mut payload = Vec::with_capacity(length);
        for _ in 0..length {
            payload.push(reader.read(8)? as u8);
        }
        Ok(payload)
    }
}

pub fn element(config: &Config, payload: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::default();
    writer.put(0, 1);
    writer.put(0, 1);
    writer.put(1, 1);
    writer.put(0, 6);
    writer.put(0, 4);
    writer.put(0, 3);
    config.write(&mut writer);
    writer.put(0, 3);
    writer.put(0xff, 8);
    writer.put(0, 1);
    writer.put(0, 1);
    let mut left = payload.len();
    while left >= 255 {
        writer.put(255, 8);
        left -= 255;
    }
    writer.put(left as u32, 8);
    for &byte in payload {
        writer.put(u32::from(byte), 8);
    }
    writer.finish()
}

pub fn strip_loas(stream: &[u8]) -> Option<(&[u8], usize)> {
    let header = stream.get(..3)?;
    if u16::from_be_bytes([header[0], header[1]]) >> 5 != 0x2b7 {
        return None;
    }
    let length = (usize::from(header[1] & 0x1f) << 8) | usize::from(header[2]);
    Some((stream.get(3..3 + length)?, 3 + length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_elements_carry_their_config_and_payload_back() {
        let config = Config::new(48000, 2).unwrap();
        let payload: Vec<u8> = (0..300u32).map(|n| n as u8).collect();
        let element = element(&config, &payload);
        let mut demuxer = Demuxer::new(None);
        assert_eq!(demuxer.payload(&element, true), Ok(payload));
        assert_eq!(demuxer.config(), Some(config));
        assert_eq!(Demuxer::new(None).payload(&[0x80, 0, 0], true), Err(Bad::NoConfig));
        let mut out_of_band = Demuxer::new(Some(config));
        assert_eq!(out_of_band.payload(&[3, 1, 2, 3], false), Ok(alloc::vec![1, 2, 3]));
    }
}
