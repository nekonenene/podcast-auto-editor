//! ローカル文字起こし。whisper.cpp (whisper-rs) を使い、音声を外部へ送信しない

pub mod model;

use std::path::Path;

use crate::error::{PaeError, Result};
use crate::progress::CancelToken;
use crate::types::TranscriptSegment;

/// 文字起こしの抽象。将来別エンジンへ差し替えられるよう trait にしている
pub trait Transcriber {
    /// 16kHz mono の i16 サンプル列を文字起こしする
    fn transcribe(
        &mut self,
        samples: &[i16],
        language: &str,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<Vec<TranscriptSegment>>;
}

/// whisper.cpp 実装。Apple Silicon では Metal で推論する
pub struct WhisperTranscriber {
    context: whisper_rs::WhisperContext,
}

impl WhisperTranscriber {
    pub fn load(model_path: &Path) -> Result<Self> {
        if !model_path.exists() {
            return Err(PaeError::Transcribe(format!(
                "モデルファイルがありません: {}",
                model_path.display()
            )));
        }
        // whisper.cpp 自身のログは冗長なので tracing 経由に流す
        whisper_rs::install_logging_hooks();

        let params = whisper_rs::WhisperContextParameters::default();
        let context = whisper_rs::WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| PaeError::Transcribe("モデルパスが UTF-8 ではありません".into()))?,
            params,
        )
        .map_err(|e| PaeError::Transcribe(format!("モデルの読み込みに失敗: {e}")))?;
        Ok(Self { context })
    }
}

impl Transcriber for WhisperTranscriber {
    fn transcribe(
        &mut self,
        samples: &[i16],
        language: &str,
        on_progress: &mut dyn FnMut(f32),
        cancel: &CancelToken,
    ) -> Result<Vec<TranscriptSegment>> {
        cancel.check()?;

        let mut audio = vec![0.0f32; samples.len()];
        whisper_rs::convert_integer_to_float_audio(samples, &mut audio)
            .map_err(|e| PaeError::Transcribe(format!("音声変換に失敗: {e}")))?;

        let mut state = self
            .context
            .create_state()
            .map_err(|e| PaeError::Transcribe(format!("状態の作成に失敗: {e}")))?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(language));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // whisper のコールバックは 'static を要求するため、進捗の百分率を
        // Atomic に書いてもらい、こちら側のスレッドから on_progress へ中継する
        let progress_pct = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
        let progress_writer = progress_pct.clone();
        params.set_progress_callback_safe(move |percent: i32| {
            progress_writer.store(percent, std::sync::atomic::Ordering::Relaxed);
        });

        // キャンセル時は whisper.cpp の処理自体を中断させる。
        // whisper-rs 0.16 の set_abort_callback_safe はクロージャの二重 Box と
        // trampoline の型が食い違うバグがあり未定義動作になるため、
        // 生ポインタ API に AtomicBool を渡す方式で回避している
        unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
            let flag = &*(user_data as *const std::sync::atomic::AtomicBool);
            flag.load(std::sync::atomic::Ordering::Relaxed)
        }
        // Arc は cancel (引数) が full() の間ずっと生きているためポインタは有効
        let cancel_flag = cancel.flag();
        unsafe {
            params.set_abort_callback(Some(abort_trampoline));
            params.set_abort_callback_user_data(
                std::sync::Arc::as_ptr(&cancel_flag) as *mut std::ffi::c_void
            );
        }

        // 推論は別スレッドで走らせ、このスレッドで進捗を中継する
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| state.full(params, &audio));
            let mut last_pct = -1;
            while !handle.is_finished() {
                let pct = progress_pct.load(std::sync::atomic::Ordering::Relaxed);
                if pct != last_pct {
                    last_pct = pct;
                    on_progress(pct as f32 / 100.0);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            handle
                .join()
                .map_err(|_| PaeError::Transcribe("推論スレッドが異常終了しました".into()))?
                .map_err(|e| PaeError::Transcribe(format!("推論に失敗: {e}")))
        })?;

        cancel.check()?;

        let n_segments = state.full_n_segments();
        let mut segments = Vec::with_capacity(n_segments as usize);
        for i in 0..n_segments {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            let text = segment
                .to_str_lossy()
                .map_err(|e| PaeError::Transcribe(format!("テキスト取得に失敗: {e}")))?;
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            // whisper のタイムスタンプは 10ms (centisecond) 単位
            let t0 = segment.start_timestamp();
            let t1 = segment.end_timestamp();
            segments.push(TranscriptSegment {
                start_ms: (t0.max(0) as u64) * 10,
                end_ms: (t1.max(0) as u64) * 10,
                text: text.to_string(),
                speaker: None,
            });
        }
        Ok(segments)
    }
}
