use alloc::vec;
use alloc::vec::Vec;

use crate::bits::BitReader;
use crate::config::Config;
use crate::syntax::{Bad, Books, INTENSITY_IN_PHASE, INTENSITY_OUT_OF_PHASE, Info, NOISE_BOOK, SCALE_OFFSET, Sequence, Stream, ZERO_BOOK};
use crate::tables;
use crate::transform::{Mdct, kbd_window, sine_window};

pub const FRAME: usize = 1024;
const SHORT: usize = 128;

pub struct Shapes {
    pub sine_long: Vec<f32>,
    pub kbd_long: Vec<f32>,
    pub sine_short: Vec<f32>,
    pub kbd_short: Vec<f32>,
}

impl Default for Shapes {
    fn default() -> Shapes {
        Shapes {
            sine_long: sine_window(2 * FRAME),
            kbd_long: kbd_window(2 * FRAME, 4.0),
            sine_short: sine_window(2 * SHORT),
            kbd_short: kbd_window(2 * SHORT, 6.0),
        }
    }
}

impl Shapes {
    pub fn long(&self, kbd: bool) -> &[f32] {
        if kbd { &self.kbd_long } else { &self.sine_long }
    }

    pub fn short(&self, kbd: bool) -> &[f32] {
        if kbd { &self.kbd_short } else { &self.sine_short }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    pub frames: usize,
    pub short: usize,
    pub kbd: usize,
    pub tns: usize,
    pub mid_side: usize,
    pub intensity: usize,
    pub noise: usize,
    pub pulses: usize,
}

struct State {
    overlap: Vec<f32>,
    kbd: bool,
}

pub struct Decoder {
    config: Config,
    books: Books,
    long: Mdct,
    short: Mdct,
    shapes: Shapes,
    states: Vec<State>,
    power: Vec<f32>,
    seed: u32,
    tally: Tally,
}

fn gain(factor: i32) -> f32 {
    libm::exp2f(0.25 * (factor - SCALE_OFFSET) as f32)
}

impl Decoder {
    pub fn new(config: Config) -> Decoder {
        Decoder {
            config,
            books: Books::default(),
            long: Mdct::new(2 * FRAME),
            short: Mdct::new(2 * SHORT),
            shapes: Shapes::default(),
            states: (0..config.channels).map(|_| State { overlap: vec![0.0; FRAME], kbd: false }).collect(),
            power: (0..8192).map(|value| libm::pow(f64::from(value), 4.0 / 3.0) as f32).collect(),
            seed: 0x1f2e_3d4c,
            tally: Tally::default(),
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn tally(&self) -> Tally {
        self.tally
    }

    fn count(&mut self, stream: &Stream) {
        self.tally.short += usize::from(stream.info.short());
        self.tally.kbd += usize::from(stream.info.kbd);
        self.tally.tns += stream.tns.iter().flatten().filter(|filter| filter.order > 0).count();
        self.tally.intensity += stream.books.iter().filter(|&&book| book == INTENSITY_IN_PHASE || book == INTENSITY_OUT_OF_PHASE).count();
        self.tally.noise += stream.books.iter().filter(|&&book| book == NOISE_BOOK).count();
        self.tally.pulses += usize::from(stream.pulsed);
    }

    fn random(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.seed as i32 as f32
    }

    fn bands<'a>(&self, info: &'a Info) -> impl Iterator<Item = (usize, usize, usize, usize, usize)> + 'a {
        let offsets = info.offsets(self.config.rate_index);
        let max_sfb = info.max_sfb;
        let short = info.short();
        let mut first = 0;
        info.groups.iter().enumerate().flat_map(move |(group, &length)| {
            let windows = first..first + length;
            first += length;
            (0..max_sfb).flat_map(move |band| {
                windows.clone().map(move |window| {
                    let base = if short { window * SHORT } else { 0 };
                    (group * max_sfb + band, window, base + usize::from(offsets[band]), base + usize::from(offsets[band + 1]), group)
                })
            })
        })
    }

    fn dequantize(&mut self, stream: &Stream) -> Result<Vec<f32>, Bad> {
        let mut spectrum = vec![0f32; FRAME];
        let bands: Vec<_> = self.bands(&stream.info).collect();
        for (index, _, start, end, _) in bands {
            let book = stream.books[index];
            match book {
                ZERO_BOOK | INTENSITY_IN_PHASE | INTENSITY_OUT_OF_PHASE => {}
                NOISE_BOOK => {
                    let mut energy = 0f32;
                    for value in &mut spectrum[start..end] {
                        *value = self.random();
                        energy += *value * *value;
                    }
                    let scale = libm::exp2f(0.25 * stream.factors[index] as f32) / libm::sqrtf(energy.max(f32::MIN_POSITIVE));
                    spectrum[start..end].iter_mut().for_each(|value| *value *= scale);
                }
                _ => {
                    let scale = gain(stream.factors[index]);
                    for (value, &quantized) in spectrum[start..end].iter_mut().zip(&stream.quantized[start..end]) {
                        let magnitude = *self.power.get(quantized.unsigned_abs() as usize).ok_or(Bad::Escape)?;
                        *value = magnitude.copysign(quantized as f32) * scale;
                    }
                }
            }
        }
        Ok(spectrum)
    }

