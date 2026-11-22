use auris::{Decoder, adts, latm};

fn decode_adts(stream: &[u8]) -> (Vec<i16>, auris::decoder::Tally) {
    let mut decoder: Option<Decoder> = None;
    let mut pcm = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let (config, block, length) = adts::parse(&stream[at..]).unwrap();
        decoder.get_or_insert_with(|| Decoder::new(config)).decode(block, &mut pcm).unwrap();
        at += length;
    }
    let tally = decoder.unwrap().tally();
    (pcm, tally)
}

fn samples(bytes: &[u8]) -> Vec<i16> {
    bytes.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect()
}

fn worst(a: &[i16], b: &[i16]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap()
}

#[test]
fn ffmpeg_streams_with_short_windows_tns_mid_side_and_intensity_decode_as_ffmpeg_decodes_them() {
    for (stream, expected) in [
        (&include_bytes!("data/ffmpeg-44k-stereo-128k.aac")[..], &include_bytes!("data/ffmpeg-44k-stereo-128k.pcm")[..]),
        (&include_bytes!("data/ffmpeg-22k-stereo-40k.aac")[..], &include_bytes!("data/ffmpeg-22k-stereo-40k.pcm")[..]),
    ] {
        let (pcm, tally) = decode_adts(stream);
        assert!(tally.short > 0 && tally.mid_side > 0 && tally.intensity > 0, "{tally:?}");
        assert!(worst(&pcm, &samples(expected)) <= 1);
    }
    let (_, tally) = decode_adts(include_bytes!("data/ffmpeg-44k-stereo-128k.aac"));
    assert!(tally.tns > 0 && tally.kbd > 0);
}

#[test]
fn noise_substitution_comes_out_as_loud_as_ffmpeg_makes_it() {
    let (pcm, tally) = decode_adts(include_bytes!("data/ffmpeg-44k-mono-32k-noise.aac"));
    assert!(tally.noise > 100, "{tally:?}");
    assert!(worst(&pcm, &samples(include_bytes!("data/ffmpeg-44k-mono-32k-noise.pcm"))) <= 1);
}

#[test]
fn the_same_blocks_in_latm_elements_decode_the_same() {
    let stream = include_bytes!("data/ffmpeg-44k-stereo-128k.aac");
    let (reference, _) = decode_adts(stream);
    let mut demuxer = latm::Demuxer::new(None);
    let mut decoder: Option<Decoder> = None;
    let mut pcm = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let (config, block, length) = adts::parse(&stream[at..]).unwrap();
        let element = latm::element(&config, block);
        let payload = demuxer.payload(&element, true).unwrap();
        assert_eq!(payload, block);
        decoder.get_or_insert_with(|| Decoder::new(demuxer.config().unwrap())).decode(&payload, &mut pcm).unwrap();
        at += length;
    }
    assert_eq!(pcm, reference);
}

#[test]
fn damaged_blocks_fail_cleanly() {
    let stream = include_bytes!("data/ffmpeg-44k-stereo-128k.aac");
    let (config, block, _) = adts::parse(stream).unwrap();
    let mut seed = 1u32;
    for round in 0..300 {
        let mut broken = block.to_vec();
        for _ in 0..1 + round % 5 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let at = (seed >> 8) as usize % broken.len();
            broken[at] ^= 1 << (seed % 8);
        }
        if round % 7 == 0 {
            broken.truncate((seed >> 4) as usize % broken.len());
        }
        let mut decoder = Decoder::new(config);
        let _ = decoder.decode(&broken, &mut Vec::new());
    }
    assert_eq!(adts::parse(&[0xff, 0xf1, 0x50]), Err(adts::Bad::Short));
    assert_eq!(adts::parse(&[0x12, 0x34, 0, 0, 0, 0, 0]), Err(adts::Bad::Sync));
}

#[test]
#[ignore = "needs ffmpeg"]
fn ffmpeg_decodes_what_auris_encodes() {
    use std::process::Command;
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("ffmpeg missing: skipped");
        return;
    }
    let config = auris::Config::new(48000, 2).unwrap();
    let pcm: Vec<i16> = (0..48000 * 2).map(|at| ((at / 2) as f32 * 0.0577).sin() as i16 * 9000 + (at % 2) as i16 * 700).collect();
    let mut encoder = auris::Encoder::new(config, 256_000);
    let mut blocks = Vec::new();
    encoder.encode(&pcm, &mut blocks);
    encoder.flush(&mut blocks);
    let stream: Vec<u8> = blocks.iter().flat_map(|block| adts::frame(&config, block)).collect();
    let path = std::env::temp_dir().join(format!("auris-{}.aac", std::process::id()));
    std::fs::write(&path, &stream).unwrap();
    let output = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-i"]).arg(&path).args(["-f", "s16le", "-"]).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(output.status.success() && output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
    let (ours, _) = decode_adts(&stream);
    let theirs = samples(&output.stdout);
    assert!(worst(&ours[..theirs.len().min(ours.len())], &theirs[..theirs.len().min(ours.len())]) <= 1);
}
