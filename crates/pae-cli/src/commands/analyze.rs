use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::media::ffmpeg::Ffmpeg;
use pae_core::media::probe::probe;
use pae_core::pipeline::{analyze, JobSpec, StageRunner};
use pae_core::types::{Preset, VadParams};

use crate::progress::CliProgress;

#[derive(Args)]
pub struct AnalyzeArgs {
    /// 入力動画ファイル
    pub input: PathBuf,

    /// タイムラインの保存先 (デフォルト: <入力名>-timeline.json)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// 無音短縮プリセット (natural / standard / aggressive)
    #[arg(long)]
    pub preset: Option<String>,

    /// VAD の発話判定しきい値 (0.0〜1.0)
    #[arg(long)]
    pub vad_threshold: Option<f32>,

    /// 発話とみなす最小長 (ms)
    #[arg(long)]
    pub min_speech: Option<u64>,

    /// 発話をつなげる最大無音長 (ms)
    #[arg(long)]
    pub min_silence: Option<u64>,

    /// 発話前の余白 (ms)
    #[arg(long)]
    pub pad_before: Option<u64>,

    /// 発話後の余白 (ms)
    #[arg(long)]
    pub pad_after: Option<u64>,
}

pub fn execute(args: AnalyzeArgs) -> anyhow::Result<()> {
    let config = AppConfig::load()?;

    let mut vad_params = VadParams::default();
    if let Some(v) = args.vad_threshold {
        vad_params.threshold = v;
    }
    if let Some(v) = args.min_speech {
        vad_params.min_speech_ms = v;
    }
    if let Some(v) = args.min_silence {
        vad_params.min_silence_ms = v;
    }
    if let Some(v) = args.pad_before {
        vad_params.pad_before_ms = v;
    }
    if let Some(v) = args.pad_after {
        vad_params.pad_after_ms = v;
    }

    let preset = match &args.preset {
        Some(name) => {
            Preset::by_name(name).ok_or_else(|| anyhow::anyhow!("未知のプリセットです: {name}"))?
        }
        None => Preset::by_name(&config.preset).unwrap_or_else(Preset::natural),
    };

    let mut spec = JobSpec::from_config(args.input.clone(), PathBuf::from("."), &config);
    spec.vad_params = vad_params;
    spec.preset = preset;

    let output = args.output.unwrap_or_else(|| {
        let stem = args
            .input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "input".into());
        PathBuf::from(format!("{stem}-timeline.json"))
    });

    let cancel = super::install_cancel_handler()?;
    let progress = CliProgress::new();
    let mut runner = StageRunner::new(&progress);

    let ffmpeg = Ffmpeg::locate(spec.ffmpeg_dir.as_deref())?;
    let info = probe(&ffmpeg, &spec.input)?;
    let temp_dir = tempfile::Builder::new().prefix("pae-").tempdir()?;
    let timeline = analyze(
        &ffmpeg,
        &spec.input,
        &info,
        &spec,
        &mut runner,
        temp_dir.path(),
        &cancel,
    )?;
    progress.finish();

    std::fs::write(&output, serde_json::to_string_pretty(&timeline)?)?;

    println!();
    println!("タイムラインを保存しました: {}", output.display());
    println!(
        "  無音区間: {} 個 (うち短縮: {} 個)",
        timeline.stats.silence_count, timeline.stats.compressed_count
    );
    println!(
        "  {} → {} (短縮率 {:.1}%)",
        super::format_duration_ms(timeline.stats.source_duration_ms),
        super::format_duration_ms(timeline.stats.output_duration_ms),
        100.0
            * (1.0
                - timeline.stats.output_duration_ms as f64
                    / timeline.stats.source_duration_ms as f64)
    );
    Ok(())
}
