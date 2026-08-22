//! パイプライン全体のオーケストレーション
//! probe → 音声抽出 → VAD → タイムライン → カット → BGM → loudnorm → MP3 → 文字起こし → 出力

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::AppConfig;
use crate::error::{PaeError, Result};
use crate::media::extract::{extract_analysis_wav, read_wav_samples};
use crate::media::ffmpeg::Ffmpeg;
use crate::media::probe::probe;
use crate::media::process::{
    apply_loudnorm, cut_video, encode_mp3, measure_loudness, mix_bgm, BgmOpts, LoudnormTarget,
    VideoEncodeOpts,
};
use crate::output::{render, TranscriptFormat};
use crate::progress::{CancelToken, ProgressReport, ProgressSink, Stage};
use crate::timeline::{generate_timeline, timeline_to_keep_ranges, validate_timeline};
use crate::transcribe::model::{find_model, ModelManager};
use crate::transcribe::{Transcriber, WhisperTranscriber};
use crate::types::{EditTimeline, MediaInfo, Preset, TranscriptSegment, VadParams};
use crate::vad::{SileroVad, Vad};

/// 1回の編集ジョブの指定内容
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    pub bgm: Option<PathBuf>,
    pub bgm_opts: BgmOpts,
    pub preset: Preset,
    pub vad_params: VadParams,
    pub target_lufs: f64,
    pub transcribe: bool,
    pub model: String,
    pub formats: Vec<TranscriptFormat>,
    pub ffmpeg_dir: Option<PathBuf>,
    /// analyze 済みタイムラインを渡すと VAD をスキップして再利用する
    pub timeline: Option<EditTimeline>,
}

impl JobSpec {
    /// 設定ファイルの値をベースに JobSpec を作る
    pub fn from_config(input: PathBuf, output_dir: PathBuf, config: &AppConfig) -> Self {
        Self {
            input,
            output_dir,
            bgm: config.default_bgm.clone(),
            bgm_opts: config.bgm.clone(),
            preset: Preset::by_name(&config.preset).unwrap_or_else(Preset::natural),
            vad_params: VadParams::default(),
            target_lufs: config.target_lufs,
            transcribe: config.transcribe,
            model: config.model.clone(),
            formats: vec![
                TranscriptFormat::Txt,
                TranscriptFormat::Json,
                TranscriptFormat::Srt,
                TranscriptFormat::Markdown,
            ],
            ffmpeg_dir: config.ffmpeg_dir.clone(),
            timeline: None,
        }
    }
}

/// ジョブの実行結果。ベンチマーク表示にも使う
#[derive(Debug, Serialize)]
pub struct JobReport {
    pub outputs: Vec<PathBuf>,
    pub source_duration_ms: u64,
    pub output_duration_ms: u64,
    pub timings: Vec<StageTiming>,
    pub total_duration: Duration,
}

#[derive(Debug, Serialize)]
pub struct StageTiming {
    pub stage: Stage,
    pub duration: Duration,
}

impl JobReport {
    /// real-time factor。処理時間 ÷ 入力の長さ。小さいほど速い
    pub fn real_time_factor(&self) -> f64 {
        self.total_duration.as_secs_f64() / (self.source_duration_ms as f64 / 1000.0)
    }
}

/// 出力ファイル名を入力ファイル名から自動生成する
/// 例: input.mp4 → input-edited.mp4, input-podcast.mp3, input-transcript.srt
pub fn output_path(output_dir: &Path, input: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    output_dir.join(format!("{stem}-{suffix}.{ext}"))
}

/// ステージの実行時間を計測しつつ進捗を通知するヘルパ
pub struct StageRunner<'a> {
    sink: &'a dyn ProgressSink,
    timings: Vec<StageTiming>,
}

impl<'a> StageRunner<'a> {
    pub fn new(sink: &'a dyn ProgressSink) -> Self {
        Self {
            sink,
            timings: Vec::new(),
        }
    }

    pub fn run<T>(
        &mut self,
        stage: Stage,
        f: impl FnOnce(&mut dyn FnMut(f32)) -> Result<T>,
    ) -> Result<T> {
        let sink = self.sink;
        sink.report(&ProgressReport {
            stage,
            fraction: Some(0.0),
            message: None,
        });
        let started = Instant::now();
        let mut on_progress = |fraction: f32| {
            sink.report(&ProgressReport {
                stage,
                fraction: Some(fraction),
                message: None,
            });
        };
        let result = f(&mut on_progress)?;
        self.timings.push(StageTiming {
            stage,
            duration: started.elapsed(),
        });
        sink.report(&ProgressReport {
            stage,
            fraction: Some(1.0),
            message: None,
        });
        Ok(result)
    }
}

