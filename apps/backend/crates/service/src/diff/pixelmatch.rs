//! pixelmatch の Rust 移植（mapbox/pixelmatch, MIT ライセンス）。
//!
//! アルゴリズムは本家 JS 実装（v5 系）をそのまま追う:
//!
//! 1. 各ピクセルで YIQ 色空間の二乗距離 [`color_delta`] を求める
//! 2. `35215 * threshold^2` を超えたら「差分候補」
//! 3. `include_aa == false` のときは [`antialiased`]（Vysniauskas 2009 の
//!    "Anti-aliased Pixel and Intensity Slope Detector"）でアンチエイリアスを判定し、
//!    AA と判定されたピクセルは黄色で描画するが**差分としては数えない**
//! 4. それ以外の差分は赤で描画して計数、非差分は baseline を淡いグレースケールで描画
//!
//! ## 本家からの意図的な差異
//!
//! - **サイズ不一致**: 本家は同一サイズを前提に例外を投げる。ここでは両画像の
//!   最大寸法まで走査し、片方だけ範囲外のピクセルを無条件で差分として数える
//!   （パディング相当）。両方とも範囲外のピクセルは差分にしない。
//! - **`diffColorAlt`**: 明暗方向で色を分ける機能は使わず、差分は常に赤。
//!   ただし [`color_delta`] の符号（本家の「img2 の方が暗いと負」）は
//!   AA 判定が輝度差の符号を使うため忠実に維持している。

use image::{Rgba, RgbaImage};

/// YIQ 差分メトリクスが取りうる最大値（本家の定数）。
const MAX_YIQ_DELTA: f64 = 35215.0;
/// 非差分ピクセルを描画するときの明度（本家 `alpha` オプションの既定値）。
const BACKGROUND_ALPHA: f64 = 0.1;

const DIFF_COLOR: Rgba<u8> = Rgba([255, 0, 0, 255]);
const AA_COLOR: Rgba<u8> = Rgba([255, 255, 0, 255]);

/// 差分計算のオプション。
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// 1 ピクセルを差分とみなす YIQ 色距離のしきい値（0.0〜1.0）。
    /// `projects.diff_threshold` をそのまま渡す。
    pub threshold: f64,
    /// `true` にするとアンチエイリアス検出を行わず、AA も差分として数える。
    /// 既定は `false`（= AA 検出あり、AA は数えない）。
    pub include_aa: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            threshold: 0.1,
            include_aa: false,
        }
    }
}

impl DiffOptions {
    pub fn with_threshold(threshold: f64) -> Self {
        Self {
            threshold,
            ..Self::default()
        }
    }
}

/// 差分計算の結果。
pub struct DiffResult {
    /// 差分と判定されたピクセル数（AA と判定されたものは含まない）。
    pub diff_pixel_count: u64,
    /// 走査した総ピクセル数（両画像の最大寸法の面積）。
    pub total_pixels: u64,
    /// `diff_pixel_count / total_pixels`（総ピクセル 0 なら 0.0）。
    pub diff_ratio: f64,
    /// 差分可視化画像。
    pub diff_image: RgbaImage,
}

impl std::fmt::Debug for DiffResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffResult")
            .field("diff_pixel_count", &self.diff_pixel_count)
            .field("total_pixels", &self.total_pixels)
            .field("diff_ratio", &self.diff_ratio)
            .field("diff_image", &self.diff_image.dimensions())
            .finish()
    }
}

/// 走査領域（両画像の最大寸法）に合わせて範囲外を透明として扱うアクセサ。
#[derive(Clone, Copy)]
struct PaddedImage<'a> {
    image: &'a RgbaImage,
}

impl<'a> PaddedImage<'a> {
    fn new(image: &'a RgbaImage) -> Self {
        Self { image }
    }

    fn contains(&self, x: u32, y: u32) -> bool {
        let (w, h) = self.image.dimensions();
        x < w && y < h
    }

    /// 範囲外は透明黒 `[0, 0, 0, 0]`。
    fn rgba(&self, x: u32, y: u32) -> [f64; 4] {
        if !self.contains(x, y) {
            return [0.0; 4];
        }
        let px = self.image.get_pixel(x, y).0;
        [
            f64::from(px[0]),
            f64::from(px[1]),
            f64::from(px[2]),
            f64::from(px[3]),
        ]
    }
}

fn rgb2y(r: f64, g: f64, b: f64) -> f64 {
    r * 0.298_895_31 + g * 0.586_622_47 + b * 0.114_482_23
}

fn rgb2i(r: f64, g: f64, b: f64) -> f64 {
    r * 0.595_977_99 - g * 0.274_176_10 - b * 0.321_801_89
}

