//! 文字起こし結果の後始末。モデルが勝手に書き足した話者名ラベルを取り除く

use std::collections::{HashMap, HashSet};

use crate::types::TranscriptSegment;

/// 同じラベルがこの回数以上出てきたら、書き足されたものとみなす。
/// 単発のものは実際の発言かもしれないので残す
const MIN_REPEAT: usize = 5;

/// ラベルとして扱う接頭辞の最大文字数。人名にしては長すぎるものは対象外にする
const MAX_LABEL_CHARS: usize = 10;

/// 名前の途中には現れない文字。これらを含む接頭辞はラベルではなく本文と判断する
const NOT_IN_NAME: &str = "、。，．,.!?！？「」『』()（）";

/// 行頭の「話者名:」を取り除く。
///
/// whisper は字幕を大量に学習しているため、相槌のような短い区間で、
/// 実際には存在しない話者名を書き足してしまうことがある。
/// whisper.cpp は前の窓の出力を次の窓のお手本として渡すので、
/// 一度出たラベルはそのまま増殖していく。
/// そこで、同じラベルが何度も繰り返されている場合だけ書き足しとみなして落とす
pub fn strip_hallucinated_speaker_labels(
    segments: Vec<TranscriptSegment>,
) -> Vec<TranscriptSegment> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for segment in &segments {
        if let Some((label, _)) = split_label(&segment.text) {
            *counts.entry(label).or_insert(0) += 1;
        }
    }
    let repeated: HashSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count >= MIN_REPEAT)
        .map(|(label, _)| label.to_string())
        .collect();
    if repeated.is_empty() {
        return segments;
    }

    segments
        .into_iter()
        .filter_map(|mut segment| {
            let stripped = split_label(&segment.text).and_then(|(label, rest)| {
                repeated
                    .contains(label)
                    .then(|| rest.trim_start().trim().to_string())
            });
            match stripped {
                // ラベルだけで中身がない区間は、残しても読めないので落とす
                Some(rest) if rest.is_empty() => None,
                Some(rest) => {
                    segment.text = rest;
                    Some(segment)
                }
                None => Some(segment),
            }
        })
        .collect()
}

/// 「ラベル + コロン + 本文」に分解する。ラベルらしくない場合は None を返す
fn split_label(text: &str) -> Option<(&str, &str)> {
    let colon = text.find([':', '：'])?;
    let (label, rest) = text.split_at(colon);
    let rest = &rest[text[colon..].chars().next()?.len_utf8()..];

    // 「http://」のような URL を名前と取り違えない
    if rest.starts_with("//") {
        return None;
    }
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return None;
    }
    // 「10:30」のような時刻を守る
    if label.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if label.contains(|c: char| c.is_whitespace() || NOT_IN_NAME.contains(c)) {
        return None;
    }
    Some((label, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start_ms: 0,
            end_ms: 1000,
            text: text.to_string(),
            speaker: None,
        }
    }

    fn texts(segments: Vec<TranscriptSegment>) -> Vec<String> {
        segments.into_iter().map(|s| s.text).collect()
    }

    fn repeat(label: &str, times: usize) -> Vec<TranscriptSegment> {
        (0..times)
            .map(|i| seg(&format!("{label}:はい{i}")))
            .collect()
    }

    #[test]
    fn label_repeated_exactly_5_times_is_stripped() {
        let got = texts(strip_hallucinated_speaker_labels(repeat("ヤンヤン", 5)));
        assert_eq!(got, vec!["はい0", "はい1", "はい2", "はい3", "はい4"]);
    }

    #[test]
    fn label_repeated_only_4_times_is_kept() {
        let got = texts(strip_hallucinated_speaker_labels(repeat("ヤンヤン", 4)));
        assert_eq!(got[0], "ヤンヤン:はい0");
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn spaces_after_colon_are_trimmed() {
        let mut segments = repeat("ヤンヤン", 5);
        segments.push(seg("ヤンヤン:  そうだね"));
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got.last().unwrap(), "そうだね");
    }

    #[test]
    fn fullwidth_colon_is_handled() {
        let segments: Vec<_> = (0..5).map(|_| seg("ヤンヤン：うん")).collect();
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got, vec!["うん"; 5]);
    }

    #[test]
    fn segment_with_only_label_is_dropped() {
        let mut segments = repeat("ヤンヤン", 5);
        segments.push(seg("ヤンヤン:"));
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn clock_time_is_not_a_label() {
        let segments: Vec<_> = (0..8).map(|_| seg("10:30に集合ね")).collect();
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got[0], "10:30に集合ね");
    }

    #[test]
    fn url_scheme_is_not_a_label() {
        let segments: Vec<_> = (0..8).map(|_| seg("https://example.com だよ")).collect();
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got[0], "https://example.com だよ");
    }

    #[test]
    fn too_long_prefix_is_not_a_label() {
        let long = "あいうえおかきくけこさ";
        let segments: Vec<_> = (0..8).map(|_| seg(&format!("{long}:はい"))).collect();
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got[0], format!("{long}:はい"));
    }

    #[test]
    fn prefix_with_punctuation_is_not_a_label() {
        let segments: Vec<_> = (0..8).map(|_| seg("えっと、それで:はい")).collect();
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got[0], "えっと、それで:はい");
    }

    #[test]
    fn each_label_is_counted_separately() {
        let mut segments = repeat("ヤンヤン", 5);
        segments.push(seg("のび:こんにちは"));
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got.last().unwrap(), "のび:こんにちは");
    }

    #[test]
    fn text_without_label_is_untouched() {
        let segments = vec![seg("こんにちは"), seg("そうだね")];
        let got = texts(strip_hallucinated_speaker_labels(segments));
        assert_eq!(got, vec!["こんにちは", "そうだね"]);
    }
}
