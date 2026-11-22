use alloc::vec;
use alloc::vec::Vec;

use crate::bits::{BitReader, Exhausted};
use crate::huffman::{self, Book};
use crate::tables;

pub const ZERO_BOOK: u8 = 0;
pub const ESCAPE_BOOK: u8 = 11;
pub const NOISE_BOOK: u8 = 13;
pub const INTENSITY_OUT_OF_PHASE: u8 = 14;
pub const INTENSITY_IN_PHASE: u8 = 15;
pub const SCALE_OFFSET: i32 = 100;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bad {
    Exhausted,
    NoCode,
    Reserved,
    Bands,
    Escape,
    Pulse,
    GainControl,
    Prediction,
    Element(u8),
    Channels,
    Config,
}

impl From<Exhausted> for Bad {
    fn from(_: Exhausted) -> Bad {
        Bad::Exhausted
    }
}

impl From<huffman::Bad> for Bad {
    fn from(bad: huffman::Bad) -> Bad {
        match bad {
            huffman::Bad::Exhausted => Bad::Exhausted,
            huffman::Bad::NoCode => Bad::NoCode,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sequence {
    OnlyLong,
    LongStart,
    EightShort,
    LongStop,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Info {
    pub sequence: Sequence,
    pub kbd: bool,
    pub max_sfb: usize,
    pub groups: Vec<usize>,
}

impl Info {
    pub fn short(&self) -> bool {
        self.sequence == Sequence::EightShort
    }

    pub fn windows(&self) -> usize {
        if self.short() { 8 } else { 1 }
    }

    pub fn offsets(&self, rate_index: usize) -> &'static [u16] {
        if self.short() { tables::SWB_SHORT[rate_index] } else { tables::SWB_LONG[rate_index] }
    }

    pub fn read(reader: &mut BitReader, rate_index: usize) -> Result<Info, Bad> {
        if reader.bit()? {
            return Err(Bad::Reserved);
        }
        let sequence = match reader.read(2)? {
            0 => Sequence::OnlyLong,
            1 => Sequence::LongStart,
            2 => Sequence::EightShort,
            _ => Sequence::LongStop,
        };
        let kbd = reader.bit()?;
        let (max_sfb, groups) = if sequence == Sequence::EightShort {
            let max_sfb = reader.read(4)? as usize;
            let grouping = reader.read(7)?;
            let mut groups = vec![1usize];
            for bit in (0..7).rev() {
                if grouping >> bit & 1 == 1 {
                    *groups.last_mut().expect("one group") += 1;
                } else {
                    groups.push(1);
                }
            }
            (max_sfb, groups)
        } else {
            let max_sfb = reader.read(6)? as usize;
            if reader.bit()? {
                return Err(Bad::Prediction);
            }
            (max_sfb, vec![1])
        };
        let info = Info { sequence, kbd, max_sfb, groups };
        if max_sfb > info.offsets(rate_index).len() - 1 {
            return Err(Bad::Bands);
        }
        Ok(info)
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Filter {
    pub length: usize,
    pub order: usize,
    pub downward: bool,
    pub coefficients: Vec<f32>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Stream {
    pub info: Info,
    pub global_gain: u8,
    pub books: Vec<u8>,
    pub factors: Vec<i32>,
    pub tns: Vec<Vec<Filter>>,
    pub quantized: Vec<i32>,
    pub pulsed: bool,
}

pub struct Books {
    scalefactors: Book,
    spectral: Vec<Book>,
}

impl Default for Books {
    fn default() -> Books {
        Books {
            scalefactors: Book::new(&tables::SCALEFACTOR_CODES, &tables::SCALEFACTOR_BITS),
            spectral: tables::SPECTRAL
                .iter()
                .map(|(codes, bits)| {
                    let wide: Vec<u32> = codes.iter().map(|&code| u32::from(code)).collect();
                    Book::new(&wide, bits)
                })
                .collect(),
        }
    }
}

fn tns_coefficients(raw: &[i32], bits: u32, resolution: u32) -> Vec<f32> {
    let half = core::f64::consts::PI / 2.0;
    let positive = ((1u32 << (resolution - 1)) as f64 - 0.5) / half;
    let negative = ((1u32 << (resolution - 1)) as f64 + 0.5) / half;
    let reflection: Vec<f64> = raw
        .iter()
        .map(|&value| {
            let signed = if value >= 1 << (bits - 1) { value - (1 << bits) } else { value };
            let divisor = if signed >= 0 { positive } else { negative };
            libm::sin(f64::from(signed) / divisor)
        })
        .collect();
    let order = reflection.len();
    let mut lpc = vec![0f64; order + 1];
    lpc[0] = 1.0;
    for m in 1..=order {
        let previous = lpc.clone();
        for i in 1..m {
            lpc[i] = previous[i] + reflection[m - 1] * previous[m - i];
        }
        lpc[m] = reflection[m - 1];
    }
    lpc[1..].iter().map(|&value| value as f32).collect()
}

impl Books {
    pub fn read_stream(&self, reader: &mut BitReader, rate_index: usize, shared: Option<&Info>) -> Result<Stream, Bad> {
        let global_gain = reader.read(8)? as u8;
        let info = match shared {
            Some(info) => info.clone(),
            None => Info::read(reader, rate_index)?,
        };
        let groups = info.groups.len();
        let bands = info.max_sfb;
        let mut books = vec![ZERO_BOOK; groups * bands];
        let (length_bits, escape) = if info.short() { (3, 7) } else { (5, 31) };
        for group in 0..groups {
            let mut band = 0;
            while band < bands {
                let book = reader.read(4)? as u8;
                if book == 12 {
                    return Err(Bad::Reserved);
                }
                let mut length = 0usize;
                loop {
                    let increment = reader.read(length_bits)? as usize;
                    length += increment;
                    if increment != escape {
                        break;
                    }
                }
                if band + length > bands || length == 0 {
                    return Err(Bad::Bands);
                }
                books[group * bands + band..group * bands + band + length].fill(book);
                band += length;
            }
        }
        let mut factors = vec![0i32; groups * bands];
        let mut scale = i32::from(global_gain);
        let mut position = 0i32;
        let mut noise = i32::from(global_gain) - 90;
        let mut first_noise = true;
        for (index, &book) in books.iter().enumerate() {
            factors[index] = match book {
                ZERO_BOOK => 0,
                INTENSITY_OUT_OF_PHASE | INTENSITY_IN_PHASE => {
                    position += i32::from(self.scalefactors.decode(reader)?) - 60;
                    position
                }
                NOISE_BOOK => {
                    if first_noise {
                        first_noise = false;
                        noise += reader.read(9)? as i32 - 256;
                    } else {
                        noise += i32::from(self.scalefactors.decode(reader)?) - 60;
                    }
                    noise
                }
                _ => {
                    scale += i32::from(self.scalefactors.decode(reader)?) - 60;
                    if !(0..=255).contains(&scale) {
                        return Err(Bad::Bands);
                    }
                    scale
                }
            };
        }
        let offsets = info.offsets(rate_index);
        let mut pulses = Vec::new();
        if reader.bit()? {
            if info.short() {
                return Err(Bad::Pulse);
            }
            let count = reader.read(2)? as usize + 1;
            let start = reader.read(6)? as usize;
            if start >= offsets.len() - 1 {
                return Err(Bad::Pulse);
            }
            let mut at = usize::from(offsets[start]);
            for _ in 0..count {
                at += reader.read(5)? as usize;
                let amplitude = reader.read(4)? as i32;
                if at >= 1024 {
                    return Err(Bad::Pulse);
                }
                pulses.push((at, amplitude));
            }
        }
        let mut tns = vec![Vec::new(); info.windows()];
        if reader.bit()? {
            let (count_bits, length_bits, order_bits) = if info.short() { (1, 4, 3) } else { (2, 6, 5) };
            for window in tns.iter_mut() {
                let filters = reader.read(count_bits)?;
                if filters == 0 {
                    continue;
                }
                let resolution = reader.read(1)? + 3;
                for _ in 0..filters {
                    let length = reader.read(length_bits)? as usize;
                    let order = reader.read(order_bits)? as usize;
                    let mut filter = Filter { length, order, ..Filter::default() };
                    if order > 0 {
                        filter.downward = reader.bit()?;
                        let compress = reader.read(1)?;
                        let bits = resolution - compress;
                        let raw = (0..order).map(|_| reader.read(bits).map(|value| value as i32)).collect::<Result<Vec<_>, _>>()?;
                        filter.coefficients = tns_coefficients(&raw, bits, resolution);
                    }
                    window.push(filter);
                }
            }
        }
        if reader.bit()? {
            return Err(Bad::GainControl);
        }
        let mut quantized = vec![0i32; 1024];
        let mut window_base = 0;
        for (group, &length) in info.groups.iter().enumerate() {
            for band in 0..bands {
                let book = books[group * bands + band];
                if book == ZERO_BOOK || book >= NOISE_BOOK {
                    continue;
                }
                let (start, end) = (usize::from(offsets[band]), usize::from(offsets[band + 1]));
                let table = &self.spectral[usize::from(book) - 1];
                for window in window_base..window_base + length {
                    let mut k = start;
                    while k < end {
                        let symbol = i32::from(table.decode(reader)?);
                        let values = unpack(book, symbol);
                        for (offset, &value) in values.iter().enumerate() {
                            let mut value = value;
                            if !signed(book) && value != 0 && reader.bit()? {
                                value = -value;
                            }
                            quantized[window * 128 * usize::from(info.short()) + k + offset] = value;
                        }
                        if book == ESCAPE_BOOK {
                            for offset in 0..values.len() {
                                let at = window * 128 * usize::from(info.short()) + k + offset;
                                if quantized[at].abs() == 16 {
                                    let sign = quantized[at].signum();
                                    quantized[at] = sign * escape_value(reader)?;
                                }
                            }
                        }
                        k += values.len();
                    }
                }
            }
            window_base += length;
        }
        let pulsed = !pulses.is_empty();
        for (at, amplitude) in pulses {
            quantized[at] += if quantized[at] > 0 { amplitude } else { -amplitude };
        }
        Ok(Stream { info, global_gain, books, factors, tns, quantized, pulsed })
    }
}

fn signed(book: u8) -> bool {
    matches!(book, 1 | 2 | 5 | 6)
}

fn unpack(book: u8, symbol: i32) -> alloc::vec::Vec<i32> {
    match book {
        1 | 2 => vec![symbol / 27 - 1, symbol / 9 % 3 - 1, symbol / 3 % 3 - 1, symbol % 3 - 1],
        3 | 4 => vec![symbol / 27, symbol / 9 % 3, symbol / 3 % 3, symbol % 3],
        5 | 6 => vec![symbol / 9 - 4, symbol % 9 - 4],
        7 | 8 => vec![symbol / 8, symbol % 8],
        9 | 10 => vec![symbol / 13, symbol % 13],
        _ => vec![symbol / 17, symbol % 17],
    }
}

fn escape_value(reader: &mut BitReader) -> Result<i32, Bad> {
    let mut prefix = 0;
    while reader.bit()? {
        prefix += 1;
        if prefix > 8 {
            return Err(Bad::Escape);
        }
    }
    let word = reader.read(prefix + 4)? as i32;
    Ok((1 << (prefix + 4)) + word)
}
