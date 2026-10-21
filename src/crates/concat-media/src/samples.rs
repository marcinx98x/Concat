// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah and Concat contributors

//! Pulling samples out of a file.
//!
//! One audio decoder for everything that needs sound as numbers: the
//! waveform peaks, the playback cache, the transcriber. Each asks for a
//! window of the file, a filter chain to run it through, and the rate,
//! channel count and sample format it wants back; libavfilter does the
//! trimming, the filtering and the resampling in one graph, so a clip's
//! speed and effects mean exactly the same thing here as in the export mix.

use std::path::{Path, PathBuf};

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::codec::decoder;
use ffmpeg_the_third::filter;
use ffmpeg_the_third::format;
use ffmpeg_the_third::util::frame::audio::Audio;

use crate::error::{Error, Result};
use crate::ffi;

/// The sample format decoded audio comes back in. Interleaved either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    /// Signed 16-bit.
    I16,
    /// 32-bit float in [-1, 1].
    F32,
}

impl SampleFormat {
    fn name(self) -> &'static str {
        match self {
            SampleFormat::I16 => "s16",
            SampleFormat::F32 => "flt",
        }
    }

    fn bytes(self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::F32 => 4,
        }
    }
}

/// What to decode and how it should come back.
#[derive(Clone, Debug)]
pub struct AudioOptions {
    /// Seconds into the file to start. The cut is exact: the container
    /// seeks near it, the graph trims to it.
    pub start: Option<f64>,
    /// Seconds of source to decode from `start`, or the rest of the file.
    pub duration: Option<f64>,
    /// FFmpeg audio filters to run the window through, in order, before
    /// resampling. Each is validated like a clip's chain.
    pub filters: Vec<String>,
    /// Samples per second to come back at.
    pub rate: u32,
    /// One for mono, two for stereo.
    pub channels: u16,
    /// The sample format to come back in.
    pub format: SampleFormat,
    /// Which audio stream of the file, by its index in the file. `None` is
    /// the first audio stream in file order, which is also what a file with
    /// one stream has; see `probe::audio_stream_index`.
    pub stream: Option<usize>,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            start: None,
            duration: None,
            filters: Vec::new(),
            rate: 48_000,
            channels: 2,
            format: SampleFormat::I16,
            stream: None,
        }
    }
}

/// The graph between the codec and the caller, as one string. `origin` is
/// the stream's own start, in seconds: the decoded frames carry it on their
/// timestamps, so a cut `start` seconds into the sound is made at
/// `origin + start` (see `ffi::start_of`).
fn audio_filter(options: &AudioOptions, origin: f64) -> String {
    let mut parts: Vec<String> = Vec::new();
    match (options.start.map(|start| start + origin), options.duration) {
        (Some(start), Some(duration)) => {
            parts.push(format!("atrim=start={start:.6}:duration={duration:.6}"));
            parts.push("asetpts=PTS-STARTPTS".to_owned());
        }
        (Some(start), None) => {
            parts.push(format!("atrim=start={start:.6}"));
            parts.push("asetpts=PTS-STARTPTS".to_owned());
        }
        (None, Some(duration)) => {
            parts.push(format!("atrim=duration={duration:.6}"));
        }
        (None, None) => {}
    }
    parts.extend(
        options
            .filters
            .iter()
            .filter(|filter| !filter.is_empty())
            .cloned(),
    );
    parts.push(format!("aresample={}", options.rate));
    parts.push(format!(
        "aformat=sample_fmts={}:sample_rates={}:channel_layouts={}",
        options.format.name(),
        options.rate,
        if options.channels == 1 {
            "mono"
        } else {
            "stereo"
        }
    ));
    parts.join(",")
}

/// `abuffer` option string for a decoded frame: sample-rate time base, hex
/// channel layout when the description would confuse the filter parser.
fn abuffer_args(frame: &Audio) -> String {
    format!(
        "time_base=1/{}:sample_rate={}:sample_fmt={}:channel_layout={}",
        frame.rate(),
        frame.rate(),
        frame.format().name(),
        ffi::abuffer_channel_layout(&frame.ch_layout())
    )
}

