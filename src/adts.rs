use crate::bits::{BitReader, BitWriter};
use crate::config::{AAC_LC, Config};

pub const HEADER: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bad {
    Sync,
    Short,
    Profile,
    Length,
}

pub fn parse(stream: &[u8]) -> Result<(Config, &[u8], usize), Bad> {
    let mut reader = BitReader::new(stream.get(..HEADER).ok_or(Bad::Short)?);
    let read = |reader: &mut BitReader, bits| reader.read(bits).map_err(|_| Bad::Short);
    if read(&mut reader, 12)? != 0xfff {
        return Err(Bad::Sync);
    }
    read(&mut reader, 3)?;
    let protection_absent = read(&mut reader, 1)? == 1;
    let profile = read(&mut reader, 2)? as u8 + 1;
    let rate_index = read(&mut reader, 4)? as usize;
    read(&mut reader, 1)?;
    let channels = read(&mut reader, 3)? as usize;
    read(&mut reader, 4)?;
    let length = read(&mut reader, 13)? as usize;
    if profile != AAC_LC {
        return Err(Bad::Profile);
    }
    let header = if protection_absent { HEADER } else { HEADER + 2 };
    if length < header || length > stream.len() || rate_index >= crate::tables::RATES.len() {
        return Err(Bad::Length);
    }
    Ok((Config { object: profile, rate_index, channels }, &stream[header..length], length))
}

pub fn frame(config: &Config, block: &[u8]) -> alloc::vec::Vec<u8> {
    let mut writer = BitWriter::default();
    let length = HEADER + block.len();
    writer.put(0xfff, 12);
    writer.put(0, 3);
    writer.put(1, 1);
    writer.put(u32::from(config.object - 1), 2);
    writer.put(config.rate_index as u32, 4);
    writer.put(0, 1);
    writer.put(config.channels as u32, 3);
    writer.put(0, 4);
    writer.put(length as u32, 13);
    writer.put(0x7ff, 11);
    writer.put(0, 2);
    let mut out = writer.finish();
    out.extend_from_slice(block);
    out
}