/// 編集ジョブを最初から最後まで実行する
pub fn run_job(spec: &JobSpec, sink: &dyn ProgressSink, cancel: &CancelToken) -> Result<JobReport> {
    let job_started = Instant::now();
    let mut runner = StageRunner::new(sink);
    let mut outputs: Vec<PathBuf> = Vec::new();

    std::fs::create_dir_all(&spec.output_dir)?;
    // 中間ファイル置き場。ドロップ時に自動削除されるため、
    // エラーやキャンセルで途中終了しても一時ファイルは残らない
    let temp_dir = tempfile::Builder::new().prefix("pae-").tempdir()?;

    let ffmpeg = Ffmpeg::locate(spec.ffmpeg_dir.as_deref())?;

    tracing::info!(input = %spec.input.display(), "処理を開始します");

    let info: MediaInfo = runner.run(Stage::Probe, |_| probe(&ffmpeg, &spec.input))?;
    tracing::info!(
        duration_ms = info.duration_ms,
        video = ?info.video_codec,
        audio = ?info.audio_codec,
        "入力ファイル情報"
    );
    cancel.check()?;

    // タイムラインを用意する（既存があれば再利用、なければ VAD から生成）
    let timeline = match &spec.timeline {
        Some(t) => {
            validate_timeline(t)?;
            if t.source_duration_ms != info.duration_ms {
                return Err(PaeError::InvalidTimeline(format!(
                    "タイムラインのソース長 {}ms が入力 {}ms と一致しません",
                    t.source_duration_ms, info.duration_ms
                )));
            }
            t.clone()
        }
        None => analyze(
            &ffmpeg,
            &spec.input,
            &info,
            spec,
            &mut runner,
            temp_dir.path(),
            cancel,
        )?,
    };

    tracing::info!(
        segments = timeline.segments.len(),
        compressed = timeline.stats.compressed_count,
        output_ms = timeline.stats.output_duration_ms,
        "タイムラインを生成しました"
    );

    // タイムラインは常に保存する。手修正して `pae render` で再利用できる
    let timeline_path = output_path(&spec.output_dir, &spec.input, "timeline", "json");
    std::fs::write(&timeline_path, serde_json::to_string_pretty(&timeline)?)?;
    outputs.push(timeline_path);

    let output_ms = timeline.stats.output_duration_ms;
    let keep_ranges = timeline_to_keep_ranges(&timeline);
    let encode = VideoEncodeOpts::auto(info.height);

    // カット → (BGM) → loudnorm と進み、最終 MP4 を作る
    let cut_path = temp_dir.path().join("cut.mp4");
    runner.run(Stage::RenderVideo, |p| {
        cut_video(
            &ffmpeg,
            &spec.input,
            &keep_ranges,
            &cut_path,
            output_ms,
            &encode,
            p,
            cancel,
        )
    })?;

    let mixed_path = if let Some(bgm) = &spec.bgm {
        let mixed = temp_dir.path().join("mixed.mp4");
        runner.run(Stage::MixBgm, |p| {
            mix_bgm(
                &ffmpeg,
                &cut_path,
                bgm,
                &mixed,
                output_ms,
                &spec.bgm_opts,
                p,
                cancel,
            )
        })?;
        mixed
    } else {
        cut_path.clone()
    };

    let target = LoudnormTarget {
        i: spec.target_lufs,
        ..LoudnormTarget::default()
    };
    let edited_path = output_path(&spec.output_dir, &spec.input, "edited", "mp4");
    runner.run(Stage::Loudnorm, |p| {
        let measured = measure_loudness(&ffmpeg, &mixed_path, &target, cancel)?;
        tracing::info!(input_i = %measured.input_i, "ラウドネス測定完了");
        apply_loudnorm(
            &ffmpeg,
            &mixed_path,
            &edited_path,
            &target,
            &measured,
            output_ms,
            p,
            cancel,
        )
    })?;
    outputs.push(edited_path.clone());

    let mp3_path = output_path(&spec.output_dir, &spec.input, "podcast", "mp3");
    runner.run(Stage::RenderMp3, |p| {
        encode_mp3(&ffmpeg, &edited_path, &mp3_path, output_ms, p, cancel)
    })?;
    outputs.push(mp3_path);

    // 文字起こしは編集後の音声に対して行う
    // タイムスタンプが完成品の MP4 / MP3 と一致し、SRT がそのまま使えるため
    if spec.transcribe {
        let segments = transcribe_media(
            &ffmpeg,
            &edited_path,
            output_ms,
            spec,
            sink,
            &mut runner,
            temp_dir.path(),
            cancel,
        )?;
        runner.run(Stage::WriteOutputs, |_| {
            for format in &spec.formats {
                let path = output_path(
                    &spec.output_dir,
                    &spec.input,
                    "transcript",
                    format.extension(),
                );
                std::fs::write(&path, render(&segments, *format))?;
                outputs.push(path);
            }
            Ok(())
        })?;
    }

    for path in &outputs {
        tracing::info!(output = %path.display(), "出力しました");
    }

    Ok(JobReport {
        outputs,
        source_duration_ms: info.duration_ms,
        output_duration_ms: output_ms,
        timings: runner.timings,
        total_duration: job_started.elapsed(),
    })
}

