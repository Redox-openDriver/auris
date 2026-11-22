use alloc::vec;
use alloc::vec::Vec;

use crate::bits::BitWriter;
use crate::config::Config;
use crate::decoder::{FRAME, Shapes};
use crate::syntax::{ESCAPE_BOOK, SCALE_OFFSET, Sequence, ZERO_BOOK};
use crate::tables;
use crate::transform::Mdct;

const SHORT: usize = 128;
const MAX_QUANTIZED: i32 = 8191;
fn signed(book: u8) -> bool {
    matches!(book, 1 | 2 | 5 | 6)
}

fn dimension(book: u8) -> usize {
    if book <= 4 { 4 } else { 2 }
}

fn symbol(book: u8, values: &[i32]) -> usize {
    match book {
        1 | 2 => values.iter().fold(0, |index, &value| index * 3 + (value + 1) as usize),
        3 | 4 => values.iter().fold(0, |index, &value| index * 3 + value.unsigned_abs() as usize),
        5 | 6 => values.iter().fold(0, |index, &value| index * 9 + (value + 4) as usize),
        7 | 8 => values.iter().fold(0, |index, &value| index * 8 + value.unsigned_abs() as usize),
        9 | 10 => values.iter().fold(0, |index, &value| index * 13 + value.unsigned_abs() as usize),
        _ => values.iter().fold(0, |index, &value| index * 17 + value.unsigned_abs().min(16) as usize),
    }
}

fn escape_bits(value: u32) -> u32 {
    if value < 16 {
        return 0;
    }
    let prefix = 31 - value.leading_zeros() - 4;
    2 * prefix + 5
}

fn write_escape(writer: &mut BitWriter, value: u32) {
    let prefix = 31 - value.leading_zeros() - 4;
    for _ in 0..prefix {
        writer.put(1, 1);
    }
    writer.put(0, 1);
    writer.put(value - (1 << (prefix + 4)), prefix + 4);
}

fn band_bits(book: u8, values: &[i32]) -> u32 {
    if book == ZERO_BOOK {
        return 0;
    }
    let (_, bits) = tables::SPECTRAL[usize::from(book) - 1];
    let mut total = 0u32;
    for chunk in values.chunks(dimension(book)) {
        total += u32::from(bits[symbol(book, chunk)]);
        if !signed(book) {
            total += chunk.iter().filter(|&&value| value != 0).count() as u32;
        }
        if book == ESCAPE_BOOK {
            total += chunk.iter().map(|value| escape_bits(value.unsigned_abs())).sum::<u32>();
        }
    }
    total
}

fn write_band(writer: &mut BitWriter, book: u8, values: &[i32]) {
    let (codes, bits) = tables::SPECTRAL[usize::from(book) - 1];
    for chunk in values.chunks(dimension(book)) {
        let index = symbol(book, chunk);
        writer.put(u32::from(codes[index]), u32::from(bits[index]));
        if !signed(book) {
            for &value in chunk.iter().filter(|&&value| value != 0) {
                writer.put(u32::from(value < 0), 1);
            }
        }
        if book == ESCAPE_BOOK {
            for &value in chunk {
                if value.unsigned_abs() >= 16 {
                    write_escape(writer, value.unsigned_abs());
                }
            }
        }
    }
}

fn books_for(largest: i32) -> &'static [u8] {
    match largest {
        0 => &[ZERO_BOOK],
        1 => &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        2 => &[3, 4, 5, 6, 7, 8, 9, 10, 11],
        3..=4 => &[5, 6, 7, 8, 9, 10, 11],
        5..=7 => &[7, 8, 9, 10, 11],
        8..=12 => &[9, 10, 11],
        _ => &[11],
    }
}

struct Band {
    largest: i32,
    costs: [u32; 12],
}

struct Coded {
    sections: Vec<(u8, usize, usize)>,
    books: Vec<u8>,
    bits: u32,
}

