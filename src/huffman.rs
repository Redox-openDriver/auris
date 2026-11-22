use alloc::vec;
use alloc::vec::Vec;

use crate::bits::{BitReader, Exhausted};

pub struct Book {
    by_length: Vec<Vec<(u32, u16)>>,
    shortest: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bad {
    Exhausted,
    NoCode,
}

impl From<Exhausted> for Bad {
    fn from(_: Exhausted) -> Bad {
        Bad::Exhausted
    }
}

impl Book {
    pub fn new(codes: &[u32], lengths: &[u8]) -> Book {
        let longest = lengths.iter().copied().max().unwrap_or(0) as usize;
        let mut by_length = vec![Vec::new(); longest + 1];
        for (symbol, (&code, &length)) in codes.iter().zip(lengths).enumerate() {
            by_length[usize::from(length)].push((code, symbol as u16));
        }
        for row in &mut by_length {
            row.sort_unstable();
        }
        let shortest = lengths.iter().copied().min().unwrap_or(1) as u32;
        Book { by_length, shortest }
    }

    pub fn decode(&self, reader: &mut BitReader) -> Result<u16, Bad> {
        let mut code = reader.read(self.shortest)?;
        let mut length = self.shortest as usize;
        loop {
            if let Some(row) = self.by_length.get(length)
                && let Ok(at) = row.binary_search_by_key(&code, |(known, _)| *known)
            {
                return Ok(row[at].1);
            }
            if length + 1 >= self.by_length.len() {
                return Err(Bad::NoCode);
            }
            code = code << 1 | reader.read(1)?;
            length += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::BitWriter;
    use crate::tables;

    #[test]
    fn every_scalefactor_and_spectral_code_decodes_to_itself() {
        let scalefactors = Book::new(&tables::SCALEFACTOR_CODES, &tables::SCALEFACTOR_BITS);
        let mut writer = BitWriter::default();
        for (code, bits) in tables::SCALEFACTOR_CODES.iter().zip(tables::SCALEFACTOR_BITS) {
            writer.put(*code, u32::from(bits));
        }
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        for symbol in 0..121 {
            assert_eq!(scalefactors.decode(&mut reader), Ok(symbol));
        }
        for (codes, bits) in tables::SPECTRAL {
            let wide: Vec<u32> = codes.iter().map(|&code| u32::from(code)).collect();
            let book = Book::new(&wide, bits);
            let mut writer = BitWriter::default();
            for (code, length) in wide.iter().zip(bits) {
                writer.put(*code, u32::from(*length));
            }
            let bytes = writer.finish();
            let mut reader = BitReader::new(&bytes);
            for symbol in 0..codes.len() {
                assert_eq!(book.decode(&mut reader), Ok(symbol as u16));
            }
        }
    }
}
