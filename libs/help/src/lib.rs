#![no_std]
extern crate alloc;
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Article {
    pub id: String,
    pub locale: String,
    pub title: String,
    pub category: String,
    pub path: String,
    pub text: String,
    pub headings: Vec<(String, String)>,
}
pub fn read_catalog(bytes: &[u8]) -> Result<Vec<Article>, serde_json::Error> {
    serde_json::from_slice(bytes)
}
pub fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter_map(|c| {
            Some(match c {
                'à' | 'â' | 'ä' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'î' | 'ï' => 'i',
                'ô' | 'ö' => 'o',
                'ù' | 'û' | 'ü' => 'u',
                'ç' => 'c',
                '\u{300}'..='\u{36f}' => return None,
                other => other,
            })
        })
        .collect()
}
pub fn search(articles: &[Article], locale: &str, query: &str) -> Vec<usize> {
    let q = normalize(query);
    let terms: Vec<_> = q.split_whitespace().collect();
    let mut matches: Vec<_> = articles
        .iter()
        .enumerate()
        .filter(|(_, a)| a.locale == locale)
        .filter_map(|(i, a)| {
            let title = normalize(&a.title);
            let text = normalize(&a.text);
            terms
                .iter()
                .all(|t| title.contains(t) || text.contains(t))
                .then_some((i, terms.iter().filter(|t| title.contains(**t)).count()))
        })
        .collect();
    matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| articles[a.0].title.cmp(&articles[b.0].title)));
    matches.into_iter().map(|(i, _)| i).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    fn article(title: &str, text: &str, locale: &str) -> Article {
        Article {
            id: title.to_string(),
            locale: locale.to_string(),
            title: title.to_string(),
            category: String::new(),
            path: String::new(),
            text: text.to_string(),
            headings: Vec::new(),
        }
    }
    #[test]
    fn french_search_ranks_titles_and_filters_language() {
        let a = alloc::vec![
            article("Aide", "réseau", "fr-CA"),
            article("Réseau", "connexion", "fr-CA"),
            article("Network", "reseau", "en")
        ];
        assert_eq!(search(&a, "fr-CA", "reseau"), alloc::vec![1, 0]);
        assert_eq!(normalize("re\u{301}seau"), "reseau");
        assert!(search(&a, "fr-CA", "impossible").is_empty());
    }
    #[test]
    fn invalid_catalog_is_reported() {
        assert!(read_catalog(b"bad").is_err());
    }
}
