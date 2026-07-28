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
}