fn sectioning(bands: &[Band], short: bool) -> Coded {
    let count = bands.len();
    let (length_bits, escape) = if short { (3u32, 7usize) } else { (5u32, 31usize) };
    let header = |length: usize| 4 + length_bits * (1 + (length / escape) as u32);
    let mut best = vec![(u32::MAX, 0usize, ZERO_BOOK); count + 1];
    best[0] = (0, 0, ZERO_BOOK);
    for end in 1..=count {
        for start in (0..end).rev() {
            if best[start].0 == u32::MAX {
                continue;
            }
            let largest = bands[start..end].iter().map(|band| band.largest).max().unwrap_or(0);
            for &book in books_for(largest) {
                if book != ZERO_BOOK && bands[start..end].iter().any(|band| band.costs[usize::from(book)] == u32::MAX) {
                    continue;
                }
                let spectral: u32 = if book == ZERO_BOOK { 0 } else { bands[start..end].iter().map(|band| band.costs[usize::from(book)]).sum() };
                let total = best[start].0 + header(end - start) + spectral;
                if total < best[end].0 {
                    best[end] = (total, start, book);
                }
            }
            if end - start > 24 {
                break;
            }
        }
    }
    let mut sections = Vec::new();
    let mut end = count;
    while end > 0 {
        let (_, start, book) = best[end];
        sections.push((book, start, end));
        end = start;
    }
    sections.reverse();
    let mut books = vec![ZERO_BOOK; count];
    for &(book, start, end) in &sections {
        books[start..end].fill(book);
    }
    Coded { sections, books, bits: best[count].0 }
}

fn absolute_threshold(frequency: f32) -> f32 {
    let khz = (frequency / 1000.0).max(0.02);
    let spl = 3.64 * libm::powf(khz, -0.8) - 6.5 * libm::expf(-0.6 * (khz - 3.3) * (khz - 3.3)) + 0.001 * khz * khz * khz * khz;
    libm::powf(10.0, (spl.min(60.0) - 10.0 + 54.0) / 10.0)
}

struct Grouped {
    sequence: Sequence,
    groups: Vec<usize>,
    spectrum: Vec<f32>,
}

struct Plan {
    factors: Vec<i32>,
    quantized: Vec<i32>,
    coded: Coded,
    global: i32,
}

pub struct Encoder {
    config: Config,
    bitrate: u32,
    long: Mdct,
    short: Mdct,
    shapes: Shapes,
    pending: Vec<Vec<f32>>,
    frame: usize,
    previous: Sequence,
    attack_next: bool,
    reservoir: i32,
    cutoff: usize,
    largest: Option<usize>,
}

impl Encoder {
    pub fn new(config: Config, bitrate: u32) -> Encoder {
        let per_channel = bitrate / config.channels as u32;
        let hertz: f32 = match per_channel {
            0..=24_000 => 8000.0,
            24_001..=32_000 => 11000.0,
            32_001..=48_000 => 14000.0,
            48_001..=64_000 => 16500.0,
            64_001..=96_000 => 19000.0,
            _ => 20500.0,
        };
        let nyquist = config.rate() as f32 / 2.0;
        let cutoff = ((hertz.min(nyquist) / nyquist) * FRAME as f32) as usize;
        Encoder {
            config,
            bitrate,
            long: Mdct::new(2 * FRAME),
            short: Mdct::new(2 * SHORT),
            shapes: Shapes::default(),
            pending: vec![vec![0.0; FRAME]; config.channels],
            frame: 0,
            previous: Sequence::OnlyLong,
            attack_next: false,
            reservoir: 0,
            cutoff,
            largest: None,
        }
    }

    pub fn limit(&mut self, bytes: usize) {
        self.largest = Some(bytes);
    }

    pub fn config(&self) -> Config {
        self.config
    }

    fn attack(&self, channel: &[f32], center: usize) -> bool {
        if center < 448 || center + 576 > channel.len() {
            return false;
        }
        let start = center - 448;
        let mut energies = [0f32; 8];
        for (segment, energy) in energies.iter_mut().enumerate() {
            let base = start + segment * 128;
            for n in base..base + 128 {
                let previous = if n == 0 { 0.0 } else { channel[n - 1] };
                let difference = channel[n] - 0.6 * previous;
                *energy += difference * difference;
            }
        }
        let mut history = energies[0].max(1e4);
        for &energy in &energies[1..] {
            if energy > 10.0 * history && energy > 1e7 {
                return true;
            }
            history = 0.7 * history + 0.3 * energy;
        }
        false
    }

