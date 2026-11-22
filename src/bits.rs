use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Exhausted;

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, at: 0 }
    }

    pub fn position(&self) -> usize {
        self.at
    }

    pub fn left(&self) -> usize {
        self.data.len() * 8 - self.at.min(self.data.len() * 8)
    }

    pub fn read(&mut self, count: u32) -> Result<u32, Exhausted> {
        if count == 0 {
            return Ok(0);
        }
        if self.left() < count as usize {
            return Err(Exhausted);
        }
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self.data[self.at / 8];
            value = value << 1 | u32::from(byte >> (7 - self.at % 8) & 1);
            self.at += 1;
        }
        Ok(value)
    }

    pub fn bit(&mut self) -> Result<bool, Exhausted> {
        Ok(self.read(1)? == 1)
    }

    pub fn skip(&mut self, count: usize) -> Result<(), Exhausted> {
        if self.left() < count {
            return Err(Exhausted);
        }
        self.at += count;
        Ok(())
    }

    pub fn align(&mut self) {
        self.at = self.at.div_ceil(8) * 8;
    }

    pub fn bytes(&mut self, count: usize) -> Result<&'a [u8], Exhausted> {
        self.align();
        let start = self.at / 8;
        let piece = self.data.get(start..start + count).ok_or(Exhausted)?;
        self.at += count * 8;
        Ok(piece)
    }
}

#[derive(Default, Clone)]
pub struct BitWriter {
    out: Vec<u8>,
    used: usize,
}

impl BitWriter {
    pub fn put(&mut self, value: u32, count: u32) {
        for shift in (0..count).rev() {
            if self.used.is_multiple_of(8) {
                self.out.push(0);
            }
            if value >> shift & 1 == 1 {
                *self.out.last_mut().expect("pushed") |= 0x80 >> (self.used % 8);
            }
            self.used += 1;
        }
    }

    pub fn bits(&self) -> usize {
        self.used
    }

    pub fn align(&mut self) {
        let pad = (8 - self.used % 8) % 8;
        self.put(0, pad as u32);
    }

    pub fn append(&mut self, other: &BitWriter) {
        let mut reader = BitReader::new(&other.out);
        let mut left = other.used;
        while left > 0 {
            let take = left.min(24) as u32;
            self.put(reader.read(take).expect("inside"), take);
            left -= take as usize;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_across_byte_edges() {
        let mut writer = BitWriter::default();
        writer.put(0b101, 3);
        writer.put(0x1ffff, 17);
        writer.put(0, 1);
        let mut tail = BitWriter::default();
        tail.put(0b11, 2);
        writer.append(&tail);
        assert_eq!(writer.bits(), 23);
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        assert_eq!((reader.read(3), reader.read(17), reader.bit(), reader.read(2)), (Ok(0b101), Ok(0x1ffff), Ok(false), Ok(0b11)));
        reader.align();
        assert_eq!(reader.read(1), Err(Exhausted));
    }
}
