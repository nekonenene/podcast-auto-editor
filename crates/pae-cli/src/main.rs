mod commands;
mod progress;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pae",
    about = "Podcast Auto Editor - 録画から自然な間の Podcast を自動生成する",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// 詳細ログを stderr に表示する
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// 動画を編集して Podcast 一式 (MP4 / MP3 / 文字起こし) を出力する
    Run(commands::run::RunArgs),
    /// 無音検出とタイムライン生成のみ行い timeline.json を保存する
    Analyze(commands::analyze::AnalyzeArgs),
    /// timeline.json (手修正可) を使って動画を書き出す
    Render(commands::render::RenderArgs),
    /// 文字起こしのみ行う
    Transcribe(commands::transcribe::TranscribeArgs),
    /// 入力メディアの情報を JSON で表示する
    Probe(commands::probe::ProbeArgs),
    /// 文字起こしモデルの管理
    Models(commands::models::ModelsArgs),
    /// 開発・検証用の低レベルコマンド
    #[command(hide = true)]
    Dev(commands::dev::DevArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Run(args) => commands::run::execute(args),
        Command::Analyze(args) => commands::analyze::execute(args),
        Command::Render(args) => commands::render::execute(args),
        Command::Transcribe(args) => commands::transcribe::execute(args),
        Command::Probe(args) => commands::probe::execute(args),
        Command::Models(args) => commands::models::execute(args),
        Command::Dev(args) => commands::dev::execute(args),
    }
}
