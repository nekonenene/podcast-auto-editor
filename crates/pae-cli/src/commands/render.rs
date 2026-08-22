use std::path::PathBuf;

use clap::Args;
use pae_core::config::AppConfig;
use pae_core::pipeline::run_job;
use pae_core::types::EditTimeline;

use crate::progress::CliProgress;

#[derive(Args)]
pub struct RenderArgs {
    /// 入力動画ファイル
    pub input: PathBuf,

    /// analyze で生成した timeline.json (手修正したものでもよい)
    #[arg(short, long)]
    pub timeline: PathBuf,

    /// 出力先ディレクトリ
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[command(flatten)]
    pub common: super::run::CommonOpts,
}

pub fn execute(args: RenderArgs) -> anyhow::Result<()> {
    let mut config = AppConfig::load()?;

    let text = std::fs::read_to_string(&args.timeline)?;
    let timeline: EditTimeline = serde_json::from_str(&text)?;

    let mut spec = super::run::build_spec(args.input, args.output, None, &args.common, &config)?;
    spec.timeline = Some(timeline);

    let cancel = super::install_cancel_handler()?;
    let progress = CliProgress::new();
    let report = run_job(&spec, &progress, &cancel)?;
    progress.finish();

    // run と同様、指定された BGM を次回以降のデフォルトとして保存する
    super::run::save_bgm_defaults(&mut config, &args.common)?;
    super::print_report(&report);
    Ok(())
}
