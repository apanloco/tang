//! A delay line: what is put in comes out again a number of frames later. It is what a path of less latency is put in step with a path of more by (`with_delays`), and what the dry
//! part of an effect that has latency is put in step with its wet part by.

use crate::exec::ChanBuf;

/// A ring of `frames` frames for each channel. A block of any length goes through it in place: what comes out is what went in `frames` frames before (silence for the first
/// `frames`), whatever the lengths of the blocks that came in between. It is made, and `resize`d, on a thread that may allocate; `run` allocates nothing.
#[derive(Clone, Debug)]
pub struct DelayLine {
    /// The ring of each channel: `frames` samples, the one at `at` the oldest.
    rings: Vec<Vec<f32>>,
    /// Where the oldest sample of every ring is.
    at: usize,
}

impl DelayLine {
    /// A line of `frames` frames for each of `channels` channels, full of silence. A line of no frames lets everything through as it is.
    pub fn new(frames: usize, channels: usize) -> Self {
        DelayLine { rings: vec![vec![0.0; frames]; channels], at: 0 }
    }

    /// How many frames later what comes out is than what went in.
    pub fn frames(&self) -> usize {
        self.rings.first().map_or(0, Vec::len)
    }

    /// How many channels the line has.
    pub fn channels(&self) -> usize {
        self.rings.len()
    }

    /// Put the block through the line, in place: each channel of `buffer` is what that channel of the line had, and the line has what `buffer` was. A buffer with more channels than
    /// the line has leaves the others as they are.
    pub fn run(&mut self, buffer: &mut ChanBuf) {
        let frames = self.frames();
        let len = buffer.first().map_or(0, Vec::len);
        if frames == 0 {
            return;
        }
        for (ring, channel) in self.rings.iter_mut().zip(buffer.iter_mut()) {
            let mut at = self.at;
            for sample in channel.iter_mut() {
                std::mem::swap(sample, &mut ring[at]);
                at += 1;
                if at == frames {
                    at = 0;
                }
            }
        }
        self.at = (self.at + len) % frames;
    }