/// Decodes a file's audio through libavcodec and libavfilter.
pub struct AudioDecoder {
    path: PathBuf,
    input: format::context::Input,
    stream: usize,
    /// Container stream time base: decoded frames carry PTS in these units
    /// until they are rescaled into the abuffer's sample-rate clock.
    time_base: ffmpeg::Rational,
    /// The stream's first timestamp, in seconds: where its source time 0
    /// is, the way the picture and the export mix measure it.
    origin: f64,
    decoder: decoder::Audio,
    /// A packet the codec refused while full; see [`crate::feed`].
    held: crate::feed::Held<ffmpeg::Packet>,
    options: AudioOptions,
    graph: Option<filter::Graph>,
    /// The stream's `abuffer` args for the last graph, kept so a failed
    /// `buffersrc.add` can name what it disagreed with.
    abuffer_args: String,
    /// The codec has been drained and the graph flushed; only what the sink
    /// still holds is left.
    flushed: bool,
    done: bool,
}

impl AudioDecoder {
    /// Opens the audio stream `options.stream` names, or `path`'s first.
    pub fn open(path: impl AsRef<Path>, options: &AudioOptions) -> Result<Self> {
        ffi::init();
        for chain in &options.filters {
            crate::audio::validate_chain(chain)?;
        }
        if options.channels != 1 && options.channels != 2 {
            return Err(Error::Probe {
                path: path.as_ref().to_path_buf(),
                detail: format!(
                    "{} channels requested; mono and stereo are the choices",
                    options.channels
                ),
            });
        }

        let path = path.as_ref();
        let mut input =
            ffmpeg::format::input(path).map_err(|error| ffi::fail("open", path, error))?;
        let stream_index =
            crate::probe::audio_stream_index(&input, options.stream).ok_or_else(|| {
                Error::NoAudioStream {
                    path: path.to_path_buf(),
                }
            })?;
        let stream = input.stream(stream_index).expect("just found");
        let time_base = stream.time_base();
        // A file whose sound starts late - MPEG-TS at 1.4 s, the MTS files
        // cameras write - is measured from its first sample, as the picture
        // and the export mix are; without this the preview's sound and a
        // transcript ran ahead of the picture (audit 2026-10-04, #3).
        let origin = ffi::start_of(&stream).as_f64();
        let context = ffmpeg::codec::Context::from_parameters(stream.parameters())
            .map_err(|error| ffi::fail("codec parameters", path, error))?;
        // (E-)AC-3 downmixes only when the decoder option `downmix` is set
        // (AC3DecodeContext.downmix_layout). Overwriting ch_layout before
        // open2 does not; surround then reaches abuffer as 5.1(side) /
        // 0x60f and buffersrc.add returns EINVAL. Playback and peaks want
        // stereo; the transcriber wants mono. Unknown on other codecs —
        // open2 leaves unused options in the dict and still succeeds.
        let decoder = context.decoder();
        let codec = ffmpeg::decoder::find(decoder.id()).ok_or_else(|| Error::Ffi {
            operation: "open decoder",
            path: path.to_path_buf(),
            detail: "decoder not found".to_owned(),
        })?;
        let mut opts = ffmpeg::Dictionary::new();
        opts.set(
            "downmix",
            if options.channels == 1 {
                "mono"
            } else {
                "stereo"
            },
        );
        let decoder = decoder
            .open_as_with(codec, opts)
            .and_then(|opened| opened.audio())
            .map_err(|error| ffi::fail("open decoder", path, error))?;

        // Near the start, not at it: the graph's atrim does the exact cut,
        // and a seek only saves decoding what comes before.
        if let Some(start) = options.start
            && start > 0.0
        {
            let target = ((start + origin) * f64::from(ffmpeg::sys::AV_TIME_BASE)) as i64;
            input
                .seek(target, ..=target)
                .map_err(|error| ffi::fail("seek", path, error))?;
        }

        Ok(Self {
            path: path.to_path_buf(),
            input,
            stream: stream_index,
            time_base,
            origin,
            decoder,
            held: None,
            options: options.clone(),
            graph: None,
            abuffer_args: String::new(),
            flushed: false,
            done: false,
        })
    }

