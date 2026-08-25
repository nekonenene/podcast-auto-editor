use std::path::Path;

use crate::error::{PaeError, Result};
use crate::types::MediaInfo;

use super::ffmpeg::Ffmpeg;

/// ffprobe で入力メディアの情報を取得する
pub fn probe(ffmpeg: &Ffmpeg, input: &Path) -> Result<MediaInfo> {
    if !input.exists() {
        return Err(PaeError::InputNotFound(input.to_path_buf()));
    }
    let json = ffmpeg.probe([
        "-v".as_ref(),
        "error".as_ref(),
        "-print_format".as_ref(),
        "json".as_ref(),
        "-show_format".as_ref(),
        "-show_streams".as_ref(),
        input.as_os_str(),
    ])?;
    parse_probe_output(&json, input)
}

fn parse_probe_output(json: &str, input: &Path) -> Result<MediaInfo> {
    let value: serde_json::Value = serde_json::from_str(json)?;

    let duration_s: f64 = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| PaeError::ProbeParse("duration が取得できません".into()))?;

    let mut info = MediaInfo {
        path: input.to_path_buf(),
        duration_ms: (duration_s * 1000.0).round() as u64,
        has_video: false,
        video_codec: None,
        width: None,
        height: None,
        fps: None,
        audio_codec: None,
        sample_rate: None,
        channels: None,
    };

    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| PaeError::ProbeParse("streams が取得できません".into()))?;

    for stream in streams {
        match stream["codec_type"].as_str() {
            Some("video") if !info.has_video => {
                info.has_video = true;
                info.video_codec = stream["codec_name"].as_str().map(String::from);
                info.width = stream["width"].as_u64().map(|v| v as u32);
                info.height = stream["height"].as_u64().map(|v| v as u32);
                info.fps = stream["r_frame_rate"].as_str().and_then(parse_frame_rate);
            }
            Some("audio") if info.audio_codec.is_none() => {
                info.audio_codec = stream["codec_name"].as_str().map(String::from);
                info.sample_rate = stream["sample_rate"].as_str().and_then(|s| s.parse().ok());
                info.channels = stream["channels"].as_u64().map(|v| v as u32);
            }
            _ => {}
        }
    }

    if info.audio_codec.is_none() {
        return Err(PaeError::ProbeParse(
            "音声ストリームが見つかりません".into(),
        ));
    }
    Ok(info)
}

/// 映像・音声それぞれのストリームの長さ (ミリ秒)。
/// 取得できなかったストリームは None になる
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamDurations {
    pub video_ms: Option<u64>,
    pub audio_ms: Option<u64>,
}

/// 出力ファイルのストリームごとの長さを取得する。
/// format 全体の duration ではなくストリーム単位で見るのは、
/// 映像は正しいのに音声だけ短い、という壊れ方を捉えるため
pub fn probe_stream_durations(ffmpeg: &Ffmpeg, path: &Path) -> Result<StreamDurations> {
    let json = ffmpeg.probe([
        "-v".as_ref(),
        "error".as_ref(),
        "-print_format".as_ref(),
        "json".as_ref(),
        "-show_streams".as_ref(),
        path.as_os_str(),
    ])?;
    parse_stream_durations(&json)
}

fn parse_stream_durations(json: &str) -> Result<StreamDurations> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| PaeError::ProbeParse("streams が取得できません".into()))?;

    let mut durations = StreamDurations::default();
    for stream in streams {
        let duration_ms = stream["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|s| (s * 1000.0).round() as u64);
        match stream["codec_type"].as_str() {
            Some("video") if durations.video_ms.is_none() => durations.video_ms = duration_ms,
            Some("audio") if durations.audio_ms.is_none() => durations.audio_ms = duration_ms,
            _ => {}
        }
    }
    Ok(durations)
}

/// "30/1" のような分数表記のフレームレートを f64 に変換する
fn parse_frame_rate(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 {
        return None;
    }
    Some(num / den)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_meet_recording() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1280,
                 "height": 720, "r_frame_rate": "30/1"},
                {"codec_type": "audio", "codec_name": "aac",
                 "sample_rate": "48000", "channels": 2}
            ],
            "format": {"duration": "111.333333"}
        }"#;
        let info = parse_probe_output(json, Path::new("in.mp4")).unwrap();
        assert_eq!(info.duration_ms, 111_333);
        assert!(info.has_video);
        assert_eq!(info.fps, Some(30.0));
        assert_eq!(info.sample_rate, Some(48000));
    }

    #[test]
    fn reads_each_stream_duration() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "duration": "4332.750000"},
                {"codec_type": "audio", "duration": "1449.400000"}
            ]
        }"#;
        let durations = parse_stream_durations(json).unwrap();
        assert_eq!(durations.video_ms, Some(4_332_750));
        assert_eq!(durations.audio_ms, Some(1_449_400));
    }

    /// duration を持たないストリームがあっても失敗しないこと
    #[test]
    fn tolerates_missing_stream_duration() {
        let json = r#"{"streams": [{"codec_type": "audio"}]}"#;
        let durations = parse_stream_durations(json).unwrap();
        assert_eq!(durations, StreamDurations::default());
    }

    #[test]
    fn rejects_video_without_audio() {
        let json = r#"{
            "streams": [{"codec_type": "video", "codec_name": "h264"}],
            "format": {"duration": "10.0"}
        }"#;
        assert!(parse_probe_output(json, Path::new("in.mp4")).is_err());
    }
}
