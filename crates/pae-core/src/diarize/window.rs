//! 発話区間を、埋め込みを取る窓へ切り分ける純粋関数。
//!
//! 発話区間をそのまま1単位にすると、話者の交代をまたいだ区間から
//! ふたりの声が混ざったベクトルができてしまう。
//! 混ざったベクトルはその区間を外すだけでなくクラスタの重心も濁らせるため、
//! 短い窓へ切り分けてから埋め込みを取る。
//!
//! 窓の長さは、話者を学ぶときと判定するときで変える。
//! クラスタを作るには長い窓が要るが、長い窓では
//! 相槌のような短い発言が多数派になれず、周りの話者へ飲まれてしまうため

/// 話者を学ぶときの窓の長さ。
/// WeSpeaker の埋め込みは1秒を下回ると不安定になり、
/// 長くすると話者の交代をまたぐ確率が上がるため、その間を取っている
pub const LEARN_WINDOW_MS: u64 = 1_500;

/// 学習用の窓をずらす幅。窓の半分だけ重ねる。
/// 重なりをやめると 5 分の音声で 4 秒しか速くならないのに、
/// 重心が粗くなって話者不明が 186 個から 235 個へ増えた
pub const LEARN_HOP_MS: u64 = 750;

/// 話者を判定するときの窓の長さ。
/// 長い発言に挟まった短い発言を拾うため、学習用よりずっと短くする。
/// できあがった重心と比べるだけなので、この長さでも判定はできる
pub const ASSIGN_WINDOW_MS: u64 = 500;

/// 判定用の窓をずらす幅
pub const ASSIGN_HOP_MS: u64 = 250;

/// ひとつの発話区間を窓へ切り分ける。
/// 窓より短い区間はそのまま1個の窓として返す
pub fn split_into_windows(
    start_ms: u64,
    end_ms: u64,
    window_ms: u64,
    hop_ms: u64,
) -> Vec<(u64, u64)> {
    if end_ms.saturating_sub(start_ms) <= window_ms {
        return vec![(start_ms, end_ms)];
    }
    let mut windows = Vec::new();
    let mut cursor = start_ms;
    while cursor + window_ms < end_ms {
        windows.push((cursor, cursor + window_ms));
        cursor += hop_ms.max(1);
    }
    // 最後の窓は区間の終わりにそろえる。末尾を取りこぼさないため
    windows.push((end_ms.saturating_sub(window_ms).max(start_ms), end_ms));
    windows
}

/// 発話区間の一覧を、話者を学ぶための窓へ切り分ける
pub fn learn_windows(ranges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    split_all(ranges, LEARN_WINDOW_MS, LEARN_HOP_MS)
}

/// 発話区間の一覧を、話者を判定するための窓へ切り分ける
pub fn assign_windows(ranges: &[(u64, u64)]) -> Vec<(u64, u64)> {
    split_all(ranges, ASSIGN_WINDOW_MS, ASSIGN_HOP_MS)
}

fn split_all(ranges: &[(u64, u64)], window_ms: u64, hop_ms: u64) -> Vec<(u64, u64)> {
    ranges
        .iter()
        .flat_map(|&(start, end)| split_into_windows(start, end, window_ms, hop_ms))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_range_stays_whole() {
        assert_eq!(split_into_windows(0, 400, 1_500, 750), vec![(0, 400)]);
        assert_eq!(
            split_into_windows(1_000, 2_500, 1_500, 750),
            vec![(1_000, 2_500)]
        );
    }

    #[test]
    fn long_range_is_split_with_overlap() {
        assert_eq!(
            split_into_windows(0, 3_000, 1_500, 750),
            vec![(0, 1_500), (750, 2_250), (1_500, 3_000)]
        );
    }

    /// 窓をちょうど割り切れない長さでも、末尾まで必ず覆う
    #[test]
    fn last_window_reaches_the_end() {
        assert_eq!(
            split_into_windows(0, 2_000, 1_500, 750),
            vec![(0, 1_500), (500, 2_000)]
        );

        let windows = split_into_windows(0, 4_100, 1_500, 750);
        assert_eq!(windows.last(), Some(&(2_600, 4_100)));
        assert!(windows.iter().all(|(s, e)| e - s == 1_500));
    }

    #[test]
    fn windows_stay_inside_the_range() {
        for (start, end) in [(0, 10_000), (3_333, 7_777), (0, 1_501)] {
            for (s, e) in split_into_windows(start, end, 1_500, 750) {
                assert!(s >= start && e <= end, "窓が区間からはみ出しています");
                assert!(s < e);
            }
        }
    }

    /// 判定用の窓は学習用より細かく、短い発言も単独の窓になる
    #[test]
    fn assign_windows_are_finer_than_learn_windows() {
        let ranges = [(0, 5_000)];
        let learn = learn_windows(&ranges);
        let assign = assign_windows(&ranges);
        assert!(
            assign.len() > learn.len() * 2,
            "判定用のほうが窓が多いはず: {} vs {}",
            assign.len(),
            learn.len()
        );
        assert!(assign.iter().all(|(s, e)| e - s <= ASSIGN_WINDOW_MS));
    }

    /// 長い発言に挟まった 600ms の発言が、単独の窓に収まる
    #[test]
    fn short_interjection_gets_its_own_window() {
        let windows = assign_windows(&[(0, 10_000)]);
        let interjection = (4_150u64, 4_750u64);
        let inside = windows.iter().filter(|(s, e)| {
            *s >= interjection.0.saturating_sub(100) && *e <= interjection.1 + 100
        });
        assert!(inside.count() > 0, "割り込みの内側に収まる窓がひとつも無い");
    }
}