    /// Samples per second of what comes back.
    pub fn rate(&self) -> u32 {
        self.options.rate
    }

    /// Channels of what comes back.
    pub fn channels(&self) -> u16 {
        self.options.channels
    }

    fn build_graph(&self, first: &Audio) -> Result<(filter::Graph, String)> {
        let mut graph = filter::Graph::new();
        // Sample-rate time base, matching the export mix and FFmpeg's own
        // auto-graph (`tb:1/48000`). Stream TB can be 1/1000 on MKV; that
        // disagrees with frame PTS counted in samples and makes abuffer
        // reject the frame.
        let args = abuffer_args(first);
        let missing = |name: &str| Error::Missing {
            what: "filter",
            name: name.to_owned(),
        };
        graph
            .add(
                &filter::find("abuffer").ok_or_else(|| missing("abuffer"))?,
                "in",
                &args,
            )
            .map_err(|error| ffi::fail("buffer source", &self.path, error))?;
        graph
            .add(
                &filter::find("abuffersink").ok_or_else(|| missing("abuffersink"))?,
                "out",
                "",
            )
            .map_err(|error| ffi::fail("buffer sink", &self.path, error))?;
        graph
            .output("in", 0)
            .and_then(|parser| parser.input("out", 0))
            .and_then(|parser| parser.parse(&audio_filter(&self.options, self.origin)))
            .map_err(|error| ffi::fail("filter graph", &self.path, error))?;
        graph
            .validate()
            .map_err(|error| ffi::fail("filter graph", &self.path, error))?;
        Ok((graph, args))
    }

    /// One decoded frame out of the codec, or `None` when the file is over.
    fn next_source(&mut self) -> Result<Option<Audio>> {
        let read = || crate::feed::read_packet(&mut self.input, self.stream);
        crate::feed::next_frame(&mut self.decoder, &mut self.held, read)
            .map_err(|stall| crate::feed::fail(stall, &self.path))
    }

    /// The next frame out of the graph: interleaved samples at the requested
    /// rate and layout, or `None` once the window is exhausted.
    pub fn next_frame(&mut self) -> Result<Option<Audio>> {
        loop {
            if self.done {
                return Ok(None);
            }
            if let Some(graph) = self.graph.as_mut() {
                let mut out = Audio::empty();
                let pulled = {
                    let mut context = graph.get("out").expect("the graph has an output");
                    context.sink().frame(&mut out)
                };
                match pulled {
                    Ok(()) => return Ok(Some(out)),
                    Err(ffmpeg::Error::Eof) => {
                        self.done = true;
                        return Ok(None);
                    }
                    Err(error) if ffi::is_again(&error) => {}
                    Err(error) => return Err(ffi::fail("filter output", &self.path, error)),
                }
                if self.flushed {
                    self.done = true;
                    return Ok(None);
                }
            }

            // The graph wants more. Feed it the next decoded frame, or tell
            // it the file is over.
            match self.next_source()? {
                Some(mut frame) => {
                    if self.graph.is_none() {
                        let (graph, args) = self.build_graph(&frame)?;
                        self.graph = Some(graph);
                        self.abuffer_args = args;
                    }
                    // abuffer runs on a 1/rate clock; frames still carry
                    // stream PTS. Rescale so atrim sees real seconds (MKV
                    // often has TB 1/1000; MPEG-TS 1/90000).
                    let sample_tb = ffmpeg::Rational::new(1, frame.rate() as i32);
                    if let Some(pts) = frame.timestamp() {
                        use ffmpeg::rescale::Rescale;
                        frame.set_pts(Some(pts.rescale(self.time_base, sample_tb)));
                    }
                    let graph = self.graph.as_mut().expect("just built");
                    let mut context = graph.get("in").expect("the graph has an input");
                    context.source().add(&frame).map_err(|error| Error::Ffi {
                        operation: "filter",
                        path: self.path.clone(),
                        detail: format!("{error} (abuffer={})", self.abuffer_args),
                    })?;
                }
                None => {
                    let Some(graph) = self.graph.as_mut() else {
                        // No audio frame at all - a stream that claims sound
                        // and delivers none.
                        self.done = true;
                        return Ok(None);
                    };
                    let mut context = graph.get("in").expect("the graph has an input");
                    let _ = context.source().flush();
                    self.flushed = true;
                }
            }
        }
    }

