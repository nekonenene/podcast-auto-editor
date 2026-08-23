use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::output::TranscriptFormat;
use pae_core::pipeline::{run_job, JobSpec};
use pae_core::types::Preset;

use crate::progress::CliProgress;

/// run / render で共通の出力オプション
#[derive(Args)]
pub struct CommonOpts {
    /// BGM ファイル。指定すると設定に保存され次回以降のデフォルトになる
    #[arg(long)]
    pub bgm: Option<PathBuf>,

    /// BGM を付けない (設定済みのデフォルト BGM も無視する)
    #[arg(long)]
    pub no_bgm: bool,

    /// BGM の音量 (会話に対する倍率, 例: 0.15)
    #[arg(long)]
    pub bgm_volume: Option<f32>,

    /// BGM のフェードイン時間 (秒)
    #[arg(long)]
    pub fade_in: Option<f32>,

    /// BGM のフェードアウト時間 (秒)
    #[arg(long)]
    pub fade_out: Option<f32>,

    /// 会話終了後に BGM だけを残す余韻の長さ (秒, 0 で無効)
    #[arg(long)]
    pub ending_tail: Option<f32>,

    /// 声の帯域で BGM を下げる量 (dB, 負の値。0 で無効)
    #[arg(long)]
    pub bgm_duck: Option<f32>,

    /// ラウドネスターゲット (LUFS, 例: -16)
    #[arg(long)]
    pub lufs: Option<f64>,

    /// 文字起こしモデル名
    #[arg(long)]
    pub model: Option<String>,

    /// 文字起こしをスキップする
    #[arg(long)]
    pub skip_transcribe: bool,

    /// 文字起こしの出力フォーマット (カンマ区切り: txt,json,srt,md)
    #[arg(long)]
    pub formats: Option<String>,
}

#[derive(Args)]
pub struct RunArgs {
    /// 入力動画ファイル
    pub input: PathBuf,

    /// 出力先ディレクトリ
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// 無音短縮プリセット (natural / standard / aggressive)
    #[arg(long)]
    pub preset: Option<String>,

    #[command(flatten)]
    pub common: CommonOpts,
}

/// CLI 引数 > 設定ファイル > デフォルト値 の優先順で JobSpec を組み立てる
pub fn build_spec(
    input: PathBuf,
    output: Option<PathBuf>,
    preset: Option<&str>,
    opts: &CommonOpts,
    config: &AppConfig,
) -> anyhow::Result<JobSpec> {
    let output_dir = output
        .or_else(|| config.output_dir.clone())
        .unwrap_or_else(|| PathBuf::from("output"));

    let mut spec = JobSpec::from_config(input, output_dir, config);

    if let Some(name) = preset {
        spec.preset =
            Preset::by_name(name).ok_or_else(|| anyhow::anyhow!("未知のプリセットです: {name}"))?;
    }
    if opts.no_bgm {
        spec.bgm = None;
    } else if let Some(bgm) = &opts.bgm {
        spec.bgm = Some(bgm.clone());
    }
    if let Some(v) = opts.bgm_volume {
        spec.bgm_opts.volume = v;
    }
    if let Some(v) = opts.fade_in {
        spec.bgm_opts.fade_in_s = v;
    }
    if let Some(v) = opts.fade_out {
        spec.bgm_opts.fade_out_s = v;
    }
    if let Some(v) = opts.ending_tail {
        spec.bgm_opts.ending_tail_s = v;
    }
    if let Some(v) = opts.bgm_duck {
        spec.bgm_opts.voice_duck_db = v;
    }
    if let Some(v) = opts.lufs {
        spec.target_lufs = v;
    }
    if let Some(model) = &opts.model {
        spec.model = model.clone();
    }
    if opts.skip_transcribe {
        spec.transcribe = false;
    }
    if let Some(formats) = &opts.formats {
        spec.formats = formats
            .split(',')
            .map(|s| {
                TranscriptFormat::parse(s.trim())
                    .ok_or_else(|| anyhow::anyhow!("未知のフォーマットです: {s}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
    }
    Ok(spec)
}

/// 一度指定した BGM を次回以降のデフォルトとして設定へ保存する
pub fn save_bgm_defaults(config: &mut AppConfig, opts: &CommonOpts) -> anyhow::Result<()> {
    let mut changed = false;
    if let Some(bgm) = &opts.bgm {
        config.default_bgm = Some(bgm.clone());
        changed = true;
    }
    if let Some(v) = opts.bgm_volume {
        config.bgm.volume = v;
        changed = true;
    }
    if changed {
        config.save()?;
    }
    Ok(())
}

pub fn execute(args: RunArgs) -> anyhow::Result<()> {
    let mut config = AppConfig::load()?;
    let spec = build_spec(
        args.input,
        args.output,
        args.preset.as_deref(),
        &args.common,
        &config,
    )?;

    let cancel = super::install_cancel_handler()?;
    let progress = CliProgress::new();
    let report = run_job(&spec, &progress, &cancel)?;
    progress.finish();

    save_bgm_defaults(&mut config, &args.common)?;
    super::print_report(&report);
    Ok(())
}
