//! WAV files of renders, to listen to (P0 of `docs/patch-graph.md` §11.6): the hashes say whether two renders are the same, and an ear says whether a render that is meant to be
//! another is a good one. `cargo test --release oracle::wav -- --ignored` writes the sessions of the corpus and the first generated ones, as the engine renders them, to
//! `target/oracle-wav/`.

use std::path::PathBuf;

use super::*;

/// A WAV file of interleaved stereo samples, 32-bit float: the samples as they are, with the ones that clip over full scale.
pub fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = samples.len() * 4;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&2u16.to_le_bytes()); // channels
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 8).to_le_bytes()); // bytes a second
    out.extend_from_slice(&8u16.to_le_bytes()); // bytes a frame
    out.extend_from_slice(&32u16.to_le_bytes()); // bits a sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

fn wav_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/oracle-wav")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_file_has_the_header_of_float_stereo_and_the_samples_after_it() {
        let bytes = wav_bytes(&[0.5, -0.25, 1.5, 0.0, -1.0, 0.125], 48_000);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let half = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(word(4) as usize, bytes.len() - 8, "the size of what follows the first two words");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!((word(16), half(20), half(22), word(24), word(28), half(32), half(34)), (16, 3, 2, 48_000, 384_000, 8, 32));
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(word(40), 24, "six samples of four bytes");
        assert_eq!(bytes.len(), 44 + 24);
        assert_eq!(f32::from_le_bytes(bytes[44..48].try_into().unwrap()), 0.5);
        assert_eq!(f32::from_le_bytes(bytes[52..56].try_into().unwrap()), 1.5, "over full scale as it is");
        assert_eq!(f32::from_le_bytes(bytes[64..68].try_into().unwrap()), 0.125);
    }

    #[test]
    #[ignore = "writes files: cargo test --release oracle::wav -- --ignored"]
    fn the_corpus_is_written_as_wav_files_to_listen_to() {
        let dir = wav_dir();
        std::fs::create_dir_all(&dir).expect("the directory");
        let mut scenarios = sessions();
        scenarios.extend(generated(0..8));
        for scenario in &scenarios {
            let rendered = render(scenario, &engine);
            let name = scenario.id.replace(':', "-");
            let path = dir.join(format!("{name}.wav"));
            std::fs::write(&path, wav_bytes(&rendered.samples, RATE as u32)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        eprintln!("oracle: {} files in {}", scenarios.len(), dir.display());
    }
}
