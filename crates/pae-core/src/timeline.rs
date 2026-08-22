//! VAD の発話区間から編集タイムラインを生成する純粋ロジック
//! I/O を持たないため、ユニットテストで境界値を重点的に検証できる

use std::path::Path;

use crate::error::{PaeError, Result};
use crate::types::{
    EditTimeline, Preset, SegmentAction, SegmentKind, SpeechSegment, TimelineSegment,
    TimelineStats, VadParams, TIMELINE_VERSION,
};

/// VAD の生の発話区間からタイムラインを生成する
pub fn generate_timeline(
    speech: &[SpeechSegment],
    source_path: &Path,
    source_duration_ms: u64,
    vad_params: &VadParams,
    preset: &Preset,
) -> Result<EditTimeline> {
    let padded = apply_padding(speech, vad_params, source_duration_ms);
    let merged = merge_segments(padded);
    let segments = build_segments(&merged, source_duration_ms, preset);
    let stats = compute_stats(&segments, source_duration_ms);

    let timeline = EditTimeline {
        version: TIMELINE_VERSION,
        source_path: source_path.to_path_buf(),
        source_duration_ms,
        preset_name: preset.name.clone(),
        vad_params: *vad_params,
        segments,
        stats,
    };
    validate_timeline(&timeline)?;
    Ok(timeline)
}

/// 発話区間の前後に余白を付ける。語頭・語尾・息継ぎの欠けを防ぐため
fn apply_padding(
    speech: &[SpeechSegment],
    params: &VadParams,
    duration_ms: u64,
) -> Vec<SpeechSegment> {
    speech
        .iter()
        .map(|s| SpeechSegment {
            start_ms: s.start_ms.saturating_sub(params.pad_before_ms),
            end_ms: (s.end_ms + params.pad_after_ms).min(duration_ms),
        })
        .collect()
}

/// 重なり・隣接した発話区間をひとつにまとめる
/// パディングによって隣の区間と接触した場合もここで吸収される
fn merge_segments(mut segments: Vec<SpeechSegment>) -> Vec<SpeechSegment> {
    segments.sort_by_key(|s| s.start_ms);
    let mut merged: Vec<SpeechSegment> = Vec::with_capacity(segments.len());
    for seg in segments {
        match merged.last_mut() {
            Some(last) if seg.start_ms <= last.end_ms => {
                last.end_ms = last.end_ms.max(seg.end_ms);
            }
            _ => merged.push(seg),
        }
    }
    merged
}

/// 発話区間の隙間を無音区間として補完し、0〜末尾を隙間なくカバーする
/// セグメント列を作る。無音には preset に基づいた短縮判断を付ける
fn build_segments(
    speech: &[SpeechSegment],
    duration_ms: u64,
    preset: &Preset,
) -> Vec<TimelineSegment> {
    let mut silences: Vec<(u64, u64)> = Vec::new();
    let mut cursor = 0u64;
    for s in speech {
        if s.start_ms > cursor {
            silences.push((cursor, s.start_ms));
        }
        cursor = s.end_ms;
    }
    if cursor < duration_ms {
        silences.push((cursor, duration_ms));
    }

    let mut segments: Vec<TimelineSegment> = Vec::new();
    let last_silence_start = silences.last().map(|s| s.0);

    let mut silence_iter = silences.into_iter().peekable();
    let mut speech_iter = speech.iter().peekable();

    // 開始時刻順に無音と発話を交互に並べる
    while silence_iter.peek().is_some() || speech_iter.peek().is_some() {
        let take_silence = match (silence_iter.peek(), speech_iter.peek()) {
            (Some(sil), Some(sp)) => sil.0 < sp.start_ms,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => unreachable!(),
        };
        if take_silence {
            let (start, end) = silence_iter.next().unwrap();
            let is_edge = start == 0 || Some(start) == last_silence_start && end == duration_ms;
            segments.push(decide_silence(start, end, preset, is_edge));
        } else {
            let sp = speech_iter.next().unwrap();
            segments.push(TimelineSegment {
                source_start_ms: sp.start_ms,
                source_end_ms: sp.end_ms,
                kind: SegmentKind::Speech,
                action: SegmentAction::Keep,
                keep_duration_ms: sp.duration_ms(),
            });
        }
    }
    segments
}

/// 無音ひとつ分の短縮判断
/// 「削除」はせず、しきい値以上の無音を自然な長さ (target_silence_ms) まで縮める
/// 冒頭・末尾の無音は会話の間ではないため、trim_edges 時はしきい値未満でも縮める
fn decide_silence(start: u64, end: u64, preset: &Preset, is_edge: bool) -> TimelineSegment {
    let len = end - start;
    let should_compress = len >= preset.compress_threshold_ms || (is_edge && preset.trim_edges);
    let keep = if should_compress {
        preset.target_silence_ms.min(len)
    } else {
        len
    };
    TimelineSegment {
        source_start_ms: start,
        source_end_ms: end,
        kind: SegmentKind::Silence,
        action: if keep < len {
            SegmentAction::Compress
        } else {
            SegmentAction::Keep
        },
        keep_duration_ms: keep,
    }
}