/// probe 済みの入力から VAD → タイムライン生成までを行う
/// `pae analyze` からも `run_job` からも使う
pub fn analyze(
    ffmpeg: &Ffmpeg,
    input: &Path,
    info: &MediaInfo,
    spec: &JobSpec,
    runner: &mut StageRunner,
    temp_dir: &Path,
    cancel: &CancelToken,
) -> Result<EditTimeline> {
    let wav_path = temp_dir.join("analysis.wav");
    runner.run(Stage::ExtractAudio, |p| {
        extract_analysis_wav(ffmpeg, input, &wav_path, info.duration_ms, p, cancel)
    })?;

    let speech = runner.run(Stage::Vad, |p| {
        let (samples, sample_rate) = read_wav_samples(&wav_path)?;
        SileroVad.detect(&samples, sample_rate, &spec.vad_params, p, cancel)
    })?;
    tracing::info!(speech_segments = speech.len(), "発話区間を検出しました");

    runner.run(Stage::Timeline, |_| {
        generate_timeline(
            &speech,
            input,
            info.duration_ms,
            &spec.vad_params,
            &spec.preset,
        )
    })
}

/// 完成した動画から音声を抽出して文字起こしする
#[allow(clippy::too_many_arguments)]
fn transcribe_media(
    ffmpeg: &Ffmpeg,
    media: &Path,
    duration_ms: u64,
    spec: &JobSpec,
    sink: &dyn ProgressSink,
    runner: &mut StageRunner,
    temp_dir: &Path,
    cancel: &CancelToken,
) -> Result<Vec<TranscriptSegment>> {
    let model_spec = find_model(&spec.model)
        .ok_or_else(|| PaeError::Transcribe(format!("未知のモデル名です: {}", spec.model)))?;
    let manager = ModelManager::new()?;

    // モデルが未ダウンロードならここで取得する（初回のみネットワークを使う）
    if !manager.is_downloaded(model_spec) {
        sink.report(&ProgressReport {
            stage: Stage::Transcribe,
            fraction: Some(0.0),
            message: Some(format!(
                "文字起こしモデル {} (約{}MB) をダウンロード中",
                model_spec.name, model_spec.approx_size_mb
            )),
        });
    }
    let mut on_dl_progress = |fraction: f32| {
        sink.report(&ProgressReport {
            stage: Stage::Transcribe,
            fraction: Some(fraction),
            message: Some("モデルをダウンロード中".into()),
        });
    };
    let model_path = manager.ensure_model(model_spec, &mut on_dl_progress, cancel)?;

    runner.run(Stage::Transcribe, |p| {
        let wav_path = temp_dir.join("transcribe.wav");
        extract_analysis_wav(ffmpeg, media, &wav_path, duration_ms, &mut |_| {}, cancel)?;
        let (samples, _) = read_wav_samples(&wav_path)?;
        let mut transcriber = WhisperTranscriber::load(&model_path)?;
        transcriber.transcribe(&samples, "ja", p, cancel)
    })
}