    fn analyse(&self, samples: &[f32], sequence: Sequence) -> Vec<f32> {
        let mut spectrum = vec![0f32; FRAME];
        let long = self.shapes.long(false);
        let short = self.shapes.short(false);
        if sequence == Sequence::EightShort {
            let mut windowed = vec![0f32; 2 * SHORT];
            for window in 0..8 {
                let base = 448 + window * SHORT;
                for i in 0..SHORT {
                    windowed[i] = samples[base + i] * short[i];
                    windowed[SHORT + i] = samples[base + SHORT + i] * short[SHORT - 1 - i];
                }
                self.short.forward(&windowed, &mut spectrum[window * SHORT..(window + 1) * SHORT]);
            }
            return spectrum;
        }
        let windowed: Vec<f32> = (0..2 * FRAME)
            .map(|n| {
                let window = match (sequence, n) {
                    (Sequence::OnlyLong | Sequence::LongStart, n) if n < FRAME => long[n],
                    (Sequence::OnlyLong | Sequence::LongStop, n) if n >= FRAME => long[2 * FRAME - 1 - n],
                    (Sequence::LongStart, n) if n < 1472 => 1.0,
                    (Sequence::LongStart, n) if n < 1600 => short[1599 - n],
                    (Sequence::LongStart, _) => 0.0,
                    (Sequence::LongStop, n) if n < 448 => 0.0,
                    (Sequence::LongStop, n) if n < 576 => short[n - 448],
                    _ => 1.0,
                };
                samples[n] * window
            })
            .collect();
        self.long.forward(&windowed, &mut spectrum);
        spectrum
    }

    fn thresholds(&self, grouped: &Grouped, offsets: &[u16], bands: usize) -> Vec<f32> {
        let short = grouped.sequence == Sequence::EightShort;
        let width_of_line = self.config.rate() as f32 / 2.0 / if short { SHORT as f32 } else { FRAME as f32 };
        let mut out = vec![0f32; grouped.groups.len() * bands];
        let mut first = 0;
        for (group, &length) in grouped.groups.iter().enumerate() {
            let mut previous = 0f32;
            for band in 0..bands {
                let (start, end) = (usize::from(offsets[band]), usize::from(offsets[band + 1]));
                let width = (end - start) as f32;
                let mut energy = 0f32;
                let mut log_sum = 0f32;
                for window in first..first + length {
                    let base = if short { window * SHORT } else { 0 };
                    for k in start..end {
                        let power = grouped.spectrum[base + k] * grouped.spectrum[base + k];
                        energy += power;
                        log_sum += libm::logf(power + 1.0);
                    }
                }
                let count = width * length as f32;
                let geometric = libm::expf(log_sum / count);
                let arithmetic = energy / count + 1.0;
                let flatness = 10.0 * libm::log10f((geometric / arithmetic).max(1e-10));
                let tonality = (flatness / -25.0).clamp(0.0, 1.0);
                let frequency = (start + end) as f32 / 2.0 * width_of_line;
                let ratio = tonality * (17.0 + frequency / 2000.0) + (1.0 - tonality) * 6.0;
                let masked = energy * libm::powf(10.0, -ratio / 10.0);
                let spread = previous * 0.08;
                let floor = absolute_threshold(frequency) * count;
                let threshold = masked.max(spread).max(floor);
                out[group * bands + band] = threshold;
                previous = masked;
            }
            first += length;
        }
        for group in 0..grouped.groups.len() {
            for band in (0..bands.saturating_sub(1)).rev() {
                let above = out[group * bands + band + 1] * 0.02;
                if above > out[group * bands + band] {
                    out[group * bands + band] = above;
                }
            }
        }
        out
    }

    fn quantize(value: f32, factor: i32) -> i32 {
        let step = libm::exp2f(-0.1875 * (factor - SCALE_OFFSET) as f32);
        let scaled = libm::powf(libm::fabsf(value), 0.75) * step;
        let quantized = (scaled + 0.4054) as i32;
        if value < 0.0 { -quantized } else { quantized }
    }

