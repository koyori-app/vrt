//! png crate の pre-IDAT 検証挙動の conformance corpus。
//!
//! vrt-actions(GitHub Action)の `scripts/lib.sh` `png_dimensions` は、
//! `validate_png`(image → png crate)がアップロード時に行う検証の
//! 「署名〜最初の IDAT 直前」の挙動を bash で写している。事前検証と
//! サーバー検証がズレると、ビルド作成後にアップロードが拒否されて
//! 未 finalize のビルドが残る。
//!
//! この corpus は両者の合意点(受理・拒絶の境界)を列挙したもので、
//! `cargo update` 等で png crate の挙動が変わるとここが失敗する。
//! 失敗した場合は、単にこのテストを直すのではなく、vrt-actions 側の
//! `scripts/lib.sh` と `tests/test-collect-pngs.sh` を必ず追随させること。
//! 参照: https://github.com/koyori-app/vrt-actions
//!
//! corpus は vrt-actions の `tests/test-collect-pngs.sh` と同じケースを
//! Rust で構築している(fixture 生成は tests/helpers.sh `write_png_dims` と同形)。

use service::screenshots::validate_png;

/// zlib CRC-32(依存を増やさないためのローカル実装)。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(ctype: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(12 + data.len());
    c.extend_from_slice(&(data.len() as u32).to_be_bytes());
    c.extend_from_slice(ctype);
    c.extend_from_slice(data);
    let mut crc_input = ctype.to_vec();
    crc_input.extend_from_slice(data);
    c.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    c
}

/// チャンク末尾の CRC 1 バイトを反転させる(helpers.sh の ":badcrc" と同じ)。
fn bad_crc(mut c: Vec<u8>) -> Vec<u8> {
    let last = c.len() - 1;
    c[last] ^= 0xFF;
    c
}

fn ihdr_data(w: u32, h: u32, depth: u8, color: u8, comp: u8, filt: u8, inter: u8) -> Vec<u8> {
    let mut d = Vec::with_capacity(13);
    d.extend_from_slice(&w.to_be_bytes());
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, color, comp, filt, inter]);
    d
}

/// 署名 + IHDR + extras + 長さ 0 の IDAT チャンクヘッダ。
/// vrt-actions の write_png_dims と同じ形(デコード可能な画像ではない)。
fn png_with(ihdr: &[u8], extras: &[Vec<u8>]) -> Vec<u8> {
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    p.extend(chunk(b"IHDR", ihdr));
    for e in extras {
        p.extend_from_slice(e);
    }
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"IDAT");
    p
}

fn rgba(w: u32, h: u32) -> Vec<u8> {
    ihdr_data(w, h, 8, 6, 0, 0, 0)
}

#[allow(clippy::too_many_arguments)]
fn fctl_data(seq: u32, w: u32, h: u32, x: u32, y: u32, dispose: u8, blend: u8) -> Vec<u8> {
    let mut d = Vec::with_capacity(26);
    d.extend_from_slice(&seq.to_be_bytes());
    d.extend_from_slice(&w.to_be_bytes());
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&x.to_be_bytes());
    d.extend_from_slice(&y.to_be_bytes());
    d.extend_from_slice(&1u16.to_be_bytes()); // delay_num
    d.extend_from_slice(&100u16.to_be_bytes()); // delay_den
    d.push(dispose);
    d.push(blend);
    d
}

