//! Finding the program a command starts, the way a shell would. The doctor uses it to flag a
//! hook whose program is missing, and the hook runner uses it to find the first word of an
//! invocation. `PATH` comes from [`ScanEnv`], never from the process.

use crate::env::ScanEnv;
use crate::model::AbsPath;
use crate::probe_fs::{EntryKind, ProbeFs};

/// What a word resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A regular file. `executable` is false when no execute bit is set.
    Found {
        path: AbsPath,
        executable: bool,
    },
    NotFound,
}

/// Words a shell handles itself, so they need no file.
const SHELL_WORDS: &[&str] = &[
    ".", ":", "[", "[[", "alias", "bg", "break", "builtin", "case", "cd", "command", "continue",
    "do", "done", "echo", "elif", "else", "esac", "eval", "exec", "exit", "export", "false", "fg",
    "fi", "for", "function", "if", "in", "jobs", "local", "popd", "printf", "pushd", "pwd", "read",
    "return", "set", "shift", "source", "test", "then", "time", "trap", "true", "type", "ulimit",
    "umask", "unset", "until", "wait", "while",
];

/// Resolves `word` the way a shell does. A word with a `/` is a path (`~` is the home
/// directory, a relative path starts at `cwd`). Any other word is searched in the `PATH` of
/// `env`, and only an executable regular file counts.
pub fn resolve(env: &ScanEnv, fs: &ProbeFs, cwd: &AbsPath, word: &str) -> Program {
    if word.is_empty() {
        return Program::NotFound;
    }
    if word.contains('/') {
        let path = match word.strip_prefix("~/") {
            Some(rest) => env.home.join(rest),
            None if word.starts_with('/') => match AbsPath::parse(word) {
                Ok(path) => path,
                Err(_) => return Program::NotFound,
            },
            None => cwd.join(word),
        };
        return match fs.stat(&path) {
            Ok(stat) if stat.kind == EntryKind::File => Program::Found {
                path,
                executable: stat.executable,
            },
            _ => Program::NotFound,
        };
    }
    env.var("PATH")
        .unwrap_or("")
        .split(':')
        .filter(|dir| dir.starts_with('/'))
        .filter_map(|dir| AbsPath::parse(dir).ok())
        .map(|dir| dir.join(word))
        .find_map(|candidate| match fs.stat(&candidate) {
            Ok(stat) if stat.kind == EntryKind::File && stat.executable => Some(Program::Found {
                path: candidate,
                executable: true,
            }),
            _ => None,
        })
        .unwrap_or(Program::NotFound)
}

/// The program a shell command line starts: the first word after leading `NAME=value`
/// assignments, with one level of quotes removed and the variables in `vars` (plus `HOME`)
/// expanded. `None` when the word is a shell word, or when it depends on something this cannot
/// know (command substitution, an unknown variable, a glob).
pub fn first_word(command: &str, env: &ScanEnv, vars: &[(String, String)]) -> Option<String> {
    let mut rest = command.trim_start();
    loop {
        let (word, tail) = split_word(rest)?;
        if is_assignment(&word) {
            rest = tail.trim_start();
            continue;
        }
        let expanded = expand(&word, env, vars)?;
        if SHELL_WORDS.contains(&expanded.as_str())
            || expanded.starts_with(['!', '(', '{', '-'])
            || expanded.contains(['*', '?', '`'])
        {
            return None;
        }
        return Some(expanded);
    }
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// The first shell word of `text` with quotes removed, and what follows it.
fn split_word(text: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for (index, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>') => {
                return (!word.is_empty()).then_some((word, &text[index..]));
            }
            None => word.push(c),
        }
    }
    (quote.is_none() && !word.is_empty()).then_some((word, ""))
}

/// Expands `$NAME`, `${NAME}` and a leading `~`. `None` for a variable it cannot resolve.
fn expand(word: &str, env: &ScanEnv, vars: &[(String, String)]) -> Option<String> {
    let lookup = |name: &str| -> Option<String> {
        if name == "HOME" {
            return Some(env.home.as_str().to_owned());
        }
        vars.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let word = match word.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", env.home.as_str())
        }
        _ => word.to_owned(),
    };
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let name: String = if chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            loop {
                match chars.next()? {
                    '}' => break,
                    c => name.push(c),
                }
            }
            name
        } else {
            let mut name = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    name.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            name
        };
        if name.is_empty() {
            return None;
        }
        out.push_str(&lookup(&name)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixture;

    fn word(fx: &Fixture, command: &str) -> Option<String> {
        let vars = [("CLAUDE_PROJECT_DIR".to_owned(), "/p".to_owned())];
        first_word(command, &fx.env(), &vars)
    }

    #[test]
    fn the_first_word_skips_assignments_and_strips_quotes() {
        let fx = Fixture::new();
        assert_eq!(word(&fx, "FOO=1 BAR=2 node run.js"), Some("node".into()));
        assert_eq!(
            word(&fx, "\"$CLAUDE_PROJECT_DIR\"/h.sh x"),
            Some("/p/h.sh".into())
        );
        assert_eq!(
            word(&fx, "${CLAUDE_PROJECT_DIR}/h.sh"),
            Some("/p/h.sh".into())
        );
        assert_eq!(
            word(&fx, "'/with space/run' a"),
            Some("/with space/run".into())
        );
        assert_eq!(word(&fx, "~/bin/x"), Some(format!("{}/bin/x", fx.home())));
    }

    #[test]
    fn shell_words_and_unknowns_have_no_program() {
        let fx = Fixture::new();
        for command in [
            "cd /x && ls",
            "echo hi",
            "if true; then x; fi",
            "$UNKNOWN/x",
            "$(which x)",
            "",
            "   ",
        ] {
            assert_eq!(word(&fx, command), None, "{command:?}");
        }
    }

    #[test]
    fn a_word_is_searched_in_the_path_of_the_environment_and_must_be_executable() {
        let fx = Fixture::new();
        let bin = fx.root().join("bin");
        fx.write(&bin.join("tool"), "#!/bin/sh\n");
        fx.write(&bin.join("plain"), "data");
        std::fs::set_permissions(
            bin.join("tool").as_path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let env = fx.env().with_var("PATH", &format!("/nonexistent:{bin}"));
        let fs = ProbeFs::new();
        let cwd = fx.project();
        assert_eq!(
            resolve(&env, &fs, &cwd, "tool"),
            Program::Found {
                path: bin.join("tool"),
                executable: true
            }
        );
        assert_eq!(resolve(&env, &fs, &cwd, "plain"), Program::NotFound);
        assert_eq!(resolve(&fx.env(), &fs, &cwd, "tool"), Program::NotFound);
        // A path is not searched, and reports a missing execute bit.
        assert_eq!(
            resolve(&env, &fs, &cwd, &format!("{bin}/plain")),
            Program::Found {
                path: bin.join("plain"),
                executable: false
            }
        );
        fx.write(&fx.project_path("hooks/h.sh"), "x");
        assert!(matches!(
            resolve(&env, &fs, &cwd, "./hooks/h.sh"),
            Program::Found {
                executable: false,
                ..
            }
        ));
    }
}