fn compute_stats(segments: &[TimelineSegment], source_duration_ms: u64) -> TimelineStats {
    TimelineStats {
        source_duration_ms,
        output_duration_ms: segments.iter().map(|s| s.keep_duration_ms).sum(),
        silence_count: segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Silence)
            .count(),
        compressed_count: segments
            .iter()
            .filter(|s| s.action == SegmentAction::Compress)
            .count(),
    }
}

/// タイムラインの不変条件を検証する
/// 生成バグや timeline.json の手修正ミスをレンダリング前に検出する
pub fn validate_timeline(timeline: &EditTimeline) -> Result<()> {
    let mut cursor = 0u64;
    for (i, seg) in timeline.segments.iter().enumerate() {
        if seg.source_start_ms != cursor {
            return Err(PaeError::InvalidTimeline(format!(
                "segment {i} が連続していません: {}ms から始まるべきところ {}ms",
                cursor, seg.source_start_ms
            )));
        }
        if seg.source_end_ms <= seg.source_start_ms {
            return Err(PaeError::InvalidTimeline(format!(
                "segment {i} の長さが 0 以下です"
            )));
        }
        if seg.keep_duration_ms > seg.source_duration_ms() {
            return Err(PaeError::InvalidTimeline(format!(
                "segment {i} の keep_duration がソース長を超えています"
            )));
        }
        cursor = seg.source_end_ms;
    }
    if cursor != timeline.source_duration_ms {
        return Err(PaeError::InvalidTimeline(format!(
            "セグメントの末尾 {}ms がソース長 {}ms と一致しません",
            cursor, timeline.source_duration_ms
        )));
    }
    let sum: u64 = timeline.segments.iter().map(|s| s.keep_duration_ms).sum();
    if sum != timeline.stats.output_duration_ms {
        return Err(PaeError::InvalidTimeline(
            "stats.output_duration_ms が keep_duration の合計と一致しません".into(),
        ));
    }
    Ok(())
}