    /// The interleaved bytes of one frame from the sink: samples times
    /// channels times the sample width, without the row padding libav adds.
    pub fn bytes_of(&self, frame: &Audio) -> Vec<u8> {
        let wanted =
            frame.samples() * usize::from(self.options.channels) * self.options.format.bytes();
        let data = frame.data(0);
        data[..wanted.min(data.len())].to_vec()
    }

    /// The next frame as interleaved 16-bit samples. Only for decoders
    /// opened with [`SampleFormat::I16`].
    pub fn next_i16(&mut self) -> Result<Option<Vec<i16>>> {
        let Some(frame) = self.next_frame()? else {
            return Ok(None);
        };
        let bytes = self.bytes_of(&frame);
        Ok(Some(
            bytes
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        ))
    }

    /// The next frame as interleaved floats. Only for decoders opened with
    /// [`SampleFormat::F32`].
    pub fn next_f32(&mut self) -> Result<Option<Vec<f32>>> {
        let Some(frame) = self.next_frame()? else {
            return Ok(None);
        };
        let bytes = self.bytes_of(&frame);
        Ok(Some(
            bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect(),
        ))
    }

    /// Everything left in the window, as interleaved floats.
    pub fn collect_f32(&mut self) -> Result<Vec<f32>> {
        let mut all = Vec::new();
        while let Some(chunk) = self.next_f32()? {
            all.extend_from_slice(&chunk);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_trims_filters_and_lands_on_the_requested_shape() {
        let options = AudioOptions {
            start: Some(1.5),
            duration: Some(2.0),
            filters: vec!["atempo=2.000000".to_owned(), String::new()],
            rate: 16_000,
            channels: 1,
            format: SampleFormat::F32,
            stream: None,
        };
        assert_eq!(
            audio_filter(&options, 0.0),
            "atrim=start=1.500000:duration=2.000000,asetpts=PTS-STARTPTS,atempo=2.000000,\
             aresample=16000,aformat=sample_fmts=flt:sample_rates=16000:channel_layouts=mono"
        );
    }

    #[test]
    fn a_whole_file_has_no_trim() {
        let graph = audio_filter(&AudioOptions::default(), 1.4);
        assert!(graph.starts_with("aresample=48000,"), "{graph}");
        assert!(graph.ends_with("channel_layouts=stereo"), "{graph}");
    }

    /// The trim is made in the stream's own time, its start put back on.
    #[test]
    fn a_late_stream_is_trimmed_from_its_first_sample() {
        let options = AudioOptions {
            start: Some(0.5),
            ..AudioOptions::default()
        };
        assert!(audio_filter(&options, 1.4).starts_with("atrim=start=1.900000,"));
    }

    /// A window into a file whose sound starts at 1.4 s - silence, then a
    /// tone a second in - comes back measured from the sound's first
    /// sample: half a second in, the tone begins half a second later, as
    /// the picture and the export mix would have it.
    #[test]
    fn a_window_into_a_late_stream_lands_on_source_time() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/tone-at-1s.ts");
        let options = AudioOptions {
            start: Some(0.5),
            duration: Some(1.0),
            rate: 8_000,
            channels: 1,
            format: SampleFormat::F32,
            ..AudioOptions::default()
        };
        let mut decoder = AudioDecoder::open(path, &options).expect("opens");
        let mut samples: Vec<f32> = Vec::new();
        while let Some(frame) = decoder.next_frame().expect("decodes") {
            let bytes = &frame.data(0)[..frame.samples() * 4];
            samples.extend(
                bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        let loud = |range: std::ops::Range<usize>| {
            samples[range]
                .iter()
                .map(|s| s.abs())
                .fold(0.0_f32, f32::max)
        };
        assert!(
            samples.len() >= 7_600,
            "a second of sound, got {}",
            samples.len()
        );
        assert!(loud(0..3_800) < 0.01, "silence before the tone");
        assert!(loud(4_200..7_600) > 0.08, "the tone after half a second");
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(AudioDecoder::open("does-not-exist.mp3", &AudioOptions::default()).is_err());
    }

    /// Opening for stereo passes `downmix=stereo` into `avcodec_open2` and
    /// still decodes an ordinary stereo/mono fixture without filter errors.
    #[test]
    fn open_for_stereo_decodes_with_downmix_option() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/tone-at-1s.ts");
        let options = AudioOptions {
            channels: 2,
            duration: Some(0.1),
            format: SampleFormat::F32,
            ..AudioOptions::default()
        };
        let mut decoder = AudioDecoder::open(path, &options).expect("opens");
        assert_eq!(decoder.channels(), 2);
        let samples = decoder.collect_f32().expect("decodes");
        assert!(!samples.is_empty(), "expected some stereo samples");
    }

    #[test]
    fn abuffer_args_use_sample_rate_time_base() {
        ffi::init();
        let mut frame = Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar),
            1_024,
            ffmpeg::util::channel_layout::ChannelLayoutMask::STEREO,
        );
        frame.set_ch_layout(ffmpeg::util::channel_layout::ChannelLayout::STEREO);
        frame.set_rate(48_000);
        let args = abuffer_args(&frame);
        assert!(
            args.starts_with("time_base=1/48000:sample_rate=48000:"),
            "{args}"
        );
        assert!(args.contains("channel_layout=0x3"), "{args}");
    }

