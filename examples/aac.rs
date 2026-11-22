use std::io::{Read, Write};

use auris::{Decoder, adts};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let mut output = Vec::new();
    match args.first().map(String::as_str) {
        Some("decode") => {
            let mut decoder: Option<Decoder> = None;
            let mut at = 0;
            let mut frames = 0;
            while at < input.len() {
                let (config, block, length) = adts::parse(&input[at..]).unwrap();
                let decoder = decoder.get_or_insert_with(|| Decoder::new(config));
                let mut pcm = Vec::new();
                if let Err(e) = decoder.decode(block, &mut pcm) {
                    panic!("frame {frames}: {e:?}");
                }
                for sample in pcm {
                    output.extend_from_slice(&sample.to_le_bytes());
                }
                at += length;
                frames += 1;
            }
            if let Some(decoder) = decoder {
                eprintln!("{:?}", decoder.tally());
            }
        }
        Some("decode-loas") => {
            let mut demuxer = auris::latm::Demuxer::new(None);
            let mut decoder: Option<Decoder> = None;
            let mut at = 0;
            while at < input.len() {
                let (element, length) = auris::latm::strip_loas(&input[at..]).unwrap();
                let payload = demuxer.payload(element, true).unwrap();
                let config = demuxer.config().unwrap();
                let decoder = decoder.get_or_insert_with(|| Decoder::new(config));
                let mut pcm = Vec::new();
                decoder.decode(&payload, &mut pcm).unwrap();
                for sample in pcm {
                    output.extend_from_slice(&sample.to_le_bytes());
                }
                at += length;
            }
        }
        Some("encode") => {
            let number = |at: usize, default: u32| args.get(at).and_then(|value| value.parse().ok()).unwrap_or(default);
            let config = auris::Config::new(number(1, 44100), number(2, 2) as usize).unwrap();
            let mut encoder = auris::Encoder::new(config, number(3, 256_000));
            let pcm: Vec<i16> = input.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect();
            let mut blocks = Vec::new();
            encoder.encode(&pcm, &mut blocks);
            encoder.flush(&mut blocks);
            for block in blocks {
                output.extend(adts::frame(&config, &block));
            }
        }
        _ => panic!("decode < in.aac > out.raw | decode-loas < in.latm > out.raw | encode RATE CHANNELS BITRATE < in.raw > out.aac"),
    }
    std::io::stdout().write_all(&output).unwrap();
}
