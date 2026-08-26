//! 話者分離を単体で走らせるための補助。
//! タイムラインが無い場面では VAD で発話区間を取り、そこから話者を求める

use std::path::Path;

use pae_core::diarize::{diarize, model::EMBEDDING_MODEL, DiarizeParams, SpeechSpeaker};
use pae_core::progress::{CancelToken, ProgressReport, ProgressSink, Stage};
use pae_core::transcribe::model::ModelManager;
use pae_core::types::VadParams;
use pae_core::vad::{SileroVad, Vad};

/// 16kHz mono のサンプル列から話者分離をおこなう
pub fn diarize_samples(
    samples: &[i16],
    sample_rate: u32,
    params: &DiarizeParams,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> anyhow::Result<Vec<SpeechSpeaker>> {
    let manager = ModelManager::new()?;
    let mut on_dl = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::Diarize,
            fraction: Some(fraction),
            message: Some("話者分離モデルをダウンロード中".into()),
        });
    };
    let model_path = manager.ensure_model(&EMBEDDING_MODEL, &mut on_dl, cancel)?;

    let ranges = detect_speech_ranges(samples, sample_rate, progress, cancel)?;
    let mut on_progress = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::Diarize,
            fraction: Some(fraction),
            message: None,
        });
    };
    Ok(diarize(
        samples,
        sample_rate,
        &ranges,
        params,
        &model_path,
        &mut on_progress,
        cancel,
    )?)
}

fn detect_speech_ranges(
    samples: &[i16],
    sample_rate: u32,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
) -> anyhow::Result<Vec<(u64, u64)>> {
    let mut on_progress = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::Vad,
            fraction: Some(fraction),
            message: None,
        });
    };
    let segments = SileroVad.detect(
        samples,
        sample_rate,
        &VadParams::default(),
        &mut on_progress,
        cancel,
    )?;
    Ok(segments.iter().map(|s| (s.start_ms, s.end_ms)).collect())
}

/// 話者を決めかねているとみなす margin の目安。
/// 判定を変えるものではなく、まとめの表示に使うだけ
const AMBIGUOUS_MARGIN: f32 = 0.1;

/// 話者分離の結果を JSON として書き出す。しきい値の調整と、外れ方の調査に使う
pub fn to_json(speakers: &[SpeechSpeaker]) -> String {
    let items: Vec<_> = speakers
        .iter()
        .map(|s| {
            serde_json::json!({
                "start": s.start_ms as f64 / 1000.0,
                "end": s.end_ms as f64 / 1000.0,
                "duration": s.duration_ms() as f64 / 1000.0,
                "speaker": pae_core::diarize::speaker_label(s.speaker),
                // 各話者の重心との距離。しきい値をいくつにすると何が変わるかを見る
                "distances": round3(&s.distances),
                // 2番目に近い話者との差。小さいほど話者を決めかねている
                "margin": s.margin().map(round),
            })
        })
        .collect();
    serde_json::to_string_pretty(&items).expect("serialize は失敗しない")
}

/// 結果のまとめ。dev コマンドの表示に使う
pub fn summarize(speakers: &[SpeechSpeaker], path: &Path) -> String {
    let unknown = speakers.iter().filter(|s| s.speaker.is_none()).count();
    let ambiguous = speakers
        .iter()
        .filter(|s| s.margin().is_some_and(|m| m < AMBIGUOUS_MARGIN))
        .count();

    let mut durations: Vec<u64> = speakers.iter().map(|s| s.duration_ms()).collect();
    durations.sort_unstable();
    let at = |ratio: f64| -> u64 {
        if durations.is_empty() {
            return 0;
        }
        let index = ((durations.len() - 1) as f64 * ratio) as usize;
        durations[index]
    };

    format!(
        "{}\n発話区間 {} 個 / 話者不明 {} 個 / 決めかねた区間 (margin < {}) {} 個\n\
         区間の長さ: 中央値 {:.1}秒, 90% {:.1}秒, 最長 {:.1}秒",
        path.display(),
        speakers.len(),
        unknown,
        AMBIGUOUS_MARGIN,
        ambiguous,
        at(0.5) as f64 / 1000.0,
        at(0.9) as f64 / 1000.0,
        at(1.0) as f64 / 1000.0,
    )
}

/// JSON を読みやすくするため、小数の桁を落とす
fn round(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

fn round3(values: &[f32]) -> Vec<f32> {
    values.iter().copied().map(round).collect()
}
