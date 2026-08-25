//! DTO で共有する serde の小道具。

use serde::{Deserialize, Deserializer};

/// `Option<Option<T>>` を「フィールド省略 = `None`」「`null` 送信 = `Some(None)`」
/// 「値送信 = `Some(Some(v))`」に分離してデシリアライズする。
///
/// 素の `Option<Option<T>>` は serde が最外の `null` を `None` に潰してしまい、
/// 「未指定（据え置き）」と「明示的な NULL 化」を区別できないため、`#[serde(default,
/// deserialize_with = "double_option")]` と併用してこの区別を復元する。
pub(crate) fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}
