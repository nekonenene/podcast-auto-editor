use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::media::extract::{extract_analysis_wav, read_wav_samples};
use pae_core::media::ffmpeg::Ffmpeg;
use pae_core::media::probe::probe;
use pae_core::output::{render, TranscriptFormat};
use pae_core::pipeline::output_path;
use pae_core::progress::{ProgressReport, ProgressSink, Stage};
use pae_core::transcribe::model::{find_model, ModelManager};
use pae_core::transcribe::{Transcriber, WhisperTranscriber};

use crate::progress::CliProgress;

#[derive(Args)]
pub struct TranscribeArgs {
    /// 入力ファイル (動画または音声)
    pub input: PathBuf,

    /// 出力先ディレクトリ
    #[arg(short, long, default_value = "output")]
    pub output: PathBuf,

    /// 文字起こしモデル名
    #[arg(long)]
    pub model: Option<String>,

    /// 言語コード (例: ja)
    #[arg(long, default_value = "ja")]
    pub language: String,

    /// 出力フォーマット (カンマ区切り: txt,json,srt,md)
    #[arg(long, default_value = "txt,json,srt,md")]
    pub formats: String,
}

pub fn execute(args: TranscribeArgs) -> anyhow::Result<()> {
    let config = AppConfig::load()?;
    let model_name = args.model.as_deref().unwrap_or(&config.model);
    let model_spec = find_model(model_name)
        .ok_or_else(|| anyhow::anyhow!("未知のモデル名です: {model_name}"))?;

    let formats = args
        .formats
        .split(',')
        .map(|s| {
            TranscriptFormat::parse(s.trim())
                .ok_or_else(|| anyhow::anyhow!("未知のフォーマットです: {s}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let cancel = super::install_cancel_handler()?;
    let progress = CliProgress::new();

    let manager = ModelManager::new()?;
    let mut on_dl = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::Transcribe,
            fraction: Some(fraction),
            message: Some("モデルをダウンロード中".into()),
        });
    };
    let model_path = manager.ensure_model(model_spec, &mut on_dl, &cancel)?;

    let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref())?;
    let info = probe(&ffmpeg, &args.input)?;

    let temp_dir = tempfile::Builder::new().prefix("pae-").tempdir()?;
    let wav_path = temp_dir.path().join("transcribe.wav");
    let mut on_extract = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::ExtractAudio,
            fraction: Some(fraction),
            message: None,
        });
    };
    extract_analysis_wav(
        &ffmpeg,
        &args.input,
        &wav_path,
        info.duration_ms,
        &mut on_extract,
        &cancel,
    )?;

    let (samples, _) = read_wav_samples(&wav_path)?;
    let mut transcriber = WhisperTranscriber::load(&model_path)?;
    let mut on_progress = |fraction: f32| {
        progress.report(&ProgressReport {
            stage: Stage::Transcribe,
            fraction: Some(fraction),
            message: None,
        });
    };
    let segments = transcriber.transcribe(&samples, &args.language, &mut on_progress, &cancel)?;
    progress.finish();

    std::fs::create_dir_all(&args.output)?;
    println!();
    for format in formats {
        let path = output_path(&args.output, &args.input, "transcript", format.extension());
        std::fs::write(&path, render(&segments, format))?;
        println!("出力しました: {}", path.display());
    }
    Ok(())
}
