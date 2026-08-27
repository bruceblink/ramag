pub(super) fn parse_result_page(value: &str, total_pages: u64) -> Result<usize, String> {
    let page = value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("页码必须是 1-{total_pages} 的整数"))?;
    if page == 0 || page > total_pages {
        return Err(format!("页码必须在 1-{total_pages} 范围内"));
    }
    usize::try_from(page - 1).map_err(|_| "页码超出当前平台可定位范围".into())
}

#[cfg(test)]
mod tests {
    use super::parse_result_page;

    #[test]
    fn parses_one_based_page_into_zero_based_index() {
        assert_eq!(parse_result_page(" 3 ", 5).unwrap(), 2);
    }

    #[test]
    fn rejects_page_outside_known_total() {
        assert!(parse_result_page("0", 5).is_err());
        assert!(parse_result_page("6", 5).is_err());
        assert!(parse_result_page("next", 5).is_err());
    }
}
