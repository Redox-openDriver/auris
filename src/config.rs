use crate::bits::{BitReader, BitWriter};
use crate::tables::RATES;

pub const AAC_LC: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub object: u8,
    pub rate_index: usize,
    pub channels: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
    Object(u8),
    Rate(u32),
    Channels(usize),
    FrameLength,
    Truncated,
}

impl Config {
    pub fn new(rate: u32, channels: usize) -> Result<Config, Unsupported> {
        let rate_index = RATES.iter().position(|&known| known == rate).ok_or(Unsupported::Rate(rate))?;
        if !(1..=2).contains(&channels) {
            return Err(Unsupported::Channels(channels));
        }
        Ok(Config { object: AAC_LC, rate_index, channels })
    }

    pub fn rate(&self) -> u32 {
        RATES[self.rate_index]
    }

    pub fn read(reader: &mut BitReader) -> Result<Config, Unsupported> {
        let truncated = |_| Unsupported::Truncated;
        let mut object = reader.read(5).map_err(truncated)? as u8;
        if object == 31 {
            object = 32 + reader.read(6).map_err(truncated)? as u8;
        }
        let index = reader.read(4).map_err(truncated)? as usize;
        let rate_index = if index == 15 {
            let rate = reader.read(24).map_err(truncated)?;
            RATES.iter().position(|&known| known == rate).ok_or(Unsupported::Rate(rate))?
        } else if index < RATES.len() {
            index
        } else {
            return Err(Unsupported::Rate(0));
        };
        let channels = reader.read(4).map_err(truncated)? as usize;
        if object != AAC_LC {
            return Err(Unsupported::Object(object));
        }
        if reader.bit().map_err(truncated)? {
            return Err(Unsupported::FrameLength);
        }
        if reader.bit().map_err(truncated)? {
            reader.read(14).map_err(truncated)?;
        }
        reader.bit().map_err(truncated)?;
        if !(1..=2).contains(&channels) {
            return Err(Unsupported::Channels(channels));
        }
        Ok(Config { object, rate_index, channels })
    }

    pub fn write(&self, writer: &mut BitWriter) {
        writer.put(u32::from(self.object), 5);
        writer.put(self.rate_index as u32, 4);
        writer.put(self.channels as u32, 4);
        writer.put(0, 3);
    }
}