fn rgb2q(r: f64, g: f64, b: f64) -> f64 {
    r * 0.211_470_17 - g * 0.522_617_11 + b * 0.311_146_94
}

/// 半透明色を白と合成する（本家 `blend`）。
fn blend(c: f64, a: f64) -> f64 {
    255.0 + (c - 255.0) * a
}

/// 2 ピクセル間の YIQ 二乗距離。`y_only` のときは輝度差そのもの（符号つき）。
///
/// 符号は本家と同じく「img2 の方が明るいと負」。AA 判定がこの符号を使う。
fn color_delta(
    img1: &PaddedImage<'_>,
    img2: &PaddedImage<'_>,
    p1: (u32, u32),
    p2: (u32, u32),
    y_only: bool,
) -> f64 {
    let [mut r1, mut g1, mut b1, mut a1] = img1.rgba(p1.0, p1.1);
    let [mut r2, mut g2, mut b2, mut a2] = img2.rgba(p2.0, p2.1);

    if a1 == a2 && r1 == r2 && g1 == g2 && b1 == b2 {
        return 0.0;
    }

    if a1 < 255.0 {
        a1 /= 255.0;
        r1 = blend(r1, a1);
        g1 = blend(g1, a1);
        b1 = blend(b1, a1);
    }

    if a2 < 255.0 {
        a2 /= 255.0;
        r2 = blend(r2, a2);
        g2 = blend(g2, a2);
        b2 = blend(b2, a2);
    }

    let y1 = rgb2y(r1, g1, b1);
    let y2 = rgb2y(r2, g2, b2);
    let y = y1 - y2;

    if y_only {
        return y;
    }

    let i = rgb2i(r1, g1, b1) - rgb2i(r2, g2, b2);
    let q = rgb2q(r1, g1, b1) - rgb2q(r2, g2, b2);

    let delta = 0.5053 * y * y + 0.299 * i * i + 0.1957 * q * q;

    if y1 > y2 { -delta } else { delta }
}

/// 同色の隣接ピクセルが 3 つ以上あるか（本家 `hasManySiblings`）。
fn has_many_siblings(img: &PaddedImage<'_>, x1: u32, y1: u32, width: u32, height: u32) -> bool {
    let x0 = x1.saturating_sub(1);
    let y0 = y1.saturating_sub(1);
    let x2 = (x1 + 1).min(width - 1);
    let y2 = (y1 + 1).min(height - 1);

    let center = img.rgba(x1, y1);
    let mut zeroes = u32::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);

    for x in x0..=x2 {
        for y in y0..=y2 {
            if x == x1 && y == y1 {
                continue;
            }
            if img.rgba(x, y) == center {
                zeroes += 1;
            }
            if zeroes > 2 {
                return true;
            }
        }
    }

    false
}

/// アンチエイリアスに由来するピクセルか（本家 `antialiased`）。
fn antialiased(
    img: &PaddedImage<'_>,
    x1: u32,
    y1: u32,
    width: u32,
    height: u32,
    other: &PaddedImage<'_>,
) -> bool {
    let x0 = x1.saturating_sub(1);
    let y0 = y1.saturating_sub(1);
    let x2 = (x1 + 1).min(width - 1);
    let y2 = (y1 + 1).min(height - 1);

    let mut zeroes = u32::from(x1 == x0 || x1 == x2 || y1 == y0 || y1 == y2);
    let mut min = 0.0_f64;
    let mut max = 0.0_f64;
    let mut min_pos: Option<(u32, u32)> = None;
    let mut max_pos: Option<(u32, u32)> = None;

    for x in x0..=x2 {
        for y in y0..=y2 {
            if x == x1 && y == y1 {
                continue;
            }

            // 中心ピクセルと隣接ピクセルの輝度差
            let delta = color_delta(img, img, (x1, y1), (x, y), true);

            if delta == 0.0 {
                zeroes += 1;
                // 同じ明るさの隣接が 3 つ以上ならアンチエイリアスではない
                if zeroes > 2 {
                    return false;
                }
            } else if delta < min {
                min = delta;
                min_pos = Some((x, y));
            } else if delta > max {
                max = delta;
                max_pos = Some((x, y));
            }
        }
    }

    // 明るい側・暗い側の両方が揃っていなければアンチエイリアスではない
    let (Some((min_x, min_y)), Some((max_x, max_y))) = (min_pos, max_pos) else {
        return false;
    };

    (has_many_siblings(img, min_x, min_y, width, height)
        && has_many_siblings(other, min_x, min_y, width, height))
        || (has_many_siblings(img, max_x, max_y, width, height)
            && has_many_siblings(other, max_x, max_y, width, height))
}

