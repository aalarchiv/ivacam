//! Minimal embedded i18n for the headless CLI.
//!
//! The CLI runs without the frontend, so it carries its own tiny catalog
//! (`crates/ivac-cli/i18n/{en,de,ko}.json`, embedded via `include_str!`) instead
//! of the Svelte `t()`/JSON pipeline. No external i18n crate: lookup is a flat
//! `key -> template` map with `{name}` placeholder substitution, mirroring the
//! frontend's `lookup()` semantics (locale -> English -> the key itself).
//!
//! Locale resolution: `--lang <en|de|ko>` wins, else the `LC_ALL`/`LC_MESSAGES`/
//! `LANG` environment (POSIX precedence), else English.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A shipped UI language. Keep in sync with the catalog files below and the
/// frontend `SUPPORTED_LOCALES`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locale {
    En,
    De,
    Ko,
}

const EN_JSON: &str = include_str!("../i18n/en.json");
const DE_JSON: &str = include_str!("../i18n/de.json");
const KO_JSON: &str = include_str!("../i18n/ko.json");

/// Parse a locale tag (`de`, `en-US`, `de_DE.UTF-8`, `ko_KR.UTF-8`, `DE`) down
/// to a shipped language, or `None` if we don't ship it. Splits on the RFC-5646
/// `-`, the POSIX `_`, and the `.<charset>` suffix, then lowercases the base
/// subtag.
pub fn parse_lang(tag: &str) -> Option<Locale> {
    let base = tag
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "en" => Some(Locale::En),
        "de" => Some(Locale::De),
        "ko" => Some(Locale::Ko),
        _ => None,
    }
}

/// Resolve the effective locale: an explicit `--lang` override first, then the
/// environment (`env(name)` returns the value of that variable), then English.
pub fn detect_locale(lang_override: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Locale {
    if let Some(l) = lang_override.and_then(parse_lang) {
        return l;
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(l) = env(var).as_deref().and_then(parse_lang) {
            return l;
        }
    }
    Locale::En
}

/// Pull a `--lang <value>` / `--lang=<value>` flag out of the raw argument
/// list, returning the override (last one wins) and the remaining args with the
/// flag removed. Done before subcommand dispatch so `--lang` works globally.
pub fn extract_lang(args: Vec<String>) -> (Option<String>, Vec<String>) {
    let mut lang = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--lang" {
            // Consume the following token as the value if present.
            if let Some(v) = it.next() {
                lang = Some(v);
            }
        } else if let Some(v) = a.strip_prefix("--lang=") {
            lang = Some(v.to_string());
        } else {
            rest.push(a);
        }
    }
    (lang, rest)
}

fn catalog(locale: Locale) -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    static DE: OnceLock<HashMap<String, String>> = OnceLock::new();
    static KO: OnceLock<HashMap<String, String>> = OnceLock::new();
    let (cell, json) = match locale {
        Locale::En => (&EN, EN_JSON),
        Locale::De => (&DE, DE_JSON),
        Locale::Ko => (&KO, KO_JSON),
    };
    cell.get_or_init(|| serde_json::from_str(json).expect("embedded i18n catalog is valid JSON"))
}

