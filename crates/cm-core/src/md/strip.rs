//! What Claude Code removes before a file reaches the model (Claude spec 1.8): YAML
//! frontmatter and block-level HTML comments. An inline comment and a comment inside a code
//! block stay.

use super::{frontmatter, spans};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stripped {
    pub text: String,
    /// Whether frontmatter or a comment was removed.
    pub transformed: bool,
}

pub fn strip(text: &str) -> Stripped {
    let fm = frontmatter::parse(text);
    let body = &text[fm.body.clone()];
    let comments = spans::scan(body).comments;
    let mut out = String::with_capacity(body.len());
    let mut at = 0;
    for comment in &comments {
        out.push_str(&body[at..comment.range.start]);
        at = comment.range.end;
        // A comment that fills its own lines takes its line break with it.
        let own_line = body[..comment.range.start]
            .rsplit('\n')
            .next()
            .is_some_and(|before| before.trim().is_empty());
        if own_line && body[at..].starts_with('\n') {
            at += 1;
        }
    }
    out.push_str(&body[at..]);
    Stripped {
        transformed: fm.is_present() || !comments.is_empty(),
        text: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_and_block_comments_are_removed() {
        let text = "---\npaths: [a]\n---\n<!-- note\nmore -->\n# Title\nbody\n";
        let out = strip(text);
        assert_eq!(out.text, "# Title\nbody\n");
        assert!(out.transformed);
    }

    #[test]
    fn inline_comments_and_comments_in_fences_stay() {
        let text = "Inline <!-- keep --> after\n```\n<!-- keep too -->\n```\n";
        let out = strip(text);
        assert_eq!(out.text, text);
        assert!(!out.transformed);
    }

    #[test]
    fn a_comment_that_is_not_closed_removes_the_rest() {
        assert_eq!(strip("a\n<!-- open\nb\n").text, "a\n");
    }
}