/// タイムラインから「出力に残すソース区間」のリストを導出する
/// Compress する無音は区間の先頭側を残す。直前の発話とつながって
/// 「発話後の間」として自然に聞こえ、さらに隣接区間とマージできるため
/// ffmpeg フィルタ式の節数も減らせる
pub fn timeline_to_keep_ranges(timeline: &EditTimeline) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for seg in &timeline.segments {
        if seg.keep_duration_ms == 0 {
            continue;
        }
        let start = seg.source_start_ms;
        let end = seg.source_start_ms + seg.keep_duration_ms;
        match ranges.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => ranges.push((start, end)),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn seg(start_ms: u64, end_ms: u64) -> SpeechSegment {
        SpeechSegment { start_ms, end_ms }
    }

    /// パディングなしのパラメータ。無音長そのものの境界値検証に使う
    fn no_pad() -> VadParams {
        VadParams {
            pad_before_ms: 0,
            pad_after_ms: 0,
            ..VadParams::default()
        }
    }

    fn gen(
        speech: &[SpeechSegment],
        duration_ms: u64,
        params: &VadParams,
        preset: &Preset,
    ) -> EditTimeline {
        generate_timeline(
            speech,
            &PathBuf::from("test.mp4"),
            duration_ms,
            params,
            preset,
        )
        .expect("timeline generation should succeed")
    }

    /// 発話に挟まれた無音の action を返すヘルパ
    /// 動画: [speech 0..1000][silence 1000..1000+gap][speech ..+1000]
    fn middle_silence(gap_ms: u64, preset: &Preset) -> TimelineSegment {
        let speech = [seg(0, 1000), seg(1000 + gap_ms, 2000 + gap_ms)];
        let t = gen(&speech, 2000 + gap_ms, &no_pad(), preset);
        assert_eq!(t.segments.len(), 3);
        t.segments[1]
    }

    #[test]
    fn aggressive_boundary_690_700_710() {
        let p = Preset::aggressive(); // threshold 700ms → 300ms
        assert_eq!(middle_silence(690, &p).action, SegmentAction::Keep);
        let s700 = middle_silence(700, &p);
        assert_eq!(s700.action, SegmentAction::Compress);
        assert_eq!(s700.keep_duration_ms, 300);
        assert_eq!(middle_silence(710, &p).action, SegmentAction::Compress);
    }

    #[test]
    fn standard_boundary_1490_1500_1510() {
        let p = Preset::standard(); // threshold 1500ms → 800ms
        assert_eq!(middle_silence(1490, &p).action, SegmentAction::Keep);
        let s1500 = middle_silence(1500, &p);
        assert_eq!(s1500.action, SegmentAction::Compress);
        assert_eq!(s1500.keep_duration_ms, 800);
        assert_eq!(middle_silence(1510, &p).action, SegmentAction::Compress);
    }

    #[test]
    fn natural_boundary_2990_3000_3010() {
        let p = Preset::natural(); // threshold 3000ms → 1200ms
        assert_eq!(middle_silence(2990, &p).action, SegmentAction::Keep);
        let s3000 = middle_silence(3000, &p);
        assert_eq!(s3000.action, SegmentAction::Compress);
        assert_eq!(s3000.keep_duration_ms, 1200);
        assert_eq!(middle_silence(3010, &p).action, SegmentAction::Compress);
    }

    /// パディング (前150 + 後250 = 400ms) と無音長の関係
    /// 無音 399/400ms → パディングで埋まり発話がマージされる。401ms → 1ms の無音が残る
    #[test]
    fn padding_collision_merges_segments() {
        let params = VadParams::default();
        let p = Preset::natural();
        for gap in [399u64, 400] {
            let speech = [seg(1000, 2000), seg(2000 + gap, 3000 + gap)];
            let t = gen(&speech, 4000 + gap, &params, &p);
            let speech_count = t
                .segments
                .iter()
                .filter(|s| s.kind == SegmentKind::Speech)
                .count();
            assert_eq!(speech_count, 1, "gap={gap}ms では発話がマージされるべき");
        }
        let speech = [seg(1000, 2000), seg(2401, 3401)];
        let t = gen(&speech, 4401, &params, &p);
        let mid = &t.segments[2];
        assert_eq!(mid.kind, SegmentKind::Silence);
        assert_eq!(mid.source_duration_ms(), 1);
        assert_eq!(mid.action, SegmentAction::Keep);
    }

    /// 冒頭・末尾の無音は trim_edges によりしきい値未満でも短縮される
    #[test]
    fn edges_are_trimmed_below_threshold() {
        let p = Preset::natural(); // threshold 3000ms, target 1200ms
        let speech = [seg(2000, 4000)];
        let t = gen(&speech, 6000, &no_pad(), &p);
        assert_eq!(t.segments.len(), 3);
        assert_eq!(t.segments[0].action, SegmentAction::Compress);
        assert_eq!(t.segments[0].keep_duration_ms, 1200);
        assert_eq!(t.segments[2].action, SegmentAction::Compress);
        assert_eq!(t.segments[2].keep_duration_ms, 1200);
    }

    /// 無音長が target より短ければ keep_duration はクランプされ Keep になる
    #[test]
    fn short_edge_silence_clamps_to_own_length() {
        let p = Preset::natural(); // target 1200ms
        let speech = [seg(500, 2000)];
        let t = gen(&speech, 2000, &no_pad(), &p);
        assert_eq!(t.segments[0].keep_duration_ms, 500);
        assert_eq!(t.segments[0].action, SegmentAction::Keep);
    }

    #[test]
    fn speech_at_boundaries_no_zero_length_silence() {
        let p = Preset::natural();
        let speech = [seg(0, 1000), seg(2000, 3000)];
        let t = gen(&speech, 3000, &no_pad(), &p);
        assert_eq!(t.segments.len(), 3);
        assert_eq!(t.segments[0].kind, SegmentKind::Speech);
        assert_eq!(t.segments[2].kind, SegmentKind::Speech);
    }

    #[test]
    fn all_silence_input() {
        let p = Preset::natural();
        let t = gen(&[], 10_000, &no_pad(), &p);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].action, SegmentAction::Compress);
        assert_eq!(t.stats.output_duration_ms, 1200);
    }

    #[test]
    fn all_speech_input() {
        let p = Preset::natural();
        let t = gen(&[seg(0, 5000)], 5000, &no_pad(), &p);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.stats.output_duration_ms, 5000);
    }

    /// パディングが動画の端を超えないようクランプされる
    #[test]
    fn padding_clamped_to_media_bounds() {
        let params = VadParams::default();
        let p = Preset::natural();
        let t = gen(&[seg(100, 4900)], 5000, &params, &p);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].source_start_ms, 0);
        assert_eq!(t.segments[0].source_end_ms, 5000);
    }

    /// keep ranges: Compress 無音は先頭側を残すため直前の keep とマージされる
    #[test]
    fn keep_ranges_merge_adjacent() {
        let p = Preset::natural();
        let speech = [seg(0, 1000), seg(5000, 6000)];
        let t = gen(&speech, 6000, &no_pad(), &p);
        // [speech 0..1000][silence 1000..5000 → 先頭1200ms残し][speech 5000..6000]
        let ranges = timeline_to_keep_ranges(&t);
        assert_eq!(ranges, vec![(0, 2200), (5000, 6000)]);
    }

    #[test]
    fn output_duration_equals_sum_of_keeps() {
        let p = Preset::standard();
        let speech = [seg(500, 1500), seg(4000, 5000), seg(5300, 7000)];
        let t = gen(&speech, 9000, &VadParams::default(), &p);
        let sum: u64 = t.segments.iter().map(|s| s.keep_duration_ms).sum();
        assert_eq!(t.stats.output_duration_ms, sum);
        validate_timeline(&t).unwrap();
    }

    #[test]
    fn validate_rejects_gap() {
        let p = Preset::natural();
        let mut t = gen(&[seg(0, 1000), seg(2000, 3000)], 3000, &no_pad(), &p);
        t.segments.remove(1);
        assert!(validate_timeline(&t).is_err());
    }
}
