//! 話者分離。発話区間ごとに声質のベクトルを求め、指定された人数へ分ける。
//!
//! 1トラックへミックス済みの録音では、被って話した部分を分けきれない。
//! 完璧を狙わず、決め手が無いときは「話者不明」に落として誤ったラベルを避ける方針にしている

pub mod assign;
pub mod cluster;
pub mod embed;
pub mod model;
pub mod window;

use std::path::Path;

use crate::error::Result;
use crate::progress::CancelToken;
use crate::types::TranscriptSegment;
use assign::assign_speakers;
use cluster::{cluster_speakers, nearest_speaker};
use embed::SpeakerEmbedder;
use window::{assign_windows, learn_windows};

/// これより短い窓は埋め込みが安定しないため、埋め込みを取らずに話者不明とする。
/// 日本語の相槌はこの長さを下回ることが多く、混ぜるとクラスタの重心が濁る
const MIN_EMBEDDING_MS: u64 = 400;

/// 進捗表示のうち、話者を学ぶ段が占める割合。
/// 学習用の窓は数が少ないぶん1個あたりが重いため、半々くらいで見ておく
const LEARN_PROGRESS_SHARE: f32 = 0.5;

/// 話者を決められなかった区間へ付けるラベル。
/// 話者分離をしていない状態 (None) と区別できるよう、明示的な文字列にしている
pub const UNKNOWN_SPEAKER_LABEL: &str = "話者不明";

/// 話者分離の調整値
#[derive(Debug, Clone, Copy)]
pub struct DiarizeParams {
    /// 収録に参加している人数。この数へ分ける
    pub speaker_count: u32,
    /// クラスタ重心からこれ以上離れた区間は話者不明にする
    pub max_center_distance: f32,
}

impl Default for DiarizeParams {
    fn default() -> Self {
        Self {
            speaker_count: 2,
            max_center_distance: cluster::DEFAULT_MAX_CENTER_DISTANCE,
        }
    }
}

/// 区間ごとの話者分離の結果。dev コマンドでの確認にも使う
#[derive(Debug, Clone)]
pub struct SpeechSpeaker {
    pub start_ms: u64,
    pub end_ms: u64,
    /// 0 から始まる話者番号。決められなかった区間は None
    pub speaker: Option<usize>,
    /// 各話者の重心とのコサイン距離。並びは話者番号と同じ。
    /// 短すぎて埋め込みを取らなかった区間は空になる
    pub distances: Vec<f32>,
}

impl SpeechSpeaker {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }

    /// もっとも近い話者と、次に近い話者との距離の差。
    /// 小さいほど決めかねている。ふたりの発話が混ざった区間を見つける手がかりになる
    pub fn margin(&self) -> Option<f32> {
        let mut sorted = self.distances.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        match sorted.as_slice() {
            [best, second, ..] => Some(second - best),
            _ => None,
        }
    }
}

