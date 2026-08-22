//! indicatif による CLI 用の進捗表示

use std::sync::Mutex;

use indicatif::{ProgressBar, ProgressStyle};
use pae_core::progress::{ProgressReport, ProgressSink, Stage};

pub struct CliProgress {
    state: Mutex<State>,
}

struct State {
    bar: Option<ProgressBar>,
    stage: Option<Stage>,
}

impl CliProgress {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                bar: None,
                stage: None,
            }),
        }
    }
}

impl ProgressSink for CliProgress {
    fn report(&self, report: &ProgressReport) {
        let mut state = self.state.lock().expect("progress lock");

        // ステージが変わったら前のバーを完了させて新しいバーを作る
        if state.stage != Some(report.stage) {
            if let Some(bar) = state.bar.take() {
                bar.finish();
            }
            let bar = ProgressBar::new(100);
            bar.set_style(
                ProgressStyle::with_template("{prefix:<24} [{bar:30}] {percent:>3}% {msg}")
                    .expect("progress style")
                    .progress_chars("=> "),
            );
            bar.set_prefix(report.stage.label().to_string());
            state.bar = Some(bar);
            state.stage = Some(report.stage);
        }

        if let Some(bar) = &state.bar {
            if let Some(fraction) = report.fraction {
                bar.set_position((fraction * 100.0).clamp(0.0, 100.0) as u64);
            }
            if let Some(message) = &report.message {
                bar.set_message(message.clone());
            }
        }
    }
}

impl CliProgress {
    pub fn finish(&self) {
        let mut state = self.state.lock().expect("progress lock");
        if let Some(bar) = state.bar.take() {
            bar.finish();
        }
    }
}
