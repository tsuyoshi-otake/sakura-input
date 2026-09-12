fn production() {}

#[allow(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_comments_do_not_close_the_module() {
        let raw = r#"} ; #[cfg(test)]
{"#;
        assert!(!raw.is_empty());
    }

    /* outer } {
       /* inner } */
    */
}