    /// AC-3 / DDP 5.1(side) MKV: set `CONCAT_TEST_AC3` to a path, or use
    /// the Synology fixture when that share is mounted. Must actually decode
    /// (not only open) so a false-green from overwriting ch_layout cannot
    /// hide a failed `buffersrc.add`.
    #[test]
    fn ac3_51_side_decodes_as_stereo() {
        let path = std::env::var("CONCAT_TEST_AC3").unwrap_or_else(|_| {
            r"\\Synology\Pobrane\xxxSlow.Horses.S03E01.PLSUB.1080p.ATVP.WEB-DL.DDP5.1.H.264-GloriousMongoose\xSlow Horses - 3x03 - Negotiating with Tigers.mkv"
                .to_owned()
        });
        if !Path::new(&path).is_file() {
            eprintln!("skipping ac3_51_side_decodes_as_stereo: {path} not found");
            return;
        }
        let options = AudioOptions {
            duration: Some(0.5),
            rate: 48_000,
            channels: 2,
            format: SampleFormat::F32,
            ..AudioOptions::default()
        };
        let mut decoder = AudioDecoder::open(&path, &options).expect("opens AC-3");
        let samples = decoder.collect_f32().expect("decodes through abuffer");
        assert!(
            !decoder.abuffer_args.contains("0x60f"),
            "abuffer should see stereo after downmix, got {}",
            decoder.abuffer_args
        );
        assert!(
            decoder.abuffer_args.contains("channel_layout=0x3"),
            "expected stereo abuffer layout, got {}",
            decoder.abuffer_args
        );
        // 0.5 s × 48 kHz × 2 channels, allow a little short from atrim.
        // Opening seconds of a show may be near-silent; length + stereo
        // abuffer is the regression check for buffersrc EINVAL.
        assert!(
            samples.len() >= 40_000,
            "expected ~0.5 s stereo, got {} samples",
            samples.len()
        );
    }
}