/// 非差分ピクセルの背景描画（本家 `drawGrayPixel`）。
fn gray_pixel(rgba: [f64; 4], alpha: f64) -> Rgba<u8> {
    let val = blend(rgb2y(rgba[0], rgba[1], rgba[2]), alpha * rgba[3] / 255.0);
    let val = val.clamp(0.0, 255.0) as u8;
    Rgba([val, val, val, 255])
}

/// baseline と current を比較して差分ピクセル数と可視化画像を返す。
///
/// 走査領域は両画像の最大寸法。片方だけ範囲外のピクセルは無条件で差分として数える。
pub fn diff_images(baseline: &RgbaImage, current: &RgbaImage, options: &DiffOptions) -> DiffResult {
    let (bw, bh) = baseline.dimensions();
    let (cw, ch) = current.dimensions();
    let width = bw.max(cw);
    let height = bh.max(ch);

    let mut diff_image = RgbaImage::from_pixel(width.max(1), height.max(1), Rgba([0, 0, 0, 255]));
    let total_pixels = u64::from(width) * u64::from(height);

    if total_pixels == 0 {
        return DiffResult {
            diff_pixel_count: 0,
            total_pixels: 0,
            diff_ratio: 0.0,
            diff_image,
        };
    }

    let img1 = PaddedImage::new(baseline);
    let img2 = PaddedImage::new(current);

    // しきい値 0 でも「完全一致は 0」を保つため、比較は `> max_delta` で行う（本家準拠）。
    let max_delta = MAX_YIQ_DELTA * options.threshold * options.threshold;
    let mut diff_pixel_count: u64 = 0;

    for y in 0..height {
        for x in 0..width {
            let in1 = img1.contains(x, y);
            let in2 = img2.contains(x, y);

            // パディング領域: 片方にしか存在しないピクセルは無条件で差分。
            if in1 != in2 {
                diff_image.put_pixel(x, y, DIFF_COLOR);
                diff_pixel_count += 1;
                continue;
            }
            if !in1 {
                // どちらの画像にも無い領域（寸法が縦横で食い違う場合のみ発生）。
                diff_image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
                continue;
            }

            let delta = color_delta(&img1, &img2, (x, y), (x, y), false);

            if delta.abs() > max_delta {
                if !options.include_aa
                    && (antialiased(&img1, x, y, width, height, &img2)
                        || antialiased(&img2, x, y, width, height, &img1))
                {
                    // アンチエイリアス由来: 黄色で描くが差分には数えない
                    diff_image.put_pixel(x, y, AA_COLOR);
                } else {
                    diff_image.put_pixel(x, y, DIFF_COLOR);
                    diff_pixel_count += 1;
                }
            } else {
                diff_image.put_pixel(x, y, gray_pixel(img1.rgba(x, y), BACKGROUND_ALPHA));
            }
        }
    }

    #[allow(clippy::cast_precision_loss)]
    let diff_ratio = diff_pixel_count as f64 / total_pixels as f64;

    DiffResult {
        diff_pixel_count,
        total_pixels,
        diff_ratio,
        diff_image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 単色で塗った RGBA 画像。
    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba(color))
    }

    /// 横方向のグラデーション（AA 検出のスモークテスト用の下地）。
    fn gradient(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, _| {
            let v = (x * 255 / width.max(1)) as u8;
            Rgba([v, v, v, 255])
        })
    }

    #[test]
    fn identical_images_have_no_diff() {
        let a = gradient(20, 20);
        let b = a.clone();
        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(result.diff_pixel_count, 0);
        assert_eq!(result.total_pixels, 400);
        assert_eq!(result.diff_ratio, 0.0);
        assert_eq!(result.diff_image.dimensions(), (20, 20));
    }

    #[test]
    fn single_pixel_change_counts_one() {
        let a = solid(10, 10, [255, 255, 255, 255]);
        let mut b = a.clone();
        // 周囲が一様な白なので AA 判定（暗い側と明るい側の両方が必要）には掛からない。
        b.put_pixel(5, 5, Rgba([0, 0, 0, 255]));

        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(result.diff_pixel_count, 1);
        assert_eq!(result.total_pixels, 100);
        assert!((result.diff_ratio - 0.01).abs() < f64::EPSILON);
        assert_eq!(*result.diff_image.get_pixel(5, 5), DIFF_COLOR);
    }

    #[test]
    fn threshold_masks_small_deltas() {
        let a = solid(4, 4, [128, 128, 128, 255]);
        let mut b = a.clone();
        b.put_pixel(1, 1, Rgba([132, 128, 128, 255]));

        // threshold 0 なら僅差でも差分
        let strict = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(strict.diff_pixel_count, 1);

        // 既定しきい値 0.1（max_delta = 352.15）なら無視される
        let lenient = diff_images(&a, &b, &DiffOptions::with_threshold(0.1));
        assert_eq!(lenient.diff_pixel_count, 0);
    }

    #[test]
    fn size_mismatch_counts_padded_area() {
        let a = solid(4, 4, [10, 20, 30, 255]);
        let b = solid(6, 4, [10, 20, 30, 255]);

        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        // 走査は 6x4、重なる 4x4 は一致、はみ出した 2x4 = 8px が差分。
        assert_eq!(result.total_pixels, 24);
        assert_eq!(result.diff_pixel_count, 8);
        assert_eq!(result.diff_image.dimensions(), (6, 4));
        assert_eq!(*result.diff_image.get_pixel(5, 0), DIFF_COLOR);
    }

    #[test]
    fn size_mismatch_both_out_of_bounds_is_not_a_diff() {
        // 10x1 と 1x10 → 走査 10x10。(5,5) はどちらの画像にも無いので差分にしない。
        let a = solid(10, 1, [0, 0, 0, 255]);
        let b = solid(1, 10, [0, 0, 0, 255]);
        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(result.total_pixels, 100);
        // a のみの領域 9px（y=0, x=1..9）+ b のみの領域 9px（x=0, y=1..9）
        assert_eq!(result.diff_pixel_count, 18);
    }

    #[test]
    fn antialiasing_is_detected_and_not_counted() {
        // 白地に黒の縦線。片方だけ線の端を中間色にして「AA が増えた」状況を作る。
        let mut a = solid(9, 9, [255, 255, 255, 255]);
        for y in 0..9 {
            a.put_pixel(4, y, Rgba([0, 0, 0, 255]));
        }
        let mut b = a.clone();
        for y in 0..9 {
            b.put_pixel(3, y, Rgba([128, 128, 128, 255]));
        }

        let with_aa_detection = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        let without = diff_images(
            &a,
            &b,
            &DiffOptions {
                threshold: 0.0,
                include_aa: true,
            },
        );

        // AA 検出を切ると 9px 全部が差分。検出ありなら AA として除外される分だけ減る。
        assert_eq!(without.diff_pixel_count, 9);
        assert!(
            with_aa_detection.diff_pixel_count < without.diff_pixel_count,
            "AA detection should exclude some pixels: {} vs {}",
            with_aa_detection.diff_pixel_count,
            without.diff_pixel_count
        );
        // 除外されたピクセルは黄色で描かれる
        assert!(
            with_aa_detection
                .diff_image
                .pixels()
                .any(|p| *p == AA_COLOR),
            "expected at least one AA-colored pixel"
        );
    }

    #[test]
    fn background_is_dimmed_grayscale_of_baseline() {
        let a = solid(2, 2, [0, 0, 0, 255]);
        let b = a.clone();
        let result = diff_images(&a, &b, &DiffOptions::default());
        // 黒 (y=0) を alpha 0.1 で白と合成 → 255 + (0 - 255) * 0.1 = 229.5 → 229
        let px = result.diff_image.get_pixel(0, 0).0;
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
        assert_eq!(px[0], 229);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn transparent_pixels_are_blended_with_white() {
        // 完全透明同士は色が違っても差分にならない（白と合成すると同じ）。
        let a = solid(3, 3, [255, 0, 0, 0]);
        let b = solid(3, 3, [0, 0, 255, 0]);
        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(result.diff_pixel_count, 0);
    }

    #[test]
    fn alpha_difference_is_detected() {
        let a = solid(3, 3, [0, 0, 0, 255]);
        let b = solid(3, 3, [0, 0, 0, 0]);
        let result = diff_images(&a, &b, &DiffOptions::with_threshold(0.0));
        assert_eq!(result.diff_pixel_count, 9);
    }

    #[test]
    fn color_delta_is_zero_for_identical_pixels() {
        let img = solid(2, 2, [10, 20, 30, 255]);
        let p = PaddedImage::new(&img);
        assert_eq!(color_delta(&p, &p, (0, 0), (1, 1), false), 0.0);
    }

    #[test]
    fn color_delta_sign_encodes_direction() {
        let dark = solid(1, 1, [0, 0, 0, 255]);
        let light = solid(1, 1, [255, 255, 255, 255]);
        let d = PaddedImage::new(&dark);
        let l = PaddedImage::new(&light);
        // img1 が明るい → 負
        assert!(color_delta(&l, &d, (0, 0), (0, 0), false) < 0.0);
        // img1 が暗い → 正
        assert!(color_delta(&d, &l, (0, 0), (0, 0), false) > 0.0);
    }

    #[test]
    fn max_delta_matches_reference_formula() {
        // 本家: maxDelta = 35215 * threshold * threshold
        assert!((MAX_YIQ_DELTA * 0.1 * 0.1 - 352.15).abs() < 1e-9);
    }
}
