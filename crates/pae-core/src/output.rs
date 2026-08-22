//! 文字起こし結果を各フォーマット (TXT / JSON / SRT / Markdown) に整形する純粋関数群

use serde::Serialize;

use crate::types::TranscriptSegment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    Txt,
    Json,
    Srt,
    Markdown,
}

impl TranscriptFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            TranscriptFormat::Txt => "txt",
            TranscriptFormat::Json => "json",
            TranscriptFormat::Srt => "srt",
            TranscriptFormat::Markdown => "md",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "txt" => Some(Self::Txt),
            "json" => Some(Self::Json),
            "srt" => Some(Self::Srt),
            "md" | "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }
}

pub fn render(segments: &[TranscriptSegment], format: TranscriptFormat) -> String {
    match format {
        TranscriptFormat::Txt => render_txt(segments),
        TranscriptFormat::Json => render_json(segments),
        TranscriptFormat::Srt => render_srt(segments),
        TranscriptFormat::Markdown => render_markdown(segments),
    }
}

fn render_txt(segments: &[TranscriptSegment]) -> String {
    let mut out = String::new();
    for seg in segments {
        out.push_str(&seg.text);
        out.push('\n');
    }
    out
}

/// JSON はタイムスタンプを秒 (小数) で出力する。外部ツールから扱いやすいため
fn render_json(segments: &[TranscriptSegment]) -> String {
    #[derive(Serialize)]
    struct JsonSegment<'a> {
        start: f64,
        end: f64,
        text: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        speaker: &'a Option<String>,
    }
    let items: Vec<JsonSegment> = segments
        .iter()
        .map(|s| JsonSegment {
            start: s.start_ms as f64 / 1000.0,
            end: s.end_ms as f64 / 1000.0,
            text: &s.text,
            speaker: &s.speaker,
        })
        .collect();
    serde_json::to_string_pretty(&items).expect("serialize は失敗しない")
}

fn render_srt(segments: &[TranscriptSegment]) -> String {
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_timestamp(seg.start_ms),
            srt_timestamp(seg.end_ms),
            seg.text
        ));
    }
    out
}

fn srt_timestamp(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{millis:03}")
}

fn render_markdown(segments: &[TranscriptSegment]) -> String {
    let mut out = String::from("# 文字起こし\n\n");
    for seg in segments {
        out.push_str(&format!(
            "**[{}]** {}\n\n",
            display_timestamp(seg.start_ms),
            seg.text
        ));
    }
    out
}

fn display_timestamp(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments() -> Vec<TranscriptSegment> {
        vec![
            TranscriptSegment {
                start_ms: 1240,
                end_ms: 4710,
                text: "今日はAIについて話していきたいと思います".into(),
                speaker: None,
            },
            TranscriptSegment {
                start_ms: 3_661_500,
                end_ms: 3_665_000,
                text: "ありがとうございました".into(),
                speaker: None,
            },
        ]
    }

    #[test]
    fn srt_format() {
        let srt = render(&segments(), TranscriptFormat::Srt);
        insta::assert_snapshot!(srt, @r"
        1
        00:00:01,240 --> 00:00:04,710
        今日はAIについて話していきたいと思います

        2
        01:01:01,500 --> 01:01:05,000
        ありがとうございました
        ");
    }

    #[test]
    fn json_uses_seconds() {
        let json = render(&segments(), TranscriptFormat::Json);
        assert!(json.contains("\"start\": 1.24"));
        assert!(json.contains("\"end\": 4.71"));
    }

    #[test]
    fn markdown_has_readable_timestamps() {
        let md = render(&segments(), TranscriptFormat::Markdown);
        assert!(md.contains("**[0:01]**"));
        assert!(md.contains("**[1:01:01]**"));
    }
}
