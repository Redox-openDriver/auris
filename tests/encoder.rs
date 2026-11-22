use std::f32::consts::PI;

use auris::{Config, Decoder, Encoder};

fn signal(rate: u32, seconds: f32, channels: usize) -> Vec<i16> {
    let frames = (rate as f32 * seconds) as usize;
    let mut seed = 11u32;
    let mut out = Vec::with_capacity(frames * channels);
    for t in 0..frames {
        let x = t as f32 / rate as f32;
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let noise = (seed >> 16) as f32 / 65_536.0 - 0.5;
        let burst = if (0.5..0.52).contains(&x) { 12000.0 * (2.0 * PI * 2500.0 * x).sin() } else { 0.0 };
        let left = 8000.0 * (2.0 * PI * 440.0 * x).sin() + 3000.0 * (2.0 * PI * 3100.0 * x).sin() + 300.0 * noise + burst;
        let right = 7000.0 * (2.0 * PI * 330.0 * x).sin() + 2000.0 * (2.0 * PI * 1250.0 * x).sin();
        out.push(left as i16);
        if channels == 2 {
            out.push(right as i16);
        }
    }
    out
}

fn round_trip(config: Config, bitrate: u32, pcm: &[i16], chunk: usize) -> (Vec<i16>, usize) {
    let mut encoder = Encoder::new(config, bitrate);
    let mut blocks = Vec::new();
    for piece in pcm.chunks(chunk * config.channels) {
        encoder.encode(piece, &mut blocks);
    }
    encoder.flush(&mut blocks);
    let bytes: usize = blocks.iter().map(Vec::len).sum();
    let mut decoder = Decoder::new(config);
    let mut decoded = Vec::new();
    for block in &blocks {
        decoder.decode(block, &mut decoded).unwrap();
    }
    (decoded, bytes)
}

fn snr(reference: &[i16], decoded: &[i16], channels: usize) -> f64 {
    let delay = 1024 * channels;
    let (mut signal, mut noise) = (0f64, 0f64);
    for at in 4096 * channels..reference.len() - 4096 * channels {
        let (want, got) = (f64::from(reference[at]), f64::from(decoded[at + delay]));
        signal += want * want;
        noise += (want - got) * (want - got);
    }
    10.0 * (signal / noise).log10()
}

#[test]
fn stereo_at_a2dp_bitrates_comes_back_clean_and_on_budget() {
    let config = Config::new(44100, 2).unwrap();
    let pcm = signal(44100, 2.0, 2);
    for (bitrate, floor) in [(320_000u32, 28.0), (256_000, 27.0), (128_000, 18.0)] {
        let (decoded, bytes) = round_trip(config, bitrate, &pcm, 1024);
        let kbps = bytes as f64 * 8.0 / 2.0 / 1000.0;
        assert!((kbps - f64::from(bitrate) / 1000.0).abs() < f64::from(bitrate) / 1000.0 * 0.12, "{bitrate}: {kbps}");
        let measured = snr(&pcm, &decoded, 2);
        assert!(measured > floor, "{bitrate}: {measured}");
    }
}

#[test]
fn every_sample_rate_and_mono_encode_and_decode() {
    for (rate, channels) in [(48000, 1), (32000, 2), (22050, 1), (16000, 2), (8000, 1), (96000, 2)] {
        let config = Config::new(rate, channels).unwrap();
        let pcm = signal(rate, 2.5, channels);
        let (decoded, _) = round_trip(config, rate * channels as u32 * 2, &pcm, 777);
        let measured = snr(&pcm, &decoded, channels);
        assert!(measured > 12.0, "{rate} {channels}: {measured}");
    }
}

#[test]
fn chunking_does_not_change_the_stream() {
    let config = Config::new(48000, 2).unwrap();
    let pcm = signal(48000, 0.5, 2);
    let encode = |chunk: usize| {
        let mut encoder = Encoder::new(config, 256_000);
        let mut blocks = Vec::new();
        for piece in pcm.chunks(chunk * 2) {
            encoder.encode(piece, &mut blocks);
        }
        blocks
    };
    assert_eq!(encode(1), encode(4096));
    assert_eq!(encode(333), encode(1024));
}

#[test]
fn silence_and_full_scale_square_waves_stay_sane() {
    let config = Config::new(44100, 2).unwrap();
    let (decoded, bytes) = round_trip(config, 256_000, &vec![0i16; 44100 * 2], 1024);
    assert!(decoded.iter().all(|&sample| sample == 0));
    assert!(bytes < 44100 / 1024 * 20);
    let square: Vec<i16> = (0..44100 * 2).map(|at| if at / 100 % 2 == 0 { i16::MAX } else { -i16::MAX }).collect();
    let (decoded, _) = round_trip(config, 320_000, &square, 1024);
    let flipped = decoded[8192..80000].iter().zip(&square[8192 - 2048..80000 - 2048]).filter(|(a, b)| (i32::from(**a) - i32::from(**b)).abs() > 40000).count();
    assert_eq!(flipped, 0);
}

#[test]
fn a_byte_limit_keeps_every_frame_inside_it() {
    let config = Config::new(44100, 2).unwrap();
    let pcm = signal(44100, 1.0, 2);
    let mut encoder = Encoder::new(config, 320_000);
    encoder.limit(700);
    let mut blocks = Vec::new();
    encoder.encode(&pcm, &mut blocks);
    assert!(blocks.iter().all(|block| block.len() <= 700), "{:?}", blocks.iter().map(Vec::len).max());
}