/// 発話区間ごとに話者を求める。
///
/// samples は 16kHz mono、ranges は samples と同じ時間軸のミリ秒で表す。
/// 戻り値は発話区間そのものではなく、区間を切り分けた窓ごとになる
pub fn diarize(
    samples: &[i16],
    sample_rate: u32,
    ranges: &[(u64, u64)],
    params: &DiarizeParams,
    model_path: &Path,
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<Vec<SpeechSpeaker>> {
    let mut embedder = SpeakerEmbedder::load(model_path)?;

    // 話者を学ぶ段。長めの窓から重心を作る。
    // 短い窓ではクラスタが作れないため、ここは長さを優先する
    let learn = learn_windows(ranges);
    let learn_embeddings = embed_windows(
        &mut embedder,
        samples,
        sample_rate,
        &learn,
        &mut |f| on_progress(f * LEARN_PROGRESS_SHARE),
        cancel,
    )?;

    let clustering = cluster_speakers(
        &learn_embeddings.embeddings,
        params.speaker_count as usize,
        params.max_center_distance,
    );

    // 話者を判定する段。短い窓を、できあがった重心へ照らす。
    // 長い発言に挟まった短い発言を、周りへ飲まれる前に拾うため
    let assign = assign_windows(ranges);
    let assign_embeddings = embed_windows(
        &mut embedder,
        samples,
        sample_rate,
        &assign,
        &mut |f| on_progress(LEARN_PROGRESS_SHARE + f * (1.0 - LEARN_PROGRESS_SHARE)),
        cancel,
    )?;
    on_progress(1.0);

    let mut result: Vec<SpeechSpeaker> = assign
        .iter()
        .map(|&(start_ms, end_ms)| SpeechSpeaker {
            start_ms,
            end_ms,
            speaker: None,
            distances: Vec::new(),
        })
        .collect();
    for (&index, embedding) in assign_embeddings
        .indexes
        .iter()
        .zip(&assign_embeddings.embeddings)
    {
        let assignment =
            nearest_speaker(&clustering.centers, embedding, params.max_center_distance);
        result[index].speaker = assignment.speaker;
        result[index].distances = assignment.distances;
    }
    Ok(result)
}

/// 埋め込みを取れた窓と、その窓が元の一覧で何番目だったか
struct Embeddings {
    indexes: Vec<usize>,
    embeddings: Vec<Vec<f32>>,
}

/// 窓ごとに埋め込みを求める。短すぎる窓は飛ばす
fn embed_windows(
    embedder: &mut SpeakerEmbedder,
    samples: &[i16],
    sample_rate: u32,
    windows: &[(u64, u64)],
    on_progress: &mut dyn FnMut(f32),
    cancel: &CancelToken,
) -> Result<Embeddings> {
    let indexes: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, (start, end))| end.saturating_sub(*start) >= MIN_EMBEDDING_MS)
        .map(|(i, _)| i)
        .collect();

    let mut embeddings = Vec::with_capacity(indexes.len());
    for (done, &index) in indexes.iter().enumerate() {
        cancel.check()?;
        on_progress(done as f32 / indexes.len().max(1) as f32);

        let (start_ms, end_ms) = windows[index];
        let slice = slice_samples(samples, sample_rate, start_ms, end_ms);
        embeddings.push(embedder.embed(slice)?);
    }
    Ok(Embeddings {
        indexes,
        embeddings,
    })
}

/// 話者分離の結果を文字起こしのセグメントへ書き込む
pub fn label_transcript(segments: &mut [TranscriptSegment], speech_speakers: &[SpeechSpeaker]) {
    let ranges: Vec<(u64, u64)> = speech_speakers
        .iter()
        .map(|s| (s.start_ms, s.end_ms))
        .collect();
    let speakers: Vec<Option<usize>> = speech_speakers.iter().map(|s| s.speaker).collect();

    let assigned = assign_speakers(segments, &ranges, &speakers);
    for (segment, speaker) in segments.iter_mut().zip(assigned) {
        segment.speaker = Some(speaker_label(speaker));
    }
}

/// 話者番号を表示用のラベルにする。番号は 0 始まりなので 1 を足して読みやすくする
pub fn speaker_label(speaker: Option<usize>) -> String {
    match speaker {
        Some(index) => format!("話者{}", index + 1),
        None => UNKNOWN_SPEAKER_LABEL.to_string(),
    }
}

fn slice_samples(samples: &[i16], sample_rate: u32, start_ms: u64, end_ms: u64) -> &[i16] {
    let to_index = |ms: u64| (ms * sample_rate as u64 / 1000) as usize;
    let start = to_index(start_ms).min(samples.len());
    let end = to_index(end_ms).min(samples.len());
    &samples[start..end.max(start)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_labels_start_at_one() {
        assert_eq!(speaker_label(Some(0)), "話者1");
        assert_eq!(speaker_label(Some(1)), "話者2");
        assert_eq!(speaker_label(None), "話者不明");
    }

    #[test]
    fn slices_by_milliseconds() {
        let samples: Vec<i16> = (0..16_000).map(|i| i as i16).collect();
        let slice = slice_samples(&samples, 16_000, 100, 200);
        assert_eq!(slice.len(), 1_600);
        assert_eq!(slice[0], 1_600);
    }

    /// 音声の末尾を越える範囲を渡しても壊れない
    #[test]
    fn slice_beyond_the_end_is_clamped() {
        let samples: Vec<i16> = vec![0; 1_600];
        assert!(slice_samples(&samples, 16_000, 500, 900).is_empty());
    }
}
