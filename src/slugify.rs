/// Convert a prompt into a short session slug.
///
/// Rules (matching the original bash implementation):
/// - Lowercase
/// - Strip non-alphanumeric characters (keep spaces)
/// - Take first 4 characters of first 4 words
/// - Join with hyphens
/// - Truncate to 30 characters
pub fn slugify(input: &str) -> String {
    let cleaned: String = input
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == ' ')
        .collect();

    let slug: String = cleaned
        .split_whitespace()
        .take(4)
        .map(|w| {
            let end = w.len().min(4);
            &w[..end]
        })
        .collect::<Vec<_>>()
        .join("-");

    let end = slug.len().min(30);
    slug[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn single_word() {
        assert_eq!(slugify("hello"), "hell");
    }

    #[test]
    fn short_word() {
        assert_eq!(slugify("hi"), "hi");
    }

    #[test]
    fn four_words() {
        assert_eq!(slugify("implement the login feature"), "impl-the-logi-feat");
    }

    #[test]
    fn more_than_four_words() {
        assert_eq!(
            slugify("implement the login feature now please"),
            "impl-the-logi-feat"
        );
    }

    #[test]
    fn mixed_case() {
        assert_eq!(slugify("Hello World"), "hell-worl");
    }

    #[test]
    fn special_characters_stripped() {
        assert_eq!(slugify("fix bug #123!"), "fix-bug-123");
    }

    #[test]
    fn unicode_stripped() {
        assert_eq!(slugify("fix the bug"), "fix-the-bug");
    }

    #[test]
    fn truncated_at_30_chars() {
        let long = "abcdefgh ijklmnop qrstuvwx yzabcdef ghijklmn";
        let result = slugify(long);
        assert!(result.len() <= 30, "got len {}: {result}", result.len());
    }

    #[test]
    fn matches_bash_output() {
        // The bash slugify for our test prompt
        assert_eq!(
            slugify("Create a file called /tmp/cortex-throwaway-test.txt with the text"),
            "crea-a-file-call"
        );
    }
}
