//! 話者埋め込みをクラスタへ分ける純粋関数。I/O を持たないためテストしやすい

/// クラスタ重心からこれ以上離れた区間は、話者を決めきれなかったものとして扱う。
/// コサイン距離で表し、0 が同じ向き、1 が無関係、2 が真逆になる。
///
/// 話者認識用の音声で試したところ、同じ話者どうしは 0.33〜0.38、
/// 別の話者どうしは 0.64 以上に開いた。重心との距離は組どうしより縮むため、
/// その間に線を引いている
pub const DEFAULT_MAX_CENTER_DISTANCE: f32 = 0.55;

/// 埋め込みひとつ分の判定結果
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerAssignment {
    /// 決めきれなかった区間は None
    pub speaker: Option<usize>,
    /// 各クラスタ重心とのコサイン距離。並びは話者番号と同じ
    pub distances: Vec<f32>,
}

impl SpeakerAssignment {
    /// もっとも近い重心との距離
    pub fn best_distance(&self) -> f32 {
        self.distances.iter().copied().fold(f32::MAX, f32::min)
    }

    /// もっとも近い重心と、次に近い重心との差。
    /// 小さいほど話者を決めかねている。話者がひとりだけなら None
    pub fn margin(&self) -> Option<f32> {
        let mut sorted = self.distances.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        match sorted.as_slice() {
            [best, second, ..] => Some(second - best),
            _ => None,
        }
    }
}

/// 埋め込み列を指定した人数へ分ける。
///
/// 戻り値は入力と同じ並びで、クラスタ重心から遠すぎる区間は話者なしになる。
/// クラスタ番号は最初に現れた順へ振り直すため、
/// 入力を時間順に並べておけば「最初に喋った人が 0 番」になる
pub fn cluster_speakers(
    embeddings: &[Vec<f32>],
    speaker_count: usize,
    max_center_distance: f32,
) -> Vec<SpeakerAssignment> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let normalized: Vec<Vec<f32>> = embeddings.iter().map(|e| l2_normalize(e)).collect();
    let merged = agglomerate(&normalized, speaker_count.max(1));
    let labels = renumber_by_first_appearance(&merged);

    let centers = cluster_centers(&normalized, &labels);
    labels
        .iter()
        .enumerate()
        .map(|(i, &label)| {
            let distances: Vec<f32> = centers
                .iter()
                .map(|center| cosine_distance(&normalized[i], center))
                .collect();
            SpeakerAssignment {
                speaker: (distances[label] <= max_center_distance).then_some(label),
                distances,
            }
        })
        .collect()
}

/// 平均リンク法の凝集型クラスタリング。
/// クラスタ数が target になるまで、もっとも近いクラスタ同士をつなげていく
fn agglomerate(normalized: &[Vec<f32>], target: usize) -> Vec<usize> {
    let n = normalized.len();
    let mut members = vec![1usize; n];
    let mut alive = vec![true; n];
    let mut label: Vec<usize> = (0..n).collect();

    // クラスタ間の距離。最初は各点がひとつのクラスタなので点同士の距離と同じになる
    let mut distance = vec![vec![0.0f32; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = cosine_distance(&normalized[i], &normalized[j]);
            distance[i][j] = d;
            distance[j][i] = d;
        }
    }

    let mut cluster_count = n;
    while cluster_count > target {
        let Some((a, b)) = closest_pair(&distance, &alive) else {
            break;
        };

        // Lance-Williams の更新式。平均リンク法では要素数で重み付けした平均になる
        let (size_a, size_b) = (members[a] as f32, members[b] as f32);
        for c in 0..n {
            if !alive[c] || c == a || c == b {
                continue;
            }
            let d = (size_a * distance[a][c] + size_b * distance[b][c]) / (size_a + size_b);
            distance[a][c] = d;
            distance[c][a] = d;
        }

        members[a] += members[b];
        alive[b] = false;
        for l in label.iter_mut() {
            if *l == b {
                *l = a;
            }
        }
        cluster_count -= 1;
    }
    label
}

fn closest_pair(distance: &[Vec<f32>], alive: &[bool]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, f32)> = None;
    for i in 0..alive.len() {
        if !alive[i] {
            continue;
        }
        for j in (i + 1)..alive.len() {
            if !alive[j] {
                continue;
            }
            if best.is_none_or(|(_, _, d)| distance[i][j] < d) {
                best = Some((i, j, distance[i][j]));
            }
        }
    }
    best.map(|(i, j, _)| (i, j))
}