    fn plan(&self, grouped: &Grouped, offsets: &[u16], bands: usize, thresholds: &[f32], shift: i32) -> Plan {
        let short = grouped.sequence == Sequence::EightShort;
        let groups = grouped.groups.len();
        let mut factors = vec![0i32; groups * bands];
        let mut quantized = vec![0i32; FRAME];
        let mut descriptions = Vec::with_capacity(groups * bands);
        let mut first = 0;
        let mut last_factor: Option<i32> = None;
        for (group, &length) in grouped.groups.iter().enumerate() {
            for band in 0..bands {
                let index = group * bands + band;
                let (start, end) = (usize::from(offsets[band]), usize::from(offsets[band + 1]));
                let lines = |window: usize| {
                    let base = if short { window * SHORT } else { 0 };
                    base + start..base + end
                };
                let mut energy = 0f32;
                for window in first..first + length {
                    for k in lines(window) {
                        energy += grouped.spectrum[k] * grouped.spectrum[k];
                    }
                }
                let count = ((end - start) * length) as f32;
                let above_cutoff = !short && start >= self.cutoff || short && start * 8 >= self.cutoff;
                if energy <= 0.0 || above_cutoff || energy <= thresholds[index] {
                    let zeros = vec![0i32; end - start];
                    let mut costs = [u32::MAX; 12];
                    for book in 1..=11u8 {
                        costs[usize::from(book)] = band_bits(book, &zeros) * length as u32;
                    }
                    descriptions.push(Band { largest: 0, costs });
                    continue;
                }
                let rms = libm::sqrtf(energy / count);
                let allowed = libm::sqrtf(thresholds[index] / count);
                let estimate = 100.0 + 4.0 * libm::log2f((allowed * 2.598 / libm::powf(rms, 0.25)).max(1e-6));
                let mut factor = (estimate as i32 + shift).clamp(0, 255);
                if let Some(previous) = last_factor {
                    factor = factor.clamp(previous - 60, previous + 60);
                }
                loop {
                    let largest = (first..first + length).flat_map(lines).map(|k| Encoder::quantize(grouped.spectrum[k], factor).abs()).max().unwrap_or(0);
                    if largest <= MAX_QUANTIZED || factor >= 255 {
                        break;
                    }
                    factor += 1;
                }
                let mut values = Vec::with_capacity(count as usize);
                for window in first..first + length {
                    for k in lines(window) {
                        let value = Encoder::quantize(grouped.spectrum[k], factor);
                        quantized[k] = value;
                        values.push(value);
                    }
                }
                let largest = values.iter().map(|value| value.abs()).max().unwrap_or(0);
                let mut costs = [u32::MAX; 12];
                if largest > 0 {
                    for &book in books_for(largest) {
                        let width = end - start;
                        costs[usize::from(book)] = values.chunks(width).map(|window| band_bits(book, window)).sum();
                    }
                    factors[index] = factor;
                    last_factor = Some(factor);
                }
                descriptions.push(Band { largest, costs });
            }
            first += length;
        }
        let mut coded = Coded { sections: Vec::new(), books: Vec::new(), bits: 0 };
        for group in 0..groups {
            let part = sectioning(&descriptions[group * bands..(group + 1) * bands], short);
            coded.bits += part.bits;
            coded.sections.extend(part.sections.into_iter().map(|(book, start, end)| (book, group * bands + start, group * bands + end)));
            coded.books.extend(part.books);
        }
        let global = factors.iter().zip(&descriptions).find(|(_, band)| band.largest > 0).map_or(100, |(&factor, _)| factor);
        let mut carried = global;
        for (index, band) in descriptions.iter().enumerate() {
            if band.largest > 0 {
                carried = factors[index];
            } else {
                factors[index] = carried;
            }
        }
        let mut previous = global;
        for (factor, &book) in factors.iter().zip(&coded.books) {
            if book != ZERO_BOOK {
                let delta = (factor - previous + 60) as usize;
                coded.bits += u32::from(tables::SCALEFACTOR_BITS[delta]);
                previous = *factor;
            }
        }
        coded.bits += 8 + 3;
        Plan { factors, quantized, coded, global }
    }

    fn write_info(writer: &mut BitWriter, grouped: &Grouped, bands: usize) {
        writer.put(0, 1);
        writer.put(
            match grouped.sequence {
                Sequence::OnlyLong => 0,
                Sequence::LongStart => 1,
                Sequence::EightShort => 2,
                Sequence::LongStop => 3,
            },
            2,
        );
        writer.put(0, 1);
        if grouped.sequence == Sequence::EightShort {
            writer.put(bands as u32, 4);
            writer.put(0, 7);
        } else {
            writer.put(bands as u32, 6);
            writer.put(0, 1);
        }
    }

