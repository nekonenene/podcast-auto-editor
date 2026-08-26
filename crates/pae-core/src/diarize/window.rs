//! 発話区間を、埋め込みを取る窓へ切り分ける純粋関数。
//!
//! 発話区間をそのまま1単位にすると、話者の交代をまたいだ区間から
//! ふたりの声が混ざったベクトルができてしまう。
//! 混ざったベクトルはその区間を外すだけでなくクラスタの重心も濁らせるため、
//! 短い窓へ切り分けてから埋め込みを取る

/// 埋め込みを取る窓の長さ。
/// WeSpeaker の埋め込みは1秒を下回ると不安定になり、
/// 長くすると話者の交代をまたぐ確率が上がるため、その間を取っている
pub const WINDOW_MS: u64 = 1_500;

/// 窓をずらす幅。窓の半分だけ重ねて、交代の位置を拾いやすくする
pub const HOP_MS: u64 = 750;

/// ひとつの発話区間を窓へ切り分ける。
/// 窓より短い区間はそのまま1個の窓として返す
pub fn split_into_windows(start_ms: u64, end_ms: u64) -> Vec<(u64, u64)> {
    if end_ms.saturating_sub(start_ms) <= WINDOW_MS {
        return vec![(start_ms, end_ms)];
    }
    let mut windows = Vec::new();
    let mut cursor = start_ms;
    while cursor + WINDOW_MS < end_ms {
        windows.push((cursor, cursor + WINDOW_MS));
        cursor += HOP_MS;
    }
    // 最後の窓は区間の終わりにそろえる。末尾を取りこぼさないため
    windows.push((end_ms.saturating_sub(WINDOW_MS).max(start_ms), end_ms));
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_range_stays_whole() {
        assert_eq!(split_into_windows(0, 400), vec![(0, 400)]);
        assert_eq!(split_into_windows(1_000, 2_500), vec![(1_000, 2_500)]);
    }

    #[test]
    fn long_range_is_split_with_overlap() {
        assert_eq!(
            split_into_windows(0, 3_000),
            vec![(0, 1_500), (750, 2_250), (1_500, 3_000)]
        );
    }

    /// 窓をちょうど割り切れない長さでも、末尾まで必ず覆う
    #[test]
    fn last_window_reaches_the_end() {
        let windows = split_into_windows(0, 2_000);
        assert_eq!(windows, vec![(0, 1_500), (500, 2_000)]);

        let windows = split_into_windows(0, 4_100);
        assert_eq!(windows.last(), Some(&(2_600, 4_100)));
        assert!(windows.iter().all(|(s, e)| e - s == WINDOW_MS));
    }

    /// 窓の長さちょうどの区間は分割しない
    #[test]
    fn exactly_one_window() {
        assert_eq!(
            split_into_windows(500, 500 + WINDOW_MS),
            vec![(500, 500 + WINDOW_MS)]
        );
    }

    #[test]
    fn windows_stay_inside_the_range() {
        for (start, end) in [(0, 10_000), (3_333, 7_777), (0, 1_501)] {
            for (s, e) in split_into_windows(start, end) {
                assert!(s >= start && e <= end, "窓が区間からはみ出しています");
                assert!(s < e);
            }
        }
    }
}