    fn stereo(&self, left: &Stream, right: &Stream, mask: u8, used: &[bool], spectra: &mut [Vec<f32>]) {
        let bands: Vec<_> = self.bands(&right.info).collect();
        let (first, second) = spectra.split_at_mut(1);
        let (l, r) = (&mut first[0], &mut second[0]);
        for (index, _, start, end, _) in bands {
            let ms = mask == 2 || (mask == 1 && used[index]);
            match right.books[index] {
                INTENSITY_IN_PHASE | INTENSITY_OUT_OF_PHASE => {
                    let direction = if right.books[index] == INTENSITY_IN_PHASE { 1.0 } else { -1.0 };
                    let invert = if mask == 1 && used[index] { -1.0 } else { 1.0 };
                    let scale = libm::exp2f(-0.25 * right.factors[index] as f32) * direction * invert;
                    for k in start..end {
                        r[k] = l[k] * scale;
                    }
                }
                NOISE_BOOK if left.books[index] == NOISE_BOOK && ms => {
                    let left_energy: f32 = l[start..end].iter().map(|value| value * value).sum();
                    let target = libm::exp2f(0.25 * right.factors[index] as f32);
                    let scale = target / libm::sqrtf(left_energy.max(f32::MIN_POSITIVE));
                    for k in start..end {
                        r[k] = l[k] * scale;
                    }
                }
                NOISE_BOOK => {}
                _ if ms && left.books[index] != NOISE_BOOK => {
                    for k in start..end {
                        let (mid, side) = (l[k], r[k]);
                        l[k] = mid + side;
                        r[k] = mid - side;
                    }
                }
                _ => {}
            }
        }
    }

    fn temporal_shaping(&self, stream: &Stream, spectrum: &mut [f32]) {
        let info = &stream.info;
        let offsets = info.offsets(self.config.rate_index);
        let all = offsets.len() - 1;
        let limit = if info.short() { tables::TNS_MAX_BANDS_SHORT } else { tables::TNS_MAX_BANDS_LONG }[self.config.rate_index] as usize;
        let ceiling = limit.min(info.max_sfb);
        for (window, filters) in stream.tns.iter().enumerate() {
            let base = if info.short() { window * SHORT } else { 0 };
            let mut bottom = all;
            for filter in filters {
                let top = bottom;
                bottom = top.saturating_sub(filter.length);
                if filter.order == 0 {
                    continue;
                }
                let start = base + usize::from(offsets[bottom.min(ceiling)]);
                let end = base + usize::from(offsets[top.min(ceiling)]);
                if end <= start {
                    continue;
                }
                let lpc = &filter.coefficients;
                let mut state = vec![0f32; lpc.len()];
                let indices: Vec<usize> = if filter.downward { (start..end).rev().collect() } else { (start..end).collect() };
                for index in indices {
                    let mut value = spectrum[index];
                    for (coefficient, past) in lpc.iter().zip(&state) {
                        value -= coefficient * past;
                    }
                    state.rotate_right(1);
                    state[0] = value;
                    spectrum[index] = value;
                }
            }
        }
    }

