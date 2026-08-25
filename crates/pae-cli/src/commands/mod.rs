pub mod analyze;
pub mod dev;
pub mod models;
pub mod probe;
pub mod render;
pub mod run;
pub mod transcribe;

use pae_core::pipeline::JobReport;
use pae_core::progress::CancelToken;

/// Ctrl+C で処理を中断できるようにする。
/// 2回目の Ctrl+C は通常のシグナル動作 (即終了) に任せる
pub fn install_cancel_handler() -> anyhow::Result<CancelToken> {
    let token = CancelToken::new();
    let handler_token = token.clone();
    ctrlc::set_handler(move || {
        if handler_token.is_cancelled() {
            std::process::exit(130);
        }
        eprintln!("\nキャンセルしています... (もう一度 Ctrl+C で強制終了)");
        handler_token.cancel();
    })?;
    Ok(token)
}

/// 処理結果とベンチマーク (各ステージの処理時間、real-time factor) を表示する
pub fn print_report(report: &JobReport) {
    println!();
    println!("完了しました。");
    println!();
    println!("出力ファイル:");
    for path in &report.outputs {
        println!("  {}", path.display());
    }
    println!();
    // BGM の余韻は無音短縮の成果ではないため、短縮率の計算からは外して別に添える
    let edited_ms = report.output_duration_ms.saturating_sub(report.tail_ms);
    let tail_note = if report.tail_ms > 0 {
        format!(" + BGM余韻 {}秒", report.tail_ms / 1000)
    } else {
        String::new()
    };
    println!(
        "{} → {} (短縮率 {:.1}%){}",
        format_duration_ms(report.edited_range_ms),
        format_duration_ms(edited_ms),
        100.0 * (1.0 - edited_ms as f64 / report.edited_range_ms as f64),
        tail_note
    );
    println!();
    println!("処理時間:");
    for timing in &report.timings {
        println!(
            "  {:<24} {:>8.1}s",
            timing.stage.label(),
            timing.duration.as_secs_f64()
        );
    }
    println!(
        "  {:<24} {:>8.1}s",
        "合計",
        report.total_duration.as_secs_f64()
    );
    println!("Real-time factor: {:.2}", report.real_time_factor());
}

pub fn format_duration_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    let m = total_secs / 60;
    let s = total_secs % 60;
    format!("{m}分{s:02}秒")
}
