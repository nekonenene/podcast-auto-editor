use clap::{Args, Subcommand};
use pae_core::progress::{ProgressReport, ProgressSink, Stage};
use pae_core::transcribe::model::{find_model, ModelManager, MODELS};

use crate::progress::CliProgress;

#[derive(Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// 利用可能なモデルの一覧を表示する
    List,
    /// モデルをダウンロードする
    Download {
        /// モデル名 (list で確認)
        name: String,
    },
    /// モデルの保存先ディレクトリを表示する
    Path,
}

pub fn execute(args: ModelsArgs) -> anyhow::Result<()> {
    let manager = ModelManager::new()?;
    match args.command {
        ModelsCommand::List => {
            for spec in MODELS {
                let status = if manager.is_downloaded(spec) {
                    "ダウンロード済み"
                } else {
                    "未ダウンロード"
                };
                println!(
                    "{:<18} 約{:>4}MB  [{}]  {}",
                    spec.name, spec.approx_size_mb, status, spec.description
                );
            }
        }
        ModelsCommand::Download { name } => {
            let spec =
                find_model(&name).ok_or_else(|| anyhow::anyhow!("未知のモデル名です: {name}"))?;
            let cancel = super::install_cancel_handler()?;
            let progress = CliProgress::new();
            let mut on_progress = |fraction: f32| {
                progress.report(&ProgressReport {
                    stage: Stage::Transcribe,
                    fraction: Some(fraction),
                    message: Some(format!("{} をダウンロード中", spec.name)),
                });
            };
            let path = manager.ensure_model(spec, &mut on_progress, &cancel)?;
            progress.finish();
            println!("ダウンロード完了: {}", path.display());
        }
        ModelsCommand::Path => {
            println!("{}", manager.models_dir().display());
        }
    }
    Ok(())
}