    fn write_stream(writer: &mut BitWriter, grouped: &Grouped, plan: &Plan, offsets: &[u16], bands: usize, with_info: bool) {
        let short = grouped.sequence == Sequence::EightShort;
        writer.put(plan.global as u32, 8);
        if with_info {
            Encoder::write_info(writer, grouped, bands);
        }
        let (length_bits, escape) = if short { (3u32, 7usize) } else { (5u32, 31usize) };
        for &(book, start, end) in &plan.coded.sections {
            writer.put(u32::from(book), 4);
            let mut length = end - start;
            while length >= escape {
                writer.put(escape as u32, length_bits);
                length -= escape;
            }
            writer.put(length as u32, length_bits);
        }
        let mut previous = plan.global;
        for (factor, &book) in plan.factors.iter().zip(&plan.coded.books) {
            if book != ZERO_BOOK {
                let delta = (factor - previous + 60) as usize;
                writer.put(tables::SCALEFACTOR_CODES[delta], u32::from(tables::SCALEFACTOR_BITS[delta]));
                previous = *factor;
            }
        }
        writer.put(0, 3);
        let mut first = 0;
        for (group, &length) in grouped.groups.iter().enumerate() {
            for band in 0..bands {
                let book = plan.coded.books[group * bands + band];
                if book == ZERO_BOOK {
                    continue;
                }
                let (start, end) = (usize::from(offsets[band]), usize::from(offsets[band + 1]));
                for window in first..first + length {
                    let base = if short { window * SHORT } else { 0 };
                    write_band(writer, book, &plan.quantized[base + start..base + end]);
                }
            }
            first += length;
        }
    }

    fn mid_side(&self, left: &mut Grouped, right: &mut Grouped, offsets: &[u16], bands: usize) -> Vec<bool> {
        let short = left.sequence == Sequence::EightShort;
        let mut used = vec![false; left.groups.len() * bands];
        let mut first = 0;
        for (group, &length) in left.groups.clone().iter().enumerate() {
            for band in 0..bands {
                let (start, end) = (usize::from(offsets[band]), usize::from(offsets[band + 1]));
                let (mut l, mut r, mut m, mut s) = (0f32, 0f32, 0f32, 0f32);
                for window in first..first + length {
                    let base = if short { window * SHORT } else { 0 };
                    for k in base + start..base + end {
                        let (a, b) = (left.spectrum[k], right.spectrum[k]);
                        l += a * a;
                        r += b * b;
                        m += (a + b) * (a + b) / 4.0;
                        s += (a - b) * (a - b) / 4.0;
                    }
                }
                if m * s < 0.2 * l * r {
                    used[group * bands + band] = true;
                    for window in first..first + length {
                        let base = if short { window * SHORT } else { 0 };
                        for k in base + start..base + end {
                            let (a, b) = (left.spectrum[k], right.spectrum[k]);
                            left.spectrum[k] = (a + b) / 2.0;
                            right.spectrum[k] = (a - b) / 2.0;
                        }
                    }
                }
            }
            first += length;
        }
        used
    }

