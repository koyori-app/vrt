use std::sync::LazyLock;

use regex::Regex;

pub static COLOR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#[0-9A-Fa-f]{6}$").unwrap());

/// slug（テナント / プロジェクトの URL 断片）: 小文字英数とハイフン。
/// 先頭・末尾のハイフン、連続ハイフンは不可。
pub static SLUG_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap());

/// slug の長さ制限。
pub const SLUG_MIN_LEN: usize = 2;
pub const SLUG_MAX_LEN: usize = 63;

/// ルーティングや将来の予約パスと衝突する slug。テナント / プロジェクト共通で拒否する。
pub const RESERVED_SLUGS: &[&str] = &[
    "admin",
    "api",
    "assets",
    "auth",
    "builds",
    "ci",
    "dashboard",
    "docs",
    "health",
    "help",
    "login",
    "logout",
    "me",
    "new",
    "projects",
    "public",
    "settings",
    "static",
    "support",
    "tenants",
    "users",
    "v1",
];

/// 予約語かどうか。
pub fn is_reserved_slug(slug: &str) -> bool {
    RESERVED_SLUGS.contains(&slug)
}

/// slug の書式エラー。`Display` の文言はそのまま API の 400 本文に載る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlugError {
    TooShort,
    TooLong,
    InvalidFormat,
    Reserved,
}

impl std::fmt::Display for SlugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlugError::TooShort => write!(f, "slug must be at least {SLUG_MIN_LEN} characters"),
            SlugError::TooLong => write!(f, "slug must be at most {SLUG_MAX_LEN} characters"),
            SlugError::InvalidFormat => f.write_str(
                "slug must contain only lowercase letters, digits and single hyphens (e.g. my-team)",
            ),
            SlugError::Reserved => f.write_str("slug is reserved"),
        }
    }
}

/// slug を検証する。
pub fn check_slug(slug: &str) -> Result<(), SlugError> {
    if slug.len() < SLUG_MIN_LEN {
        return Err(SlugError::TooShort);
    }
    if slug.len() > SLUG_MAX_LEN {
        return Err(SlugError::TooLong);
    }
    if !SLUG_REGEX.is_match(slug) {
        return Err(SlugError::InvalidFormat);
    }
    if is_reserved_slug(slug) {
        return Err(SlugError::Reserved);
    }
    Ok(())
}

/// スクリーンショット名 1 件あたりの最大長。**バイト**であって文字数ではない
/// （`String::len()` = UTF-8 バイト長で数える。マルチバイト文字を含む名前は
/// 255 文字よりずっと短いところで上限に達する）。
///
/// 255 バイトの根拠:
///
/// - DB の `screenshots.name` / `baseline_entries.name` は長さ無制限の VARCHAR で、
///   DB は制約源にならない。
/// - 名前の実分布は storybook の story ID（`components-button--primary` 程度、
///   数十バイト）か、CI が PNG のパスから導出する `mobile/home` 形。後者の
///   1 セグメントは主要ファイルシステム（ext4 / APFS 等）の 255 バイト
///   filename 上限に縛られており、成果物や比較レポートで名前をファイル名へ
///   書き戻す運用を壊さない上限として 255 バイトを採る。
/// - アップロード経路は初版からこの値で拒否してきたため、これを超える名前で
///   完走したビルドは存在しえない。全経路をこの値へ寄せても壊れる既存データは
///   無い（緩い側の 512 に寄せると、アップロードだけが通らない名前を計画に
///   載せられてしまい、そのビルドは永久に finalize できない）。
pub const SCREENSHOT_NAME_MAX_BYTES: usize = 255;

/// スクリーンショット名の書式エラー。`Display` の文言はそのまま API の 400 本文に載る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotNameError {
    Empty,
    /// 前後に空白がある。黙って trim せず**拒否**する——サーバーが名前を
    /// 書き換えると、クライアントが計画に載せた名前と保存された名前が
    /// 一致しなくなり、計画との突き合わせが成立しないためである。
    Untrimmed,
    TooLong,
}

impl std::fmt::Display for ScreenshotNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScreenshotNameError::Empty => f.write_str("screenshot name must not be empty"),
            ScreenshotNameError::Untrimmed => f.write_str(
                "screenshot name must not have leading or trailing whitespace \
                 (the name is matched verbatim against the capture plan and \
                 baseline entries, so it is rejected instead of silently trimmed)",
            ),
            ScreenshotNameError::TooLong => write!(
                f,
                "screenshot name must be {SCREENSHOT_NAME_MAX_BYTES} bytes or fewer \
                 (bytes, not characters: multi-byte UTF-8 names hit the limit sooner)"
            ),
        }
    }
}