    /// Make the line `frames` long, keeping what it has of the newest samples: a longer line has silence before them, a shorter one has the last `frames` of them.
    pub fn resize(&mut self, frames: usize) {
        let old = self.frames();
        if frames == old {
            return;
        }
        for ring in &mut self.rings {
            // In time order, the oldest first.
            let mut time: Vec<f32> = Vec::with_capacity(frames.max(old));
            time.extend_from_slice(&ring[self.at..]);
            time.extend_from_slice(&ring[..self.at]);
            *ring = if frames > old {
                let mut longer = vec![0.0; frames - old];
                longer.extend_from_slice(&time);
                longer
            } else {
                time.split_off(old - frames)
            };
        }
        self.at = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp 1, 2, 3, … of `n` frames from `from`, for `channels` channels (a channel is the ramp plus its number times 1000).
    fn ramp(from: usize, n: usize, channels: usize) -> ChanBuf {
        (0..channels).map(|c| (from..from + n).map(|i| (i + 1) as f32 + c as f32 * 1000.0).collect()).collect()
    }

    /// Everything that comes out of a line of `frames` when ramps of the lengths given go in, as one run of samples for channel 0.
    fn out_of(frames: usize, lengths: &[usize]) -> Vec<f32> {
        let mut line = DelayLine::new(frames, 1);
        let (mut from, mut all) = (0, Vec::new());
        for &n in lengths {
            let mut block = ramp(from, n, 1);
            line.run(&mut block);
            all.extend_from_slice(&block[0]);
            from += n;
        }
        all
    }

    #[test]
    fn what_goes_in_comes_out_the_number_of_frames_later_with_silence_first() {
        let out = out_of(3, &[8]);
        assert_eq!(out, [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn it_does_not_matter_how_the_frames_are_cut_into_blocks() {
        let whole = out_of(5, &[40]);
        for lengths in [[1usize; 40].as_slice(), &[3, 7, 11, 19], &[5, 5, 5, 5, 5, 5, 5, 5], &[4, 6, 30], &[6, 34], &[2, 38], &[39, 1]] {
            assert_eq!(out_of(5, lengths), whole, "{lengths:?}");
        }
    }

    #[test]
    fn a_block_longer_than_the_line_goes_through_it_too() {
        // A line of 2 frames and a block of 6: the block's own first frames are not read back before the line's silence has gone.
        assert_eq!(out_of(2, &[6]), [0.0, 0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn a_line_of_no_frames_lets_everything_through_as_it_is() {
        assert_eq!(out_of(0, &[4, 4]), [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn every_channel_is_delayed_on_its_own() {
        let mut line = DelayLine::new(2, 2);
        let mut block = ramp(0, 4, 2);
        line.run(&mut block);
        assert_eq!(block, [vec![0.0, 0.0, 1.0, 2.0], vec![0.0, 0.0, 1001.0, 1002.0]]);
        let mut next = ramp(4, 2, 2);
        line.run(&mut next);
        assert_eq!(next, [vec![3.0, 4.0], vec![1003.0, 1004.0]]);
    }

    #[test]
    fn the_signs_of_zeros_and_the_bits_of_what_goes_in_are_what_comes_out() {
        let mut line = DelayLine::new(1, 1);
        let mut block: ChanBuf = vec![vec![-0.0, 0.1, f32::MIN_POSITIVE / 4.0]];
        line.run(&mut block);
        assert_eq!(block[0][0].to_bits(), 0.0f32.to_bits(), "silence at first is a positive zero");
        assert_eq!(block[0][1].to_bits(), (-0.0f32).to_bits());
        assert_eq!(block[0][2].to_bits(), 0.1f32.to_bits());
        let mut next: ChanBuf = vec![vec![0.0]];
        line.run(&mut next);
        assert_eq!(next[0][0].to_bits(), (f32::MIN_POSITIVE / 4.0).to_bits(), "a number that is not normal is not flushed by the line");
    }

    #[test]
    fn an_empty_block_changes_nothing() {
        let mut line = DelayLine::new(3, 1);
        let mut first = ramp(0, 2, 1);
        line.run(&mut first);
        line.run(&mut vec![vec![]]);
        let mut rest = ramp(2, 4, 1);
        line.run(&mut rest);
        assert_eq!([first[0].clone(), rest[0].clone()].concat(), out_of(3, &[2, 4]));
    }

    #[test]
    fn a_buffer_with_more_channels_than_the_line_leaves_the_others_as_they_are() {
        let mut line = DelayLine::new(1, 1);
        let mut block = ramp(0, 3, 2);
        line.run(&mut block);
        assert_eq!(block, [vec![0.0, 1.0, 2.0], vec![1001.0, 1002.0, 1003.0]]);
    }

    #[test]
    fn a_longer_line_has_silence_before_what_the_shorter_one_had() {
        let mut line = DelayLine::new(2, 1);
        let mut block = ramp(0, 5, 1); // the line has 4 and 5 now
        line.run(&mut block);
        line.resize(4);
        let mut next = ramp(5, 6, 1);
        line.run(&mut next);
        assert_eq!(next[0], [0.0, 0.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(line.frames(), 4);
    }

    #[test]
    fn a_shorter_line_has_the_newest_of_what_the_longer_one_had() {
        let mut line = DelayLine::new(5, 1);
        let mut block = ramp(0, 7, 1); // the line has 3, 4, 5, 6, 7 now
        line.run(&mut block);
        line.resize(2);
        let mut next = ramp(7, 4, 1);
        line.run(&mut next);
        assert_eq!(next[0], [6.0, 7.0, 8.0, 9.0]);
    }

    #[test]
    fn a_line_that_is_made_as_long_as_it_is_is_left_alone_and_one_of_no_frames_has_no_channels_to_speak_of() {
        let mut line = DelayLine::new(3, 2);
        let mut block = ramp(0, 2, 2);
        line.run(&mut block);
        line.resize(3);
        let mut next = ramp(2, 3, 2);
        line.run(&mut next);
        assert_eq!(next[0], [0.0, 1.0, 2.0]);
        assert_eq!((line.frames(), line.channels()), (3, 2));
        assert_eq!(DelayLine::new(4, 0).frames(), 0);
    }
}