/// クラスタ番号を、最初に現れた順の 0, 1, 2 … へ振り直す
fn renumber_by_first_appearance(labels: &[usize]) -> Vec<usize> {
    let mut order: Vec<usize> = Vec::new();
    labels
        .iter()
        .map(|&l| match order.iter().position(|&o| o == l) {
            Some(pos) => pos,
            None => {
                order.push(l);
                order.len() - 1
            }
        })
        .collect()
}

fn cluster_centers(normalized: &[Vec<f32>], labels: &[usize]) -> Vec<Vec<f32>> {
    let dim = normalized.first().map_or(0, |e| e.len());
    let count = labels.iter().copied().max().map_or(0, |m| m + 1);
    let mut sums = vec![vec![0.0f32; dim]; count];
    for (embedding, &label) in normalized.iter().zip(labels) {
        for (sum, value) in sums[label].iter_mut().zip(embedding) {
            *sum += value;
        }
    }
    sums.iter().map(|s| l2_normalize(s)).collect()
}

fn l2_normalize(embedding: &[f32]) -> Vec<f32> {
    let norm = embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return embedding.to_vec();
    }
    embedding.iter().map(|v| v / norm).collect()
}

/// 正規化済みベクトル同士のコサイン距離
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    1.0 - dot
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 話者らしさを表す向きのベクトルを作る。base が同じなら同じ話者に近づく
    fn vector(base: f32, jitter: f32) -> Vec<f32> {
        vec![base + jitter, 1.0 - base, jitter * 0.5]
    }

    /// 話者番号だけを取り出す。距離まで見ないテストのためのヘルパ
    fn speakers(embeddings: &[Vec<f32>], count: usize, max_distance: f32) -> Vec<Option<usize>> {
        cluster_speakers(embeddings, count, max_distance)
            .iter()
            .map(|a| a.speaker)
            .collect()
    }

    #[test]
    fn splits_two_speakers() {
        let embeddings = vec![
            vector(1.0, 0.01),
            vector(0.0, 0.02),
            vector(1.0, -0.01),
            vector(0.0, -0.02),
        ];
        let result = speakers(&embeddings, 2, 1.0);
        assert_eq!(result[0], Some(0));
        assert_eq!(result[2], Some(0));
        assert_eq!(result[1], Some(1));
        assert_eq!(result[3], Some(1));
    }

    #[test]
    fn numbers_clusters_by_first_appearance() {
        let embeddings = vec![vector(0.0, 0.0), vector(1.0, 0.0), vector(0.0, 0.01)];
        let result = speakers(&embeddings, 2, 1.0);
        assert_eq!(result[0], Some(0));
        assert_eq!(result[1], Some(1));
        assert_eq!(result[2], Some(0));
    }

    #[test]
    fn everyone_is_the_same_speaker() {
        let embeddings = vec![vector(1.0, 0.0), vector(1.0, 0.01), vector(1.0, -0.01)];
        assert_eq!(
            speakers(&embeddings, 1, 1.0),
            vec![Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn speaker_count_larger_than_segments() {
        let embeddings = vec![vector(1.0, 0.0), vector(0.0, 0.0)];
        assert_eq!(speakers(&embeddings, 6, 1.0), vec![Some(0), Some(1)]);
    }

    #[test]
    fn empty_input() {
        assert!(cluster_speakers(&[], 2, 1.0).is_empty());
    }

    /// 重心から遠く離れた区間は話者を決めきれなかったものとして None になる
    #[test]
    fn far_from_center_becomes_unknown() {
        let embeddings = vec![
            vec![1.0, 0.0, 0.0],
            vec![1.0, 0.0, 0.0],
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
        ];
        // 最後のひとつだけが直交しており、コサイン距離が大きく開く
        let result = cluster_speakers(&embeddings, 1, 0.5);
        assert_eq!(result[0].speaker, Some(0));
        assert_eq!(result[3].speaker, None);
        assert!(
            result[3].best_distance() > result[0].best_distance(),
            "外れた区間のほうが重心から遠いはず"
        );
        assert_eq!(result[0].margin(), None, "話者がひとりなら差は取れない");
    }

    /// どちらの話者にも近い区間は margin が小さくなる。
    /// 発話が混ざった区間を見つける手がかりになる
    #[test]
    fn margin_shrinks_for_ambiguous_segments() {
        let embeddings = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            // ふたつの話者のちょうど中間を向いたベクトル
            vec![1.0, 1.0, 0.0],
        ];
        let result = cluster_speakers(&embeddings, 2, 1.0);
        let ambiguous = result[2].margin().unwrap();
        let clear = result[0].margin().unwrap();
        assert!(
            ambiguous < clear,
            "中間を向いた区間のほうが迷っているはず: {ambiguous} < {clear}"
        );
    }
}