/// 検証済みスクリーンショット名。
///
/// capture plan（`selected_names` / `manifest_names`）・アップロード・finalize の
/// `captured_names` は**すべて名前の文字列一致**で突き合わせる。経路ごとに規則が
/// ずれると「計画には載せられるのにアップロードできない名前」ができ、finalize は
/// 計画とアップロードの完全一致を要求するため、そのビルドは永久に finalize
/// できない。規則はこの型の [`ScreenshotName::parse`] に一本化し、名前を受け取る
/// 関数は `String` ではなくこの型を要求することで、検証の呼び忘れを型で防ぐ。
///
/// 規則: 空でなく、前後に空白（`char::is_whitespace`）が無く、UTF-8 で
/// [`SCREENSHOT_NAME_MAX_BYTES`] バイト以内。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenshotName(String);

impl ScreenshotName {
    /// 名前を検証して型に包む。スクリーンショット名が外から入る境界
    /// （capture plan・アップロード・finalize の `captured_names`・storybook
    /// レンダリングの名前生成）は必ずここを通る。
    pub fn parse(raw: impl Into<String>) -> Result<Self, ScreenshotNameError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(ScreenshotNameError::Empty);
        }
        if raw.trim() != raw {
            return Err(ScreenshotNameError::Untrimmed);
        }
        if raw.len() > SCREENSHOT_NAME_MAX_BYTES {
            return Err(ScreenshotNameError::TooLong);
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for ScreenshotName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_alnum_and_hyphens() {
        for slug in ["ab", "my-team", "team1", "a-b-c", "0-9"] {
            assert_eq!(check_slug(slug), Ok(()), "{slug} should be valid");
        }
    }

    #[test]
    fn rejects_bad_formats() {
        assert_eq!(check_slug("a"), Err(SlugError::TooShort));
        assert_eq!(check_slug(&"a".repeat(64)), Err(SlugError::TooLong));
        for slug in [
            "-lead",
            "trail-",
            "double--hyphen",
            "UPPER",
            "with space",
            "u_score",
        ] {
            assert_eq!(
                check_slug(slug),
                Err(SlugError::InvalidFormat),
                "{slug} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_reserved_words() {
        assert_eq!(check_slug("admin"), Err(SlugError::Reserved));
        assert_eq!(check_slug("api"), Err(SlugError::Reserved));
    }

    #[test]
    fn screenshot_name_accepts_typical_names() {
        for name in ["home", "components-button--primary", "mobile/home", "あ-ん"] {
            assert!(
                ScreenshotName::parse(name).is_ok(),
                "{name} should be valid"
            );
        }
        assert!(ScreenshotName::parse("a".repeat(SCREENSHOT_NAME_MAX_BYTES)).is_ok());
    }

    #[test]
    fn screenshot_name_rejects_empty_and_whitespace() {
        assert_eq!(ScreenshotName::parse(""), Err(ScreenshotNameError::Empty));
        for name in [" home", "home ", "home\n", "\thome", " "] {
            assert_eq!(
                ScreenshotName::parse(name),
                Err(ScreenshotNameError::Untrimmed),
                "{name:?} should be rejected, not trimmed"
            );
        }
        // 内側の空白は名前の一部として有効（trim では消えない位置）。
        assert!(ScreenshotName::parse("home page").is_ok());
    }

    #[test]
    fn screenshot_name_limit_is_bytes_not_chars() {
        assert_eq!(
            ScreenshotName::parse("a".repeat(SCREENSHOT_NAME_MAX_BYTES + 1)),
            Err(ScreenshotNameError::TooLong)
        );
        // "あ" は UTF-8 で 3 バイト。86 文字 = 258 バイトは、255 **文字**より
        // はるかに短いのに上限超過になる（バイトと文字の取り違え検出）。
        let multibyte = "あ".repeat(86);
        assert_eq!(multibyte.chars().count(), 86);
        assert_eq!(multibyte.len(), 258);
        assert_eq!(
            ScreenshotName::parse(multibyte),
            Err(ScreenshotNameError::TooLong)
        );
        // 85 文字 = 255 バイトはちょうど収まる。
        assert!(ScreenshotName::parse("あ".repeat(85)).is_ok());
    }
}
