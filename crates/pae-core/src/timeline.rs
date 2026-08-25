//! VAD の発話区間から編集タイムラインを生成する純粋ロジック。
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
    let segments = build_segments(&merged, source_duration_ms, vad_params, preset);
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

/// 重なり・隣接した発話区間をひとつにまとめる。
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
    params: &VadParams,
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
            // この無音は、前後の発話へ付けたパディングにすでに削られている。
            // 手前に発話があれば pad_after の分、後ろに発話があれば pad_before の分だけ短い
            let head_pad = if start > 0 { params.pad_after_ms } else { 0 };
            let tail_pad = if end < duration_ms {
                params.pad_before_ms
            } else {
                0
            };
            segments.push(decide_silence(
                start,
                end,
                head_pad + tail_pad,
                preset,
                is_edge,
            ));
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

/// 無音ひとつ分の短縮判断。
/// 「削除」はせず、しきい値以上の無音を自然な長さ (target_silence_ms) まで縮める。
/// 冒頭・末尾の無音は会話の間ではないため、trim_edges 時はしきい値未満でも縮める。
///
/// preset のしきい値と目標値は、耳に聞こえる「間」の長さとして扱う。
/// セグメントとしての無音は前後の発話パディングにその分を譲り渡したあとの長さなので、
/// pads を足し戻してから判定し、残す長さは逆に pads を引いてから決める
fn decide_silence(
    start: u64,
    end: u64,
    pads: u64,
    preset: &Preset,
    is_edge: bool,
) -> TimelineSegment {
    let len = end - start;
    let pause_ms = len + pads;
    let should_compress =
        pause_ms >= preset.compress_threshold_ms || (is_edge && preset.trim_edges);
    let keep = if should_compress {
        preset.target_silence_ms.saturating_sub(pads).min(len)
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

/// タイムラインの不変条件を検証する。
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

/// 出力範囲 (トリム) をタイムラインに適用する。
/// 収録の前後の無駄話をカットする用途で、範囲外のセグメントは Remove になり、
/// 範囲の境界をまたぐセグメントは境界で分割される。全時間カバーの不変条件は保たれる
pub fn apply_trim_range(
    timeline: &mut EditTimeline,
    keep_start_ms: u64,
    keep_end_ms: u64,
) -> Result<()> {
    if keep_start_ms >= keep_end_ms || keep_end_ms > timeline.source_duration_ms {
        return Err(PaeError::InvalidTimeline(format!(
            "トリム範囲が不正です: {keep_start_ms}ms〜{keep_end_ms}ms"
        )));
    }

    let mut segments: Vec<TimelineSegment> = Vec::with_capacity(timeline.segments.len() + 2);
    for seg in &timeline.segments {
        // トリム境界がセグメントの内側にあれば、そこで分割する
        let mut points = vec![seg.source_start_ms];
        for p in [keep_start_ms, keep_end_ms] {
            if p > seg.source_start_ms && p < seg.source_end_ms {
                points.push(p);
            }
        }
        points.push(seg.source_end_ms);

        for pair in points.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let len = end - start;
            let inside = start >= keep_start_ms && end <= keep_end_ms;
            let (action, keep) = if !inside {
                (SegmentAction::Remove, 0)
            } else {
                match seg.action {
                    SegmentAction::Keep => (SegmentAction::Keep, len),
                    SegmentAction::Compress => {
                        let keep = seg.keep_duration_ms.min(len);
                        let action = if keep < len {
                            SegmentAction::Compress
                        } else {
                            SegmentAction::Keep
                        };
                        (action, keep)
                    }
                    SegmentAction::Remove => (SegmentAction::Remove, 0),
                }
            };
            segments.push(TimelineSegment {
                source_start_ms: start,
                source_end_ms: end,
                kind: seg.kind,
                action,
                keep_duration_ms: keep,
            });
        }
    }

    timeline.segments = segments;
    timeline.stats = compute_stats(&timeline.segments, timeline.source_duration_ms);
    validate_timeline(timeline)
}

/// タイムラインから「出力に残すソース区間」のリストを導出する。
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

    /// 発話に挟まれた無音の action を返すヘルパ。
    /// 動画: [speech 0..1000][silence 1000..1000+gap][speech ..+1000]
    fn middle_silence(gap_ms: u64, preset: &Preset) -> TimelineSegment {
        let speech = [seg(0, 1000), seg(1000 + gap_ms, 2000 + gap_ms)];
        let t = gen(&speech, 2000 + gap_ms, &no_pad(), preset);
        assert_eq!(t.segments.len(), 3);
        t.segments[1]
    }

    #[test]
    fn aggressive_boundary_690_700_710() {
        let p = Preset::aggressive(); // threshold 700ms → 400ms
        assert_eq!(middle_silence(690, &p).action, SegmentAction::Keep);
        let s700 = middle_silence(700, &p);
        assert_eq!(s700.action, SegmentAction::Compress);
        assert_eq!(s700.keep_duration_ms, 400);
        assert_eq!(middle_silence(710, &p).action, SegmentAction::Compress);
    }

    #[test]
    fn standard_boundary_990_1000_1010() {
        let p = Preset::standard(); // threshold 1000ms → 600ms
        assert_eq!(middle_silence(990, &p).action, SegmentAction::Keep);
        let s1000 = middle_silence(1000, &p);
        assert_eq!(s1000.action, SegmentAction::Compress);
        assert_eq!(s1000.keep_duration_ms, 600);
        assert_eq!(middle_silence(1010, &p).action, SegmentAction::Compress);
    }

    #[test]
    fn natural_boundary_1490_1500_1510() {
        let p = Preset::natural(); // threshold 1500ms → 900ms
        assert_eq!(middle_silence(1490, &p).action, SegmentAction::Keep);
        let s1500 = middle_silence(1500, &p);
        assert_eq!(s1500.action, SegmentAction::Compress);
        assert_eq!(s1500.keep_duration_ms, 900);
        assert_eq!(middle_silence(1510, &p).action, SegmentAction::Compress);
    }

    /// しきい値は、パディングを付ける前の「耳に聞こえる間」で判定する。
    /// パディング 400ms のとき、間 1500ms の無音セグメントは 1100ms しか残っていないが、
    /// 間としては natural のしきい値 1500ms に達しているので短縮される
    #[test]
    fn threshold_uses_pause_before_padding() {
        let p = Preset::natural();
        let params = VadParams::default();

        let t = gen(&[seg(1000, 2000), seg(3400, 4400)], 5400, &params, &p);
        let mid = t.segments[2];
        assert_eq!(mid.kind, SegmentKind::Silence);
        assert_eq!(mid.source_duration_ms(), 1000);
        assert_eq!(mid.action, SegmentAction::Keep, "間 1400ms は短縮しない");

        let t = gen(&[seg(1000, 2000), seg(3500, 4500)], 5500, &params, &p);
        let mid = t.segments[2];
        assert_eq!(mid.source_duration_ms(), 1100);
        assert_eq!(mid.action, SegmentAction::Compress);
        // 残す間 900ms のうち 400ms はパディングが担うため、セグメントには 500ms 残す
        assert_eq!(mid.keep_duration_ms, 500);
    }

    /// パディング (前150 + 後250 = 400ms) と無音長の関係。
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
        let p = Preset::natural(); // threshold 1500ms, target 900ms
        let speech = [seg(2000, 4000)];
        let t = gen(&speech, 6000, &no_pad(), &p);
        assert_eq!(t.segments.len(), 3);
        assert_eq!(t.segments[0].action, SegmentAction::Compress);
        assert_eq!(t.segments[0].keep_duration_ms, 900);
        assert_eq!(t.segments[2].action, SegmentAction::Compress);
        assert_eq!(t.segments[2].keep_duration_ms, 900);
    }

    /// 無音長が target より短ければ keep_duration はクランプされ Keep になる
    #[test]
    fn short_edge_silence_clamps_to_own_length() {
        let p = Preset::natural(); // target 900ms
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
        assert_eq!(t.stats.output_duration_ms, 900);
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
        // [speech 0..1000][silence 1000..5000 → 先頭900ms残し][speech 5000..6000]
        let ranges = timeline_to_keep_ranges(&t);
        assert_eq!(ranges, vec![(0, 1900), (5000, 6000)]);
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

    /// トリム範囲の適用: 範囲外は Remove、境界をまたぐセグメントは分割される
    #[test]
    fn trim_range_splits_and_removes() {
        let p = Preset::natural();
        // [speech 0-1000][silence 1000-5000 compress][speech 5000-6000]
        let mut t = gen(&[seg(0, 1000), seg(5000, 6000)], 6000, &no_pad(), &p);
        // 500ms〜5500ms だけを残す
        apply_trim_range(&mut t, 500, 5500).unwrap();
        validate_timeline(&t).unwrap();

        // 先頭 speech は 0-500 (Remove) と 500-1000 (Keep) に分割される
        assert_eq!(t.segments[0].source_end_ms, 500);
        assert_eq!(t.segments[0].action, SegmentAction::Remove);
        assert_eq!(t.segments[1].source_start_ms, 500);
        assert_eq!(t.segments[1].action, SegmentAction::Keep);
        assert_eq!(t.segments[1].keep_duration_ms, 500);

        // 末尾 speech は 5000-5500 (Keep) と 5500-6000 (Remove) に分割される
        let last = t.segments.last().unwrap();
        assert_eq!(last.source_start_ms, 5500);
        assert_eq!(last.action, SegmentAction::Remove);

        // 中央の圧縮無音は範囲内なのでそのまま
        let mid = t
            .segments
            .iter()
            .find(|s| s.kind == SegmentKind::Silence && s.source_duration_ms() == 4000)
            .unwrap();
        assert_eq!(mid.action, SegmentAction::Compress);
        assert_eq!(mid.keep_duration_ms, 900);

        // 出力尺 = 500 (前) + 900 (無音圧縮後) + 500 (後)
        assert_eq!(t.stats.output_duration_ms, 1900);
    }

    /// トリム境界が圧縮無音の中にあるとき、範囲内の残り部分に keep が引き継がれる
    #[test]
    fn trim_inside_compressed_silence() {
        let p = Preset::natural(); // target 900ms
        let mut t = gen(&[seg(0, 1000), seg(5000, 6000)], 6000, &no_pad(), &p);
        // 無音 (1000-5000) の途中 4500ms から残す
        apply_trim_range(&mut t, 4500, 6000).unwrap();
        // 4500-5000 の 500ms は keep 900 とクランプされ全部残る (Keep 扱い)
        let part = t
            .segments
            .iter()
            .find(|s| s.source_start_ms == 4500 && s.source_end_ms == 5000)
            .unwrap();
        assert_eq!(part.action, SegmentAction::Keep);
        assert_eq!(part.keep_duration_ms, 500);
        assert_eq!(t.stats.output_duration_ms, 1500);
    }

    #[test]
    fn trim_range_rejects_invalid() {
        let p = Preset::natural();
        let mut t = gen(&[seg(0, 1000)], 1000, &no_pad(), &p);
        assert!(apply_trim_range(&mut t, 500, 500).is_err());
        assert!(apply_trim_range(&mut t, 0, 2000).is_err());
    }

    #[test]
    fn validate_rejects_gap() {
        let p = Preset::natural();
        let mut t = gen(&[seg(0, 1000), seg(2000, 3000)], 3000, &no_pad(), &p);
        t.segments.remove(1);
        assert!(validate_timeline(&t).is_err());
    }
}
