//! GitHub repository search helpers.

use std::env;

use crate::error::Result;
use crate::github::{get_current_org, search_repos, RepoSearchResult};

/// Environment variable containing the default GitHub organisations.
pub const ORGS_ENV_VAR: &str = "ENVOY_GITHUB_ORGS";

pub(crate) fn parse_orgs(raw: &str) -> Vec<String> {
    raw.replace(',', ";")
        .split(';')
        .map(str::trim)
        .filter(|org| !org.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn print_results(results: &[RepoSearchResult], query: &str, orgs: Option<&[String]>) {
    let scope = orgs
        .map(|orgs| orgs.join(", "))
        .filter(|scope| !scope.is_empty())
        .unwrap_or_else(|| String::from("GitHub (global)"));
    println!(
        "\nSearch: '{query}'  |  Scope: {scope}  |  {} result(s)\n",
        results.len()
    );

    for repo in results {
        println!("  {}", repo.full_name);
        if !repo.description.is_empty() {
            println!("    {}", repo.description);
        }
        let updated = repo.updated_at.get(..10).unwrap_or_default();
        println!("    {}  ★ {}  updated: {updated}", repo.url, repo.stars);
        println!();
    }
}

/// Search GitHub repositories and print formatted results.
pub fn run_search(query: &str, orgs: Option<&[String]>, limit: usize) -> Result<()> {
    let effective_orgs = if let Some(orgs) = orgs {
        Some(orgs.to_vec())
    } else if let Ok(raw) = env::var(ORGS_ENV_VAR) {
        let parsed = parse_orgs(&raw);
        if parsed.is_empty() {
            None
        } else {
            Some(parsed)
        }
    } else {
        get_current_org().map(|org| vec![org])
    };

    let results = search_repos(query, effective_orgs.as_deref(), limit)?;

    if results.is_empty() {
        let scope = effective_orgs
            .as_deref()
            .map(|orgs| orgs.join(", "))
            .filter(|scope| !scope.is_empty())
            .unwrap_or_else(|| String::from("GitHub (global)"));
        println!("No repositories found matching '{query}' in: {scope}");
        return Ok(());
    }

    print_results(&results, query, effective_orgs.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_orgs;

    #[test]
    fn parses_semicolon_and_comma_separated_orgs() {
        assert_eq!(
            parse_orgs("gtvfx-contrib, gtvfx ; gt"),
            vec![
                String::from("gtvfx-contrib"),
                String::from("gtvfx"),
                String::from("gt"),
            ]
        );
    }
}