/// Look up `key` in `locale`, falling back to English then to the key itself,
/// and substitute `{name}` placeholders from `params`. Pure — no global state,
/// so it's directly unit-testable.
pub fn tr(locale: Locale, key: &str, params: &[(&str, &str)]) -> String {
    let template = catalog(locale)
        .get(key)
        .or_else(|| catalog(Locale::En).get(key))
        .map_or(key, String::as_str);
    let mut out = template.to_string();
    for (name, value) in params {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

static LOCALE: OnceLock<Locale> = OnceLock::new();

/// Set the process-wide locale once, at startup. Later calls are ignored.
pub fn set_locale(locale: Locale) {
    let _ = LOCALE.set(locale);
}

fn current() -> Locale {
    *LOCALE.get().unwrap_or(&Locale::En)
}

/// Translate `key` in the process locale.
pub fn t(key: &str) -> String {
    tr(current(), key, &[])
}

/// Translate `key` with `{name}` placeholder substitutions.
pub fn tp(key: &str, params: &[(&str, &str)]) -> String {
    tr(current(), key, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalog key the CLI actually references. Keeping this list beside
    /// the code makes a stray `t("cli.…")` typo (or a key dropped from the
    /// catalog) a test failure — the CLI analog of the frontend's "missing key
    /// => svelte-check error" guard.
    const CLI_KEYS: &[&str] = &[
        "cli.help.tagline",
        "cli.help.usage",
        "cli.help.import",
        "cli.help.generate",
        "cli.help.stream",
        "cli.help.posts",
        "cli.help.help",
        "cli.help.lang",
        "cli.err.unknown_subcommand",
        "cli.err.missing_path",
        "cli.err.importing",
        "cli.err.opt_needs_value",
        "cli.err.unexpected_arg",
        "cli.err.missing_input_path",
        "cli.err.unknown_post",
        "cli.err.reading",
        "cli.err.parse_project",
        "cli.err.output",
        "cli.err.streaming",
        "cli.err.two_sided_needs_output",
        "cli.stream.done",
    ];

    fn parse(json: &str) -> HashMap<String, String> {
        serde_json::from_str(json).expect("catalog is valid JSON")
    }

    fn sorted_keys(map: &HashMap<String, String>) -> Vec<&String> {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        keys
    }

    #[test]
    fn catalogs_parse_and_are_key_identical() {
        let en = parse(EN_JSON);
        let de = parse(DE_JSON);
        let ko = parse(KO_JSON);
        assert_eq!(
            sorted_keys(&en),
            sorted_keys(&de),
            "crates/ivac-cli/i18n/en.json and de.json must have identical keys"
        );
        assert_eq!(
            sorted_keys(&en),
            sorted_keys(&ko),
            "crates/ivac-cli/i18n/en.json and ko.json must have identical keys"
        );
    }

    #[test]
    fn placeholders_match_english_in_every_catalog() {
        fn placeholders(text: &str) -> Vec<String> {
            let mut out = Vec::new();
            let mut rest = text;
            while let Some((_, after)) = rest.split_once('{') {
                let Some((name, remaining)) = after.split_once('}') else {
                    break;
                };
                out.push(name.to_string());
                rest = remaining;
            }
            out.sort();
            out
        }
        let en = parse(EN_JSON);
        for (id, json) in [("de", DE_JSON), ("ko", KO_JSON)] {
            for (key, text) in parse(json) {
                assert_eq!(
                    placeholders(&text),
                    placeholders(&en[&key]),
                    "{id}: placeholder mismatch in {key}"
                );
            }
        }
    }

    #[test]
    fn en_catalog_matches_referenced_keys_exactly() {
        let en = parse(EN_JSON);
        let mut expected: Vec<&str> = CLI_KEYS.to_vec();
        expected.sort_unstable();
        let mut actual: Vec<&str> = en.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "en.json keys must match exactly the keys the CLI references (CLI_KEYS)"
        );
    }

    #[test]
    fn parse_lang_handles_common_tags() {
        assert_eq!(parse_lang("de"), Some(Locale::De));
        assert_eq!(parse_lang("DE"), Some(Locale::De));
        assert_eq!(parse_lang("de_DE.UTF-8"), Some(Locale::De));
        assert_eq!(parse_lang("en-US"), Some(Locale::En));
        assert_eq!(parse_lang("de_AT@euro"), Some(Locale::De));
        assert_eq!(parse_lang("ko"), Some(Locale::Ko));
        assert_eq!(parse_lang("KO"), Some(Locale::Ko));
        assert_eq!(parse_lang("ko_KR.UTF-8"), Some(Locale::Ko));
        assert_eq!(parse_lang("ko-KR"), Some(Locale::Ko));
        assert_eq!(parse_lang("fr"), None);
        assert_eq!(parse_lang(""), None);
    }

    #[test]
    fn detect_prefers_override_then_lc_all_then_lang() {
        let env = |k: &str| match k {
            "LANG" => Some("en_US.UTF-8".to_string()),
            "LC_ALL" => Some("de_DE.UTF-8".to_string()),
            _ => None,
        };
        // Override wins over everything.
        assert_eq!(detect_locale(Some("de"), env), Locale::De);
        assert_eq!(detect_locale(Some("ko"), env), Locale::Ko);
        // No override: LC_ALL beats LANG.
        assert_eq!(detect_locale(None, env), Locale::De);
        // Unshipped override falls through to the environment.
        assert_eq!(
            detect_locale(Some("fr"), |_| Some("en".to_string())),
            Locale::En
        );
        // A Korean POSIX locale in the environment is picked up.
        assert_eq!(
            detect_locale(None, |k| (k == "LANG").then(|| "ko_KR.UTF-8".to_string())),
            Locale::Ko
        );
        // Nothing set at all -> English.
        assert_eq!(detect_locale(None, |_| None), Locale::En);
    }

    #[test]
    fn extract_lang_pulls_flag_in_both_forms() {
        let (lang, rest) = extract_lang(vec![
            "generate".into(),
            "--lang".into(),
            "de".into(),
            "file.dxf".into(),
        ]);
        assert_eq!(lang.as_deref(), Some("de"));
        assert_eq!(rest, vec!["generate", "file.dxf"]);

        let (lang, rest) = extract_lang(vec!["--lang=ko".into(), "import".into()]);
        assert_eq!(lang.as_deref(), Some("ko"));
        assert_eq!(rest, vec!["import"]);

        let (lang, rest) = extract_lang(vec!["import".into(), "file.dxf".into()]);
        assert_eq!(lang, None);
        assert_eq!(rest, vec!["import", "file.dxf"]);
    }

    #[test]
    fn tr_substitutes_params_and_falls_back() {
        // Known key + placeholder.
        assert_eq!(
            tr(Locale::En, "cli.err.unknown_post", &[("name", "svg")]),
            "unknown post processor: svg"
        );
        assert_eq!(
            tr(Locale::Ko, "cli.err.unknown_post", &[("name", "svg")]),
            "알 수 없는 포스트 프로세서: svg"
        );
        // Unknown key falls back to the key itself.
        assert_eq!(tr(Locale::En, "cli.nope", &[]), "cli.nope");
        assert_eq!(tr(Locale::Ko, "cli.nope", &[]), "cli.nope");
    }
}
