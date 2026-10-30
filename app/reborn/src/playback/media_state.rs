//! The now-playing record and the artwork published for it.
use super::*;

pub(crate) fn update_media_state(
    state: &Arc<Mutex<serde_json::Value>>,
    track: &Track,
    decoder: &Decoder,
    job: &Job,
    processing_format: PcmFormat,
    preopened: bool,
) {
    if let Ok(mut value) = state.lock() {
        let alsa = value.get("alsa").cloned();
        *value = json!({
            "state":"buffering",
            "correlation_id":job.id,
            "generation":job.generation,
            "track_id":track.id,
            "filename":track.filename,
            "source":decoder.metadata.clone(),
            "decoder_format":decoder.metadata.decoder_sample_fmt.clone(),
            "internal_processing_format":"fltp",
            "filters":decoder.metadata.filters.clone(),
            "dsp":job.dsp.clone(),
            "output":{
                "rate":job.spec.rate,
                "format":job.spec.format,
                "processing_format":processing_format,
                "fallback":job.spec.fallback,
                "fallback_reason":job.spec.fallback_reason
            },
            "replay_gain":{
                "mode":decoder.metadata.replaygain_mode,
                "track_gain_db":decoder.metadata.track_gain_db,
                "album_gain_db":decoder.metadata.album_gain_db,
                "track_peak":decoder.metadata.track_peak,
                "album_peak":decoder.metadata.album_peak,
                "applied_gain_db":decoder.metadata.applied_gain_db,
                "headroom_db":decoder.metadata.headroom_db
            },
            "eq":{"active":decoder.metadata.eq_active},
            "resampling":{
                "active":decoder.metadata.resampling,
                "reason":decoder.metadata.resample_reason,
                "source_rate":decoder.metadata.sample_rate,
                "output_rate":job.spec.rate
            },
            "final_format_conversion":decoder.metadata.format_conversion,
            "gapless":{"enabled":job.gapless_enabled,"preopened":preopened},
            "crossfade":{"enabled":job.dsp.crossfade_ms>0,"duration_ms":job.dsp.crossfade_ms}
        });
        if let Some(alsa) = alsa {
            if let Some(object) = value.as_object_mut() {
                object.insert("alsa".into(), alsa);
            }
        }
    }
}

/// FFmpeg's AVERROR_EXIT ("Immediate exit requested"): the job's
/// cancellation interrupted the open.
pub(crate) fn open_cancelled(error: &str) -> bool {
    error.starts_with("FFmpeg -1414092869:")
}

/// A cached embedded cover: `ART_SIZE` RGBA. A cover cached at another size
/// (before Beta 8.18) is decoded again and replaced.
pub(crate) const ART_BYTES: usize = ART_SIZE * ART_SIZE * 4;

pub(crate) fn publish_artwork(
    cache: &std::path::Path,
    track: &Track,
    decoder: &mut BoundDecoder,
) -> Option<Vec<u8>> {
    let _ = std::fs::create_dir_all(cache);
    if let Ok(entries) = std::fs::read_dir(cache) {
        let mut files = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "rgba"))
            .collect::<Vec<_>>();
        files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
        let remove = files.len().saturating_sub(63);
        for entry in files.into_iter().take(remove) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    // The folder's cover first, the embedded picture only when the folder has
    // none (same order as the thumbnail worker). Only embedded pictures are
    // cached here: a folder image can change without the track changing.
    let Source::File(pinned) = &decoder._source else {
        return None; // a station has no folder or embedded cover
    };
    if let Ok(folder) = pinned_folder_artwork(pinned) {
        return Some(folder);
    }
    let path = cache.join(format!("{}-{}-{}.rgba", track.id, track.size, track.mtime));
    let cached = decoder
        .metadata
        .artwork
        .then(|| std::fs::metadata(&path).ok())
        .flatten()
        .filter(|meta| meta.len() == ART_BYTES as u64)
        .and_then(|_| std::fs::read(&path).ok())
        .filter(|b| b.len() == ART_BYTES);
    let have_cached = cached.is_some();
    let embedded = cached.or_else(|| {
        decoder
            .metadata
            .artwork
            .then(|| decoder.artwork(ART_SIZE as u32))
            .and_then(Result::ok)
    });
    if let Some(bytes) = &embedded {
        if !have_cached {
            let _ = reborn_core::atomic_write(&path, bytes);
        }
    }
    embedded
}

pub(crate) fn pinned_folder_artwork(
    source: &reborn_platform::storage::PinnedTrack,
) -> Result<Vec<u8>, String> {
    for name in folder_artwork_candidates(&source.sidecar_directory())? {
        if let Ok(file) = source.open_sidecar(&name) {
            if let Ok(pixels) = external_artwork(file.path(), ART_SIZE as u32) {
                return Ok(pixels);
            }
        }
    }
    Err("no usable pinned folder artwork".into())
}