#[test]
fn png_crate_pre_idat_conformance() {
    let full = fctl_data(0, 10, 10, 0, 0, 0, 0);
    let cases: Vec<(&str, Vec<u8>, bool)> = vec![
        // --- 拒絶: 署名・切断・CRC --------------------------------------------
        (
            "not a png at all",
            b"this is text, not a png".to_vec(),
            false,
        ),
        (
            "cut mid-IHDR (24 bytes)",
            png_with(&rgba(10, 10), &[])[..24].to_vec(),
            false,
        ),
        (
            "signature + full IHDR, no IDAT",
            {
                let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
                p.extend(chunk(b"IHDR", &rgba(10, 10)));
                p
            },
            false,
        ),
        (
            "IHDR with corrupted CRC",
            {
                let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
                p.extend(bad_crc(chunk(b"IHDR", &rgba(10, 10))));
                p.extend_from_slice(&0u32.to_be_bytes());
                p.extend_from_slice(b"IDAT");
                p
            },
            false,
        ),
        // --- 拒絶: IHDR の意味検証 --------------------------------------------
        (
            "undefined color type 5",
            png_with(&ihdr_data(10, 10, 8, 5, 0, 0, 0), &[]),
            false,
        ),
        (
            "bit depth 3 for grayscale",
            png_with(&ihdr_data(10, 10, 3, 0, 0, 0, 0), &[]),
            false,
        ),
        (
            "nonzero compression",
            png_with(&ihdr_data(10, 10, 8, 6, 1, 0, 0), &[]),
            false,
        ),
        (
            "nonzero filter",
            png_with(&ihdr_data(10, 10, 8, 6, 0, 1, 0), &[]),
            false,
        ),
        (
            "interlace method 2",
            png_with(&ihdr_data(10, 10, 8, 6, 0, 0, 2), &[]),
            false,
        ),
        (
            "zero width",
            png_with(&ihdr_data(0, 100, 8, 6, 0, 0, 0), &[]),
            false,
        ),
        // 10001px は crate は通すが validate_png の MAX_DIMENSION が拒否する。
        (
            "width over server limit",
            png_with(&ihdr_data(10001, 100, 8, 6, 0, 0, 0), &[]),
            false,
        ),
        // --- 拒絶: critical チャンクの構成 ------------------------------------
        (
            "duplicate IHDR",
            png_with(&rgba(10, 10), &[chunk(b"IHDR", &rgba(10, 10))]),
            false,
        ),
        (
            "unknown critical chunk",
            png_with(&rgba(10, 10), &[chunk(b"ABCD", b"")]),
            false,
        ),
        (
            "duplicate PLTE",
            png_with(
                &rgba(10, 10),
                &[chunk(b"PLTE", &[0; 3]), chunk(b"PLTE", &[0; 3])],
            ),
            false,
        ),
        (
            "PLTE length 2",
            png_with(&rgba(10, 10), &[chunk(b"PLTE", &[0; 2])]),
            false,
        ),
        (
            "PLTE length 774",
            png_with(&rgba(10, 10), &[chunk(b"PLTE", &[0; 774])]),
            false,
        ),
        (
            "PLTE with bad CRC",
            png_with(&rgba(10, 10), &[bad_crc(chunk(b"PLTE", &[1, 2, 3]))]),
            false,
        ),
        // --- 拒絶: fcTL / fdAT ------------------------------------------------
        (
            "fcTL with bad length",
            png_with(&rgba(10, 10), &[chunk(b"fcTL", &[0; 4])]),
            false,
        ),
        (
            "fcTL bad length and bad CRC",
            png_with(&rgba(10, 10), &[bad_crc(chunk(b"fcTL", &[0; 4]))]),
            false,
        ),
        (
            "fcTL with nonzero sequence",
            png_with(
                &rgba(10, 10),
                &[chunk(b"fcTL", &fctl_data(1, 10, 10, 0, 0, 0, 0))],
            ),
            false,
        ),
        (
            "two fcTL repeating sequence 0",
            png_with(
                &rgba(10, 10),
                &[chunk(b"fcTL", &full), chunk(b"fcTL", &full)],
            ),
            false,
        ),
        (
            "fcTL frame smaller than image",
            png_with(
                &rgba(10, 10),
                &[chunk(b"fcTL", &fctl_data(0, 5, 5, 0, 0, 0, 0))],
            ),
            false,
        ),
        (
            "fcTL frame larger than image",
            png_with(
                &rgba(10, 10),
                &[chunk(b"fcTL", &fctl_data(0, 11, 10, 0, 0, 0, 0))],
            ),
            false,
        ),
        (
            "fcTL with invalid blend op",
            png_with(
                &rgba(10, 10),
                &[chunk(b"fcTL", &fctl_data(0, 10, 10, 0, 0, 0, 2))],
            ),
            false,
        ),
        (
            "fdAT before the first IDAT",
            png_with(&rgba(10, 10), &[chunk(b"fdAT", &[0, 0, 0, 0, 1, 2, 3])]),
            false,
        ),
        (
            "fdAT before IDAT with bad CRC",
            png_with(
                &rgba(10, 10),
                &[bad_crc(chunk(b"fdAT", &[0, 0, 0, 0, 1, 2, 3]))],
            ),
            false,
        ),
        // --- 受理: crate が黙って許すもの(こちらが弾いてはいけない)----------
        ("plain 10x10 RGBA", png_with(&rgba(10, 10), &[]), true),
        (
            "Adam7 interlace",
            png_with(&ihdr_data(10, 10, 8, 6, 0, 0, 1), &[]),
            true,
        ),
        (
            "unknown ancillary chunk",
            png_with(&rgba(10, 10), &[chunk(b"abCD", b"")]),
            true,
        ),
        (
            "single valid PLTE and fcTL",
            png_with(
                &rgba(10, 10),
                &[chunk(b"PLTE", &[0; 3]), chunk(b"fcTL", &full)],
            ),
            true,
        ),
        (
            "PLTE length not divisible by 3",
            png_with(&rgba(10, 10), &[chunk(b"PLTE", &[1, 2, 3, 4])]),
            true,
        ),
        (
            "tRNS on an alpha color type",
            png_with(&rgba(10, 10), &[chunk(b"tRNS", &[0, 1, 0, 2, 0, 3])]),
            true,
        ),
        (
            "short grayscale tRNS",
            png_with(&ihdr_data(10, 10, 8, 0, 0, 0, 0), &[chunk(b"tRNS", &[7])]),
            true,
        ),
        (
            "sBIT with wrong length",
            png_with(&rgba(10, 10), &[chunk(b"sBIT", &[8, 8])]),
            true,
        ),
        (
            "acTL with zero frames",
            png_with(&rgba(10, 10), &[chunk(b"acTL", &[0; 8])]),
            true,
        ),
        (
            "ancillary chunk with bad CRC",
            png_with(
                &rgba(10, 10),
                &[bad_crc(chunk(b"gAMA", &45455u32.to_be_bytes()))],
            ),
            true,
        ),
        (
            "invalid fcTL hidden by a bad CRC",
            png_with(
                &rgba(10, 10),
                &[bad_crc(chunk(b"fcTL", &fctl_data(1, 2, 2, 0, 0, 0, 0)))],
            ),
            true,
        ),
        (
            "two fcTL in sequence order",
            png_with(
                &rgba(10, 10),
                &[
                    chunk(b"fcTL", &full),
                    chunk(b"fcTL", &fctl_data(1, 10, 10, 0, 0, 0, 0)),
                ],
            ),
            true,
        ),
    ];

    let mut failures = Vec::new();
    for (name, bytes, expect_ok) in &cases {
        let result = validate_png(bytes);
        if result.is_ok() != *expect_ok {
            failures.push(format!(
                "{name}: expected {}, got {:?}",
                if *expect_ok { "accept" } else { "reject" },
                result.map(|_| "accepted")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "png crate behavior drifted from the vrt-actions pre-validation corpus.\n\
         Update scripts/lib.sh png_dimensions (and its tests) in koyori-app/vrt-actions\n\
         to match BEFORE fixing this test:\n{}",
        failures.join("\n")
    );
}
