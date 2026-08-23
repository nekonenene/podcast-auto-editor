//! フロントエンドから invoke される Tauri コマンド群。
//! ビジネスロジックは持たず、pae-core の呼び出しと DTO 変換だけを行う

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::State;

use pae_core::config::AppConfig;
use pae_core::media::ffmpeg::Ffmpeg;
use pae_core::media::probe::probe;
use pae_core::pipeline::{run_job, JobSpec};
use pae_core::progress::{CancelToken, ProgressReport, ProgressSink, Stage};
use pae_core::transcribe::model::{ModelManager, MODELS};
use pae_core::types::{MediaInfo, Preset, VadParams};

/// 実行中ジョブのキャンセルトークン置き場。同時実行は1ジョブに制限する
pub struct JobState(pub Mutex<Option<CancelToken>>);

/// フロントエンドへ送る進捗イベント
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub stage: Stage,
    pub stage_label: String,
    pub fraction: Option<f32>,
    pub message: Option<String>,
}

struct ChannelSink(Channel<ProgressEvent>);

impl ProgressSink for ChannelSink {
    fn report(&self, report: &ProgressReport) {
        // 送信失敗はフロント側の再描画が止まるだけなので処理は継続する
        let _ = self.0.send(ProgressEvent {
            stage: report.stage,
            stage_label: report.stage.label().to_string(),
            fraction: report.fraction,
            message: report.message.clone(),
        });
    }
}

/// GUI の編集開始リクエスト
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRequest {
    pub input: PathBuf,
    pub output_dir: PathBuf,
    pub bgm: Option<PathBuf>,
    pub bgm_volume: f32,
    pub fade_in_s: f32,
    pub fade_out_s: f32,
    pub ending_tail_s: f32,
    pub preset: String,
    pub transcribe: bool,
    pub model: String,
    /// 出力範囲 (ミリ秒)。全体を出力するときは None
    pub trim_start_ms: Option<u64>,
    pub trim_end_ms: Option<u64>,
}

/// GUI へ返すジョブ結果
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    pub outputs: Vec<String>,
    pub source_duration_ms: u64,
    pub output_duration_ms: u64,
    pub timings: Vec<StageSeconds>,
    pub total_seconds: f64,
    pub real_time_factor: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageSeconds {
    pub stage_label: String,
    pub seconds: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub name: String,
    pub approx_size_mb: u64,
    pub description: String,
    pub downloaded: bool,
}

