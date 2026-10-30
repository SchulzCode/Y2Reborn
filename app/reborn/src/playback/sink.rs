//! Opening and keeping the sink: the retries of a busy device and the sink kept across a switch.
use super::*;

pub(crate) type SinkFactory =
    Box<dyn Fn(&SinkSpec, Observer, u64) -> Result<Box<dyn AudioSink>, String> + Send>;

/// How long a start waits for its sink. A wired open whose crystal misses its
/// start cycles the codec's rails in the kernel (≈1.9 s), and the sink opens a
/// second time: 2 s gave up during the second open even when it played (#52).
pub(crate) const OPEN_DEADLINE: Duration = Duration::from_secs(5);

/// What one slow open can take (the crystal case above): no retry starts so
/// late that it cannot finish inside `OPEN_DEADLINE`.
pub(crate) const SLOW_OPEN: Duration = Duration::from_secs(2);

/// A busy output is tried again for this long from the first open (F24: the
/// headphone open refused with ALSA -16 for 1–3 s after a wake).
pub(crate) const BUSY_OPEN_FOR: Duration = Duration::from_secs(2);

/// How long a wired sink stays open after its stream ended or was stopped
/// for the next one: the next track starts in it well within this.
pub(crate) const KEEP_SINK_FOR: Duration = Duration::from_secs(2);

/// The fade-in a kept sink starts the next stream with, so that the new
/// stream does not start with a click where the old one was cut.
pub(crate) const SWITCH_FADE_MS: u32 = 10;

/// A wired sink between two streams, its queued audio dropped. Reopening the
/// headphone output for every track power-cycles the codec's amplifier and
/// charge pump, on the rail of its crystal: a marginal crystal stopped after
/// enough of them (#52). The stock firmware keeps its output open too.
pub(crate) struct KeptSink {
    pub(crate) sink: Box<dyn AudioSink>,
    pub(crate) spec: SinkSpec,
    /// `reborn_audio::wired_epoch` when it opened.
    pub(crate) wired: u64,
    pub(crate) since: Instant,
}

/// Keep a wired sink for the next stream, its queued audio dropped at once
/// (nothing of the old track plays on). Any other sink, or one that cannot
/// drop its audio, closes.
pub(crate) fn keep_sink(
    sink: Option<Box<dyn AudioSink>>,
    opened: Option<(SinkSpec, u64)>,
    log: &Observer,
    id: u64,
) -> Option<KeptSink> {
    let (mut sink, (spec, wired)) = (sink?, opened?);
    if !matches!(spec.output, AudioOutput::Wired) {
        return None;
    }
    if let Err(e) = sink.discard() {
        log.emit(
            Level::Warn,
            "audio",
            "sink_keep_failed",
            &e,
            Some(id),
            json!({"recovery":"close"}),
        );
        return None;
    }
    Some(KeptSink {
        sink,
        spec,
        wired,
        since: Instant::now(),
    })
}

/// Why a kept sink cannot play the next stream; None: it can, being exactly
/// the sink that stream wants, with the wired level and DAC filter it opened
/// with (the amplifier takes a new choice only when it powers up).
pub(crate) fn sink_change(
    kept: &SinkSpec,
    kept_wired: u64,
    wanted: &SinkSpec,
    wired: u64,
) -> Option<&'static str> {
    if !matches!(kept.output, AudioOutput::Wired) || kept.output != wanted.output {
        Some("output")
    } else if kept.rate != wanted.rate {
        Some("rate")
    } else if kept.format != wanted.format {
        Some("format")
    } else if kept != wanted {
        Some("spec")
    } else if kept_wired != wired {
        Some("wired_settings")
    } else {
        None
    }
}

/// An output that is busy, not broken: ALSA -16 (EBUSY, also the activity
/// lease the sleep helper holds until the radios are back) or -11 (EAGAIN).
pub(crate) fn open_busy(error: &str) -> bool {
    error.starts_with("ALSA -16:") || error.starts_with("ALSA -11:")
}

/// The pause before busy retry `attempt` (from 1): 100 ms, growing to 250.
pub(crate) fn busy_open_delay(attempt: u32) -> Duration {
    Duration::from_millis((50 + 50 * u64::from(attempt)).min(250))
}

/// Open, and open again after a refusal. A codec whose crystal did not start
/// in time (#48) refuses one open; a second open a moment later plays. A busy
/// output is tried every 100–250 ms until `BUSY_OPEN_FOR` has passed. No retry
/// starts after `OPEN_DEADLINE` minus `SLOW_OPEN`. `refused` hears of each
/// refusal that is tried again, with the attempt number and the pause.
pub(crate) fn open_with_retries<T>(
    mut open: impl FnMut() -> Result<T, String>,
    mut refused: impl FnMut(&str, u32, Duration),
    mut pause: impl FnMut(Duration),
    elapsed: impl Fn() -> Duration,
) -> Result<T, String> {
    let mut attempt = 0u32;
    let mut other_retried = false;
    loop {
        let error = match open() {
            Ok(sink) => return Ok(sink),
            Err(error) => error,
        };
        let busy = open_busy(&error);
        let delay = if busy {
            busy_open_delay(attempt + 1)
        } else {
            Duration::from_millis(250)
        };
        let resume = elapsed() + delay;
        let again = resume + SLOW_OPEN <= OPEN_DEADLINE
            && if busy {
                resume <= BUSY_OPEN_FOR
            } else {
                !std::mem::replace(&mut other_retried, true)
            };
        if !again {
            return Err(error);
        }
        attempt += 1;
        refused(&error, attempt, delay);
        pause(delay);
    }
}