    fn synthesize(&mut self, channel: usize, info: &Info, spectrum: &[f32], out: &mut [f32]) {
        let previous = self.states[channel].kbd;
        let shapes = &self.shapes;
        let mut z = vec![0f32; 2 * FRAME];
        if info.short() {
            let mut back = vec![0f32; 2 * SHORT];
            for window in 0..8 {
                self.short.inverse(&spectrum[window * SHORT..(window + 1) * SHORT], &mut back);
                let rising = shapes.short(if window == 0 { previous } else { info.kbd });
                let falling = shapes.short(info.kbd);
                let base = 448 + window * SHORT;
                for i in 0..SHORT {
                    z[base + i] += back[i] * rising[i];
                    z[base + SHORT + i] += back[SHORT + i] * falling[SHORT - 1 - i];
                }
            }
        } else {
            let mut back = vec![0f32; 2 * FRAME];
            self.long.inverse(spectrum, &mut back);
            let long_rising = shapes.long(previous);
            let long_falling = shapes.long(info.kbd);
            let short_rising = shapes.short(previous);
            let short_falling = shapes.short(info.kbd);
            for n in 0..2 * FRAME {
                let window = match (info.sequence, n) {
                    (Sequence::OnlyLong | Sequence::LongStart, n) if n < FRAME => long_rising[n],
                    (Sequence::OnlyLong | Sequence::LongStop, n) if n >= FRAME => long_falling[2 * FRAME - 1 - n],
                    (Sequence::LongStart, n) if n < 1472 => 1.0,
                    (Sequence::LongStart, n) if n < 1600 => short_falling[1599 - n],
                    (Sequence::LongStart, _) => 0.0,
                    (Sequence::LongStop, n) if n < 448 => 0.0,
                    (Sequence::LongStop, n) if n < 576 => short_rising[n - 448],
                    _ => 1.0,
                };
                z[n] = back[n] * window;
            }
        }
        let state = &mut self.states[channel];
        for n in 0..FRAME {
            out[n] = z[n] + state.overlap[n];
        }
        state.overlap.copy_from_slice(&z[FRAME..]);
        state.kbd = info.kbd;
    }

    pub fn decode(&mut self, block: &[u8], pcm: &mut Vec<i16>) -> Result<(), Bad> {
        let mut reader = BitReader::new(block);
        let rate = self.config.rate_index;
        let mut channels: Vec<(Info, Vec<f32>)> = Vec::new();
        loop {
            match reader.read(3)? {
                0 | 3 => {
                    reader.read(4)?;
                    let stream = self.books.read_stream(&mut reader, rate, None)?;
                    self.count(&stream);
                    let mut spectrum = self.dequantize(&stream)?;
                    self.temporal_shaping(&stream, &mut spectrum);
                    channels.push((stream.info, spectrum));
                }
                1 => {
                    reader.read(4)?;
                    let common = reader.bit()?;
                    let (shared, mask, used) = if common {
                        let info = Info::read(&mut reader, rate)?;
                        let mask = reader.read(2)? as u8;
                        if mask == 3 {
                            return Err(Bad::Reserved);
                        }
                        let mut used = vec![false; info.groups.len() * info.max_sfb];
                        if mask == 1 {
                            for flag in used.iter_mut() {
                                *flag = reader.bit()?;
                            }
                        }
                        (Some(info), mask, used)
                    } else {
                        (None, 0, Vec::new())
                    };
                    let left = self.books.read_stream(&mut reader, rate, shared.as_ref())?;
                    let right = self.books.read_stream(&mut reader, rate, shared.as_ref())?;
                    self.count(&left);
                    self.count(&right);
                    self.tally.mid_side += if mask == 2 { used.len() } else { used.iter().filter(|&&flag| flag).count() };
                    let mut spectra = vec![self.dequantize(&left)?, self.dequantize(&right)?];
                    if common {
                        self.stereo(&left, &right, mask, &used, &mut spectra);
                    }
                    let right_spectrum = spectra.pop().expect("two");
                    let left_spectrum = spectra.pop().expect("two");
                    let (mut left_spectrum, mut right_spectrum) = (left_spectrum, right_spectrum);
                    self.temporal_shaping(&left, &mut left_spectrum);
                    self.temporal_shaping(&right, &mut right_spectrum);
                    channels.push((left.info, left_spectrum));
                    channels.push((right.info, right_spectrum));
                }
                4 => {
                    reader.read(4)?;
                    let aligned = reader.bit()?;
                    let mut count = reader.read(8)? as usize;
                    if count == 255 {
                        count += reader.read(8)? as usize;
                    }
                    if aligned {
                        reader.align();
                    }
                    reader.skip(8 * count)?;
                }
                6 => {
                    let mut count = reader.read(4)? as usize;
                    if count == 15 {
                        count += reader.read(8)? as usize - 1;
                    }
                    reader.skip(8 * count)?;
                }
                7 => break,
                other => return Err(Bad::Element(other as u8)),
            }
        }
        if channels.len() != self.config.channels {
            return Err(Bad::Channels);
        }
        self.tally.frames += 1;
        let mut outputs = vec![vec![0f32; FRAME]; channels.len()];
        for (channel, (info, spectrum)) in channels.iter().enumerate() {
            let mut out = vec![0f32; FRAME];
            self.synthesize(channel, info, spectrum, &mut out);
            outputs[channel] = out;
        }
        pcm.reserve(FRAME * outputs.len());
        for n in 0..FRAME {
            for output in &outputs {
                pcm.push(libm::roundf(output[n]).clamp(-32768.0, 32767.0) as i16);
            }
        }
        Ok(())
    }
}