#[tauri::command]
pub fn get_config() -> Result<AppConfig, String> {
    AppConfig::load().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelInfo>, String> {
    let manager = ModelManager::new().map_err(|e| e.to_string())?;
    Ok(MODELS
        .iter()
        .map(|spec| ModelInfo {
            name: spec.name.to_string(),
            approx_size_mb: spec.approx_size_mb,
            description: spec.description.to_string(),
            downloaded: manager.is_downloaded(spec),
        })
        .collect())
}

/// 波形表示用のデータ
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformData {
    pub duration_ms: u64,
    /// 各区間のピーク (0.0〜1.0)
    pub peaks: Vec<f32>,
}

/// 入力メディアの波形データを計算する
#[tauri::command]
pub async fn waveform(path: PathBuf) -> Result<WaveformData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = AppConfig::load().map_err(|e| e.to_string())?;
        let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref()).map_err(|e| e.to_string())?;
        let info = probe(&ffmpeg, &path).map_err(|e| e.to_string())?;

        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let wav = dir.path().join("waveform.wav");
        pae_core::media::extract::extract_analysis_wav(
            &ffmpeg,
            &path,
            &wav,
            info.duration_ms,
            &mut |_| {},
            &CancelToken::new(),
        )
        .map_err(|e| e.to_string())?;
        let (samples, _) =
            pae_core::media::extract::read_wav_samples(&wav).map_err(|e| e.to_string())?;

        Ok(WaveformData {
            duration_ms: info.duration_ms,
            peaks: pae_core::media::waveform::compute_waveform(&samples, 1500),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 選択した入力ファイルを asset プロトコルで再生できるようにする。
/// GUI の波形プレビューが <audio> で元ファイルを直接シーク再生するために使う
#[tauri::command]
pub fn allow_media_preview(app: tauri::AppHandle, path: PathBuf) -> Result<(), String> {
    use tauri::Manager;
    app.asset_protocol_scope()
        .allow_file(&path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn probe_media(path: PathBuf) -> Result<MediaInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = AppConfig::load().map_err(|e| e.to_string())?;
        let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref()).map_err(|e| e.to_string())?;
        probe(&ffmpeg, &path).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn start_job(
    state: State<'_, JobState>,
    request: JobRequest,
    on_progress: Channel<ProgressEvent>,
) -> Result<JobResult, String> {
    let cancel = CancelToken::new();
    {
        let mut slot = state.0.lock().expect("job state lock");
        if slot.is_some() {
            return Err("すでに処理が実行中です".to_string());
        }
        *slot = Some(cancel.clone());
    }

    let result = tauri::async_runtime::spawn_blocking(move || {
        let spec = build_spec_and_save_defaults(&request)?;
        let sink = ChannelSink(on_progress);
        run_job(&spec, &sink, &cancel).map_err(|e| e.to_string())
    })
    .await;

    // 成功・失敗にかかわらず「実行中」状態を解除する
    state.0.lock().expect("job state lock").take();

    let report = result.map_err(|e| e.to_string())??;
    Ok(JobResult {
        outputs: report
            .outputs
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        source_duration_ms: report.source_duration_ms,
        output_duration_ms: report.output_duration_ms,
        timings: report
            .timings
            .iter()
            .map(|t| StageSeconds {
                stage_label: t.stage.label().to_string(),
                seconds: t.duration.as_secs_f64(),
            })
            .collect(),
        total_seconds: report.total_duration.as_secs_f64(),
        real_time_factor: report.real_time_factor(),
    })
}

/// リクエストから JobSpec を作り、あわせて設定を次回デフォルトとして保存する
fn build_spec_and_save_defaults(request: &JobRequest) -> Result<JobSpec, String> {
    let mut config = AppConfig::load().map_err(|e| e.to_string())?;
    config.default_bgm = request.bgm.clone();
    config.bgm.volume = request.bgm_volume;
    config.bgm.fade_in_s = request.fade_in_s;
    config.bgm.fade_out_s = request.fade_out_s;
    config.bgm.ending_tail_s = request.ending_tail_s;
    config.preset = request.preset.clone();
    config.model = request.model.clone();
    config.transcribe = request.transcribe;
    config.output_dir = Some(request.output_dir.clone());
    config.save().map_err(|e| e.to_string())?;

    let preset = Preset::by_name(&request.preset)
        .ok_or_else(|| format!("未知のプリセット: {}", request.preset))?;

    Ok(JobSpec {
        input: request.input.clone(),
        output_dir: request.output_dir.clone(),
        bgm: request.bgm.clone(),
        bgm_opts: config.bgm.clone(),
        preset,
        vad_params: VadParams::default(),
        target_lufs: config.target_lufs,
        transcribe: request.transcribe,
        model: request.model.clone(),
        outputs: config.outputs.clone(),
        mp3_bitrate_kbps: config.mp3_bitrate_kbps,
        ffmpeg_dir: config.ffmpeg_dir.clone(),
        timeline: None,
        trim_range_ms: match (request.trim_start_ms, request.trim_end_ms) {
            (None, None) => None,
            (start, end) => Some((start.unwrap_or(0), end.unwrap_or(u64::MAX))),
        },
    })
}

/// 設定画面からの保存リクエスト。
/// 出力ファイルの選択と EQ 分離は使用頻度が低いため、メイン画面ではなく
/// 設定画面で変更してその場で保存する
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdate {
    pub outputs: pae_core::config::OutputSelection,
    pub voice_duck_db: f32,
    pub mp3_bitrate_kbps: u32,
}

#[tauri::command]
pub fn save_settings(update: SettingsUpdate) -> Result<(), String> {
    let mut config = AppConfig::load().map_err(|e| e.to_string())?;
    config.outputs = update.outputs;
    config.bgm.voice_duck_db = update.voice_duck_db;
    config.mp3_bitrate_kbps = update.mp3_bitrate_kbps;
    config.save().map_err(|e| e.to_string())
}

/// BGM 音量プレビューのリクエスト。EQ 分離は設定に保存された値を使う
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BgmPreviewRequest {
    pub input: PathBuf,
    pub bgm: PathBuf,
    pub bgm_volume: f32,
}

/// 入力動画の一部と BGM を現在の設定でミックスした試聴用 MP3 を生成し、
/// バイト列で返す。フロントエンドは Blob にして <audio> で再生する
#[tauri::command]
pub async fn bgm_preview(request: BgmPreviewRequest) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let config = AppConfig::load().map_err(|e| e.to_string())?;
        let ffmpeg = Ffmpeg::locate(config.ffmpeg_dir.as_deref()).map_err(|e| e.to_string())?;
        let info = probe(&ffmpeg, &request.input).map_err(|e| e.to_string())?;

        // 冒頭は挨拶前の無音が多いので、会話が始まっていそうな
        // 全体の3割地点から15秒を試聴に使う
        let duration_ms = 15_000.min(info.duration_ms);
        let start_ms = ((info.duration_ms as f64 * 0.3) as u64)
            .min(info.duration_ms.saturating_sub(duration_ms));

        let opts = pae_core::media::process::BgmOpts {
            volume: request.bgm_volume,
            ..config.bgm.clone()
        };

        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let out = dir.path().join("preview.mp3");
        pae_core::media::process::render_bgm_preview(
            &ffmpeg,
            &request.input,
            &request.bgm,
            &opts,
            start_ms,
            duration_ms,
            &out,
            &CancelToken::new(),
        )
        .map_err(|e| e.to_string())?;

        let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_job(state: State<'_, JobState>) {
    if let Some(token) = state.0.lock().expect("job state lock").as_ref() {
        token.cancel();
    }
}

/// Finder でファイルを表示する (macOS)。他 OS は将来対応
#[tauri::command]
pub fn reveal_path(path: PathBuf) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("このOSではまだ対応していません".to_string())
    }
}
