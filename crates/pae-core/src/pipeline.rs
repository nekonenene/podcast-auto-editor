//! パイプライン全体のオーケストレーション。
//! probe → 音声抽出 → VAD → タイムライン → カット → BGM → loudnorm → MP3 → 文字起こし → 出力

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{AppConfig, OutputSelection};
use crate::error::{PaeError, Result};
use crate::media::extract::{extract_analysis_wav, read_wav_samples};
use crate::media::ffmpeg::Ffmpeg;
use crate::media::probe::probe;
use crate::media::process::{
    apply_loudnorm, cut_media, encode_mp3, measure_loudness, mix_bgm, BgmOpts, LoudnormTarget,
    VideoEncodeOpts,
};
use crate::output::render;
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
    pub outputs: OutputSelection,
    /// Podcast MP3 のビットレート (kbps)
    pub mp3_bitrate_kbps: u32,
    pub ffmpeg_dir: Option<PathBuf>,
    /// analyze 済みタイムラインを渡すと VAD をスキップして再利用する
    pub timeline: Option<EditTimeline>,
    /// 出力する範囲 (ミリ秒)。収録前後の無駄話をカットする。None なら全体
    pub trim_range_ms: Option<(u64, u64)>,
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
            outputs: config.outputs.clone(),
            mp3_bitrate_kbps: config.mp3_bitrate_kbps,
            ffmpeg_dir: config.ffmpeg_dir.clone(),
            timeline: None,
            trim_range_ms: None,
        }
    }
}

/// ジョブの実行結果。ベンチマーク表示にも使う
#[derive(Debug, Serialize)]
pub struct JobReport {
    pub outputs: Vec<PathBuf>,
    pub source_duration_ms: u64,
    /// 編集の対象になった範囲の長さ。出力範囲の指定がなければ入力の長さと同じ
    pub edited_range_ms: u64,
    pub output_duration_ms: u64,
    /// 出力の末尾へ足した BGM 余韻の長さ。BGM なしなら 0
    pub tail_ms: u64,
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

/// 出力ファイル名を入力ファイル名から自動生成する。
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

    let transcript_formats = spec.outputs.transcript_formats();
    let want_transcribe = spec.transcribe && !transcript_formats.is_empty();
    if !spec.outputs.any_selected() {
        return Err(PaeError::Config(
            "出力するファイルがひとつも選択されていません。設定を確認してください".into(),
        ));
    }

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
    let mut timeline = match &spec.timeline {
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

    // 出力範囲が指定されていれば、範囲外を Remove にする
    let mut edited_range_ms = info.duration_ms;
    if let Some((start_ms, end_ms)) = spec.trim_range_ms {
        let end_ms = end_ms.min(info.duration_ms);
        crate::timeline::apply_trim_range(&mut timeline, start_ms, end_ms)?;
        edited_range_ms = end_ms.saturating_sub(start_ms);
        tracing::info!(start_ms, end_ms, "出力範囲を適用しました");
    }

    tracing::info!(
        segments = timeline.segments.len(),
        compressed = timeline.stats.compressed_count,
        output_ms = timeline.stats.output_duration_ms,
        "タイムラインを生成しました"
    );

    // タイムラインを保存しておくと、手修正して `pae render` で再利用できる
    if spec.outputs.timeline_json {
        let timeline_path = output_path(&spec.output_dir, &spec.input, "timeline", "json");
        std::fs::write(&timeline_path, serde_json::to_string_pretty(&timeline)?)?;
        outputs.push(timeline_path);
    }

    // BGM を付けるときだけ、末尾に BGM の余韻分の静止映像・無音を足す。
    // 会話終了後に BGM だけが残ってフェードアウトするエンディングになる
    let tail_ms = if spec.bgm.is_some() {
        (spec.bgm_opts.ending_tail_s.max(0.0) * 1000.0) as u64
    } else {
        0
    };
    let output_ms = timeline.stats.output_duration_ms + tail_ms;
    let keep_ranges = timeline_to_keep_ranges(&timeline);
    let encode = VideoEncodeOpts::auto(info.height);

    // 音声のみの入力 (mp3 / wav 等) では動画を作れないため、
    // 中間ファイルを可逆の FLAC にして音声だけのパイプラインで処理する
    let has_video = info.has_video;
    let intermediate_ext = if has_video { "mp4" } else { "flac" };
    if !has_video && spec.outputs.edited_mp4 {
        tracing::info!("入力に映像がないため、編集済み MP4 の出力はスキップします");
    }
    if !has_video && !spec.outputs.podcast_mp3 && !spec.outputs.timeline_json && !want_transcribe {
        return Err(PaeError::Config(
            "音声入力では編集済み MP4 を出力できません。MP3 や文字起こしなど他の出力を選択してください".into(),
        ));
    }

    // カット → (BGM) → loudnorm と進み、完成版を作る
    let cut_path = temp_dir.path().join(format!("cut.{intermediate_ext}"));
    runner.run(Stage::RenderVideo, |p| {
        cut_media(
            &ffmpeg,
            &spec.input,
            &keep_ranges,
            &cut_path,
            output_ms,
            tail_ms,
            has_video,
            &encode,
            p,
            cancel,
        )
    })?;

    let mixed_path = if let Some(bgm) = &spec.bgm {
        let mixed = temp_dir.path().join(format!("mixed.{intermediate_ext}"));
        runner.run(Stage::MixBgm, |p| {
            mix_bgm(
                &ffmpeg,
                &cut_path,
                bgm,
                &mixed,
                output_ms,
                has_video,
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
    // MP4 を出力しない設定でも、MP3 や文字起こしの元として完成版は一時的に作る
    let write_edited_mp4 = spec.outputs.edited_mp4 && has_video;
    let edited_path = if write_edited_mp4 {
        output_path(&spec.output_dir, &spec.input, "edited", "mp4")
    } else {
        temp_dir.path().join(format!("edited.{intermediate_ext}"))
    };
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
            has_video,
            p,
            cancel,
        )
    })?;
    if write_edited_mp4 {
        outputs.push(edited_path.clone());
    }

    if spec.outputs.podcast_mp3 {
        let mp3_path = output_path(&spec.output_dir, &spec.input, "podcast", "mp3");
        runner.run(Stage::RenderMp3, |p| {
            encode_mp3(
                &ffmpeg,
                &edited_path,
                &mp3_path,
                output_ms,
                spec.mp3_bitrate_kbps,
                p,
                cancel,
            )
        })?;
        outputs.push(mp3_path);
    }

    // 文字起こしは編集後の音声に対して行う。
    // タイムスタンプが完成品の MP4 / MP3 と一致し、SRT がそのまま使えるため
    if want_transcribe {
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
            for format in &transcript_formats {
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
        edited_range_ms,
        output_duration_ms: output_ms,
        tail_ms,
        timings: runner.timings,
        total_duration: job_started.elapsed(),
    })
}

/// probe 済みの入力から VAD → タイムライン生成までを行う。
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