    fn frame_block(&mut self, windows: &[Vec<f32>], sequence: Sequence) -> Vec<u8> {
        let short = sequence == Sequence::EightShort;
        let groups = if short { vec![1usize; 8] } else { vec![1usize] };
        let offsets = if short { tables::SWB_SHORT[self.config.rate_index] } else { tables::SWB_LONG[self.config.rate_index] };
        let bands = offsets.len() - 1;
        let mut grouped: Vec<Grouped> =
            windows.iter().map(|samples| Grouped { sequence, groups: groups.clone(), spectrum: self.analyse(samples, sequence) }).collect();
        let mut thresholds: Vec<Vec<f32>> = grouped.iter().map(|channel| self.thresholds(channel, offsets, bands)).collect();
        let mut used = Vec::new();
        if grouped.len() == 2 {
            let (first, second) = grouped.split_at_mut(1);
            used = self.mid_side(&mut first[0], &mut second[0], offsets, bands);
            for (index, flag) in used.iter().enumerate() {
                if *flag {
                    let lowest = thresholds[0][index].min(thresholds[1][index]) / 2.0;
                    thresholds[0][index] = lowest;
                    thresholds[1][index] = lowest;
                }
            }
        }
        let target = (u64::from(self.bitrate) * FRAME as u64 / u64::from(self.config.rate())) as i32;
        let mut budget = (target + self.reservoir.min(target / 2)).min(6144 * self.config.channels as i32 - 64);
        if let Some(largest) = self.largest {
            budget = budget.min(8 * largest as i32 - 16);
        }
        let overhead = if grouped.len() == 2 { 3 + 4 + 1 + 1 + 15 + 2 + used.len() as i32 } else { 3 + 4 + 15 } + 3;
        let plans_for =
            |shift: i32| -> Vec<Plan> { grouped.iter().zip(&thresholds).map(|(channel, limits)| self.plan(channel, offsets, bands, limits, shift)).collect() };
        let cost = |plans: &[Plan]| plans.iter().map(|plan| plan.coded.bits as i32).sum::<i32>() + overhead;
        let (mut low, mut high) = (-30, 90);
        let mut chosen = plans_for(high);
        if cost(&plans_for(low)) <= budget {
            chosen = plans_for(low);
        } else {
            while high - low > 1 {
                let middle = (low + high) / 2;
                let plans = plans_for(middle);
                if cost(&plans) <= budget {
                    high = middle;
                    chosen = plans;
                } else {
                    low = middle;
                }
            }
        }
        let used_bits = cost(&chosen);
        self.reservoir = (self.reservoir + target - used_bits).clamp(-target, 6144 * self.config.channels as i32 - target);
        let mut writer = BitWriter::default();
        if chosen.len() == 2 {
            writer.put(1, 3);
            writer.put(0, 4);
            writer.put(1, 1);
            Encoder::write_info(&mut writer, &grouped[0], bands);
            let any = used.iter().any(|&flag| flag);
            let all = used.iter().all(|&flag| flag);
            match (any, all) {
                (false, _) => writer.put(0, 2),
                (true, true) => writer.put(2, 2),
                (true, false) => {
                    writer.put(1, 2);
                    for &flag in &used {
                        writer.put(u32::from(flag), 1);
                    }
                }
            }
            for (channel, plan) in grouped.iter().zip(&chosen) {
                Encoder::write_stream(&mut writer, channel, plan, offsets, bands, false);
            }
        } else {
            writer.put(0, 3);
            writer.put(0, 4);
            Encoder::write_stream(&mut writer, &grouped[0], &chosen[0], offsets, bands, true);
        }
        writer.put(7, 3);
        writer.finish()
    }

    pub fn encode(&mut self, pcm: &[i16], blocks: &mut Vec<Vec<u8>>) {
        let channels = self.config.channels;
        for frame in pcm.chunks(channels) {
            if frame.len() < channels {
                break;
            }
            for (channel, &sample) in frame.iter().enumerate() {
                self.pending[channel].push(f32::from(sample));
            }
        }
        while self.pending[0].len() >= 3 * FRAME {
            let attack_now = self.attack_next;
            self.attack_next = self.pending.iter().any(|channel| self.attack(channel, 2 * FRAME));
            let attack_here = attack_now || self.pending.iter().any(|channel| self.attack(channel, FRAME));
            let sequence = match (self.previous, attack_here, self.attack_next) {
                (Sequence::LongStart | Sequence::EightShort, true, _) => Sequence::EightShort,
                (Sequence::EightShort, false, true) => Sequence::EightShort,
                (Sequence::EightShort, false, false) => Sequence::LongStop,
                (Sequence::LongStart, false, _) => Sequence::EightShort,
                (_, _, true) => Sequence::LongStart,
                (_, true, false) => Sequence::LongStart,
                _ => Sequence::OnlyLong,
            };
            let windows: Vec<Vec<f32>> = self.pending.iter().map(|channel| channel[..2 * FRAME].to_vec()).collect();
            blocks.push(self.frame_block(&windows, sequence));
            self.previous = sequence;
            for channel in &mut self.pending {
                channel.drain(..FRAME);
            }
            self.frame += 1;
        }
    }

    pub fn flush(&mut self, blocks: &mut Vec<Vec<u8>>) {
        let silence = vec![0i16; 2 * FRAME * self.config.channels];
        self.encode(&silence, blocks);
    }
}
