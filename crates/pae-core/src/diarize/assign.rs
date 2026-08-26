//! 発話区間ごとに求めた話者を、文字起こしのセグメントへ写す純粋関数

use crate::types::TranscriptSegment;

/// もっとも長く重なった話者がこの割合に届かなければ、話者を決めきれなかったものとして扱う。
/// whisper のセグメントは話者の交代をまたぐことがあり、
/// 拮抗しているときに片方へ寄せると誤った発言者が付いてしまうため
const MAJORITY_PERCENT: u64 = 60;

/// 文字起こしセグメントごとの話者を決める。
///
/// ranges と speakers は同じ並びで、出力側の時間軸のミリ秒で表す。
/// 重なっている時間がもっとも長い話者を採用し、
/// 話者不明の区間としか重ならない場合や、拮抗している場合は None を返す
pub fn assign_speakers(
    segments: &[TranscriptSegment],
    ranges: &[(u64, u64)],
    speakers: &[Option<usize>],
) -> Vec<Option<usize>> {
    segments
        .iter()
        .map(|segment| {
            let mut overlaps: Vec<u64> = Vec::new();
            for (range, speaker) in ranges.iter().zip(speakers) {
                // 話者不明の区間は、どの話者の根拠にもならないため数えない
                let Some(speaker) = *speaker else { continue };
                let ms = overlap_ms((segment.start_ms, segment.end_ms), *range);
                if ms == 0 {
                    continue;
                }
                if overlaps.len() <= speaker {
                    overlaps.resize(speaker + 1, 0);
                }
                overlaps[speaker] += ms;
            }

            let total: u64 = overlaps.iter().sum();
            if total == 0 {
                return None;
            }
            let (best, best_ms) = overlaps
                .iter()
                .enumerate()
                .max_by_key(|(_, &ms)| ms)
                .map(|(i, &ms)| (i, ms))?;
            if best_ms * 100 < total * MAJORITY_PERCENT {
                None
            } else {
                Some(best)
            }
        })
        .collect()
}

fn overlap_ms(a: (u64, u64), b: (u64, u64)) -> u64 {
    let start = a.0.max(b.0);
    let end = a.1.min(b.1);
    end.saturating_sub(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start_ms: u64, end_ms: u64) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms,
            text: "テスト".into(),
            speaker: None,
        }
    }

    #[test]
    fn picks_the_longest_overlap() {
        let segments = vec![segment(0, 1000)];
        let ranges = vec![(0, 800), (800, 1000)];
        let speakers = vec![Some(0), Some(1)];
        assert_eq!(
            assign_speakers(&segments, &ranges, &speakers),
            vec![Some(0)]
        );
    }

    #[test]
    fn no_overlap_means_unknown() {
        let segments = vec![segment(2000, 3000)];
        let ranges = vec![(0, 1000)];
        let speakers = vec![Some(0)];
        assert_eq!(assign_speakers(&segments, &ranges, &speakers), vec![None]);
    }

    /// 話者不明の区間としか重ならないセグメントも話者不明になる
    #[test]
    fn only_unknown_ranges_means_unknown() {
        let segments = vec![segment(0, 1000)];
        let ranges = vec![(0, 1000)];
        let speakers = vec![None];
        assert_eq!(assign_speakers(&segments, &ranges, &speakers), vec![None]);
    }

    /// 話者不明の区間は割合の分母にも入れない。
    /// 決め手が無いだけで、他の話者を否定する根拠ではないため
    #[test]
    fn unknown_ranges_do_not_dilute_the_majority() {
        let segments = vec![segment(0, 1000)];
        let ranges = vec![(0, 500), (500, 1000)];
        let speakers = vec![Some(0), None];
        assert_eq!(
            assign_speakers(&segments, &ranges, &speakers),
            vec![Some(0)]
        );
    }

    #[test]
    fn majority_threshold_boundary() {
        let segments = vec![segment(0, 1000)];
        // ちょうど 60% なら採用する
        let ranges = vec![(0, 600), (600, 1000)];
        assert_eq!(
            assign_speakers(&segments, &ranges, &[Some(0), Some(1)]),
            vec![Some(0)]
        );

        // 59% では決め手に欠けるとみなす
        let ranges = vec![(0, 590), (590, 1000)];
        assert_eq!(
            assign_speakers(&segments, &ranges, &[Some(0), Some(1)]),
            vec![None]
        );
    }

    #[test]
    fn handles_multiple_segments() {
        let segments = vec![segment(0, 1000), segment(1000, 2000)];
        let ranges = vec![(0, 1000), (1000, 2000)];
        let speakers = vec![Some(0), Some(1)];
        assert_eq!(
            assign_speakers(&segments, &ranges, &speakers),
            vec![Some(0), Some(1)]
        );
    }
}
