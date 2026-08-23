//! Shared page-size policy for database result editors.

/// The default keeps the first database response bounded while remaining useful for scanning.
pub const DEFAULT_RESULT_PAGE_SIZE: usize = 100;
/// Preset sizes exposed by the result-editor menu.
pub const RESULT_PAGE_SIZE_PRESETS: [usize; 4] = [100, 200, 500, 1000];
/// Custom values stay bounded so a single interaction cannot request an unreasonably large page.
pub const MAX_RESULT_PAGE_SIZE: usize = 10_000;

/// Validates a page size before it is stored in a pager or sent to a database driver.
pub fn validate_result_page_size(value: usize) -> Result<usize, &'static str> {
    if (1..=MAX_RESULT_PAGE_SIZE).contains(&value) {
        Ok(value)
    } else {
        Err("每页行数必须是 1-10000 的整数")
    }
}

/// Parses the custom page-size dialog input and returns a bounded positive integer.
pub fn parse_result_page_size(value: &str) -> Result<usize, &'static str> {
    let value = value
        .trim()
        .parse::<usize>()
        .map_err(|_| "请输入 1-10000 的整数")?;
    validate_result_page_size(value).map_err(|_| "请输入 1-10000 的整数")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_default_presets_and_bounded_custom_values() {
        assert_eq!(validate_result_page_size(DEFAULT_RESULT_PAGE_SIZE), Ok(100));
        assert_eq!(validate_result_page_size(10_000), Ok(10_000));
        assert_eq!(parse_result_page_size(" 500 "), Ok(500));
    }

    #[test]
    fn rejects_zero_out_of_range_and_non_numeric_values() {
        assert!(validate_result_page_size(0).is_err());
        assert!(validate_result_page_size(10_001).is_err());
        assert!(parse_result_page_size("many").is_err());
    }
}
