use anyhow::{Context, Result, anyhow};
use chrono::{Datelike, NaiveDate, Utc};
use clap::ArgAction;
use clap::{Parser, ValueEnum};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use reqwest::{Client, Url};
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::EnvFilter;

#[derive(ValueEnum, Clone, Debug)]
enum DatePattern {
    Month,
    Week,
}

#[derive(Parser, Debug)]
#[command(
    name = "es-retention",
    version,
    about = "Delete old indices by name (monthly or weekly patterns)",
    after_help = "Examples:\n  es-delete-old-indices --url http://localhost:9200 --index-prefix zis-audit- --older-than 25m\n  es-delete-old-indices --url http://localhost:9200 --index-prefix kafka-zis-external-orders-notify- --date-pattern week --older-than 21m --no-dryrun\n"
)]
struct Args {
    #[arg(long = "url")]
    url: String,
    #[arg(long = "username")]
    username: Option<String>,
    #[arg(long = "password")]
    password: Option<String>,
    #[arg(long = "index-prefix", default_value = "zis-audit-")]
    index_prefix: String,
    #[arg(long = "older-than", default_value = "25m")]
    older_than: String,
    #[arg(long = "date-pattern", value_enum, default_value_t = DatePattern::Month)]
    date_pattern: DatePattern,
    #[arg(long = "no-dryrun", action = ArgAction::SetTrue)]
    no_dryrun: bool,
}

#[derive(Deserialize)]
struct CatIndex {
    index: String,
}

fn parse_months(s: &str) -> Result<i32> {
    let re = Regex::new(r"(?i)^\s*(\d+)\s*m(?:onths?)?\s*$")?;
    let caps = re
        .captures(s)
        .ok_or_else(|| anyhow!("Invalid months value: '{s}'. Try '25m'."))?;
    let n: i32 = caps[1].parse()?;
    if n < 0 {
        return Err(anyhow!("Months must be non-negative"));
    }
    Ok(n)
}

fn months_between(now: NaiveDate, then: NaiveDate) -> i32 {
    (now.year() - then.year()) * 12 + (now.month() as i32 - then.month() as i32)
}

fn build_date_regex(date_pattern: &DatePattern, index_prefix: &str) -> Result<Regex> {
    let re = match date_pattern {
        DatePattern::Month => format!(
            r#"^{}(\d{{4}})[\.-](\d{{2}})$"#,
            regex::escape(index_prefix)
        ),
        DatePattern::Week => format!(r#"^{}(\d{{4}})-(\d{{1,2}})$"#, regex::escape(index_prefix)),
    };
    Ok(Regex::new(&re)?)
}

fn age_months_from_parts(
    date_pattern: &DatePattern,
    year: i32,
    part: u32,
    now_first: NaiveDate,
) -> Result<i32> {
    let then_first = match date_pattern {
        DatePattern::Month => {
            if !(1..=12).contains(&part) {
                return Err(anyhow!("month out of range"));
            }
            NaiveDate::from_ymd_opt(year, part, 1).ok_or_else(|| anyhow!("invalid month"))?
        }
        DatePattern::Week => {
            if !(1..=53).contains(&part) {
                return Err(anyhow!("week out of range"));
            }
            let then = NaiveDate::from_isoywd_opt(year, part, chrono::Weekday::Mon)
                .ok_or_else(|| anyhow!("invalid ISO week"))?;
            NaiveDate::from_ymd_opt(then.year(), then.month(), 1).unwrap()
        }
    };
    Ok(months_between(now_first, then_first))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();
    trace!("parsed args: {:?}", args);

    if (args.username.is_some()) ^ (args.password.is_some()) {
        return Err(anyhow!(
            "Both --username and --password must be provided for basic auth."
        ));
    }

    let months_cutoff = parse_months(&args.older_than)
        .with_context(|| format!("Failed to parse --older-than='{}'", args.older_than))?;
    info!("Cutoff: indices older than or equal to {months_cutoff} months will be deleted.");

    let base = Url::parse(&args.url).context("Invalid --url")?;
    debug!("Base URL: {base}");

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    let client = Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()?;

    // Build /_cat/indices/<prefix>*?format=json&h=index
    let mut cat_url = base.clone();
    {
        let path = format!(
            "{}/{}*",
            "_cat/indices",
            utf8_percent_encode(&args.index_prefix, NON_ALPHANUMERIC)
        );
        cat_url.set_path(&path);
        cat_url
            .query_pairs_mut()
            .append_pair("format", "json")
            .append_pair("h", "index");
    }
    debug!("CAT URL: {}", cat_url);

    let req = client.get(cat_url);
    let req = if let (Some(u), Some(p)) = (args.username.as_ref(), args.password.as_ref()) {
        req.basic_auth(u, Some(p))
    } else {
        req
    };

    let res = req.send().await?;
    if !res.status().is_success() {
        return Err(anyhow!(
            "CAT request failed: {}",
            res.text().await.unwrap_or_default()
        ));
    }

    let mut items: Vec<CatIndex> = res.json().await?;
    info!("Fetched {} index names", items.len());

    // NEW: sort načtených indexů podle YYYY-MM/YY.MM části (vzestupně = nejstarší první)
    items.sort_by(|a, b| {
        // normalizuj separátor na '-' kvůli lexikografii
        let na = a.index.replace('.', "-");
        let nb = b.index.replace('.', "-");
        // vytáhni část za prefixem (pokud to nejde, vezmi celý)
        let pa = na.strip_prefix(&args.index_prefix).unwrap_or(&na);
        let pb = nb.strip_prefix(&args.index_prefix).unwrap_or(&nb);
        pa.cmp(pb)
    });
    debug!("Sorted {} index names by YYYY-MM", items.len());

    let re = build_date_regex(&args.date_pattern, &args.index_prefix)?;

    let now = Utc::now().date_naive();
    let now_first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap();

    let mut targets: Vec<(String, i32)> = Vec::new(); // (index, age_months)
    for it in items {
        if let Some(caps) = re.captures(&it.index) {
            let y: i32 = match caps[1].parse() {
                Ok(v) => v,
                Err(e) => {
                    warn!("Skip {}: bad year: {e}", it.index);
                    continue;
                }
            };
            let part: u32 = match caps[2].parse() {
                Ok(v) => v,
                Err(e) => {
                    warn!("Skip {}: bad date part: {e}", it.index);
                    continue;
                }
            };
            let age_months = match age_months_from_parts(&args.date_pattern, y, part, now_first) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Skip {}: {}", it.index, e);
                    continue;
                }
            };
            debug!("Index {} -> age {} months", it.index, age_months);

            if age_months >= months_cutoff {
                targets.push((it.index, age_months));
            }
        } else {
            trace!("Index name did not match pattern, skipping: {}", it.index);
        }
    }

    // NEW: seřadit kandidáty k mazání od nejstarších (největší age) po nejmladší
    targets.sort_by(|a, b| a.1.cmp(&b.1)); // vzestupně dle age (nejstarší = nejvyšší age; pokud chceš opačně, použij b.1.cmp(&a.1))

    if targets.is_empty() {
        info!("Nothing to delete (0 indices match threshold).");
        return Ok(());
    }

    let dryrun = !args.no_dryrun; // default true (dry-run), pokud uživatel zadá --no-dryrun => false

    if dryrun {
        info!(
            "Dryrun: would delete {} indices (oldest first):",
            targets.len()
        );
        for (t, age) in &targets {
            info!("{t}  (age={}m)", age);
        }
        return Ok(());
    }

    info!("Live: Deleting {} indices (oldest first)…", targets.len());
    for (idx, _age) in targets {
        let mut del_url = base.clone();
        let path = utf8_percent_encode(&idx, NON_ALPHANUMERIC).to_string();
        del_url.set_path(&path);

        let mut req = client.delete(del_url);
        if let (Some(u), Some(p)) = (args.username.as_ref(), args.password.as_ref()) {
            req = req.basic_auth(u, Some(p));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            info!("DELETE {} -> {}", idx, status);
        } else {
            error!("DELETE {} failed: {} | {}", idx, status, body);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_months_accepts_values() {
        assert_eq!(parse_months("25m").unwrap(), 25);
        assert_eq!(parse_months(" 12 months ").unwrap(), 12);
        assert_eq!(parse_months("0m").unwrap(), 0);
    }

    #[test]
    fn parse_months_rejects_invalid() {
        assert!(parse_months("abc").is_err());
        assert!(parse_months("-1m").is_err());
    }

    #[test]
    fn months_between_counts_months() {
        let now = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        let then = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        assert_eq!(months_between(now, then), 2);
    }

    #[test]
    fn build_date_regex_month_matches() {
        let re = build_date_regex(&DatePattern::Month, "foo-").unwrap();
        assert!(re.is_match("foo-2025-03"));
        assert!(re.is_match("foo-2025.03"));
        assert!(!re.is_match("foo-2025-3"));
    }

    #[test]
    fn build_date_regex_week_matches() {
        let re = build_date_regex(&DatePattern::Week, "foo-").unwrap();
        assert!(re.is_match("foo-2025-1"));
        assert!(re.is_match("foo-2025-12"));
        assert!(!re.is_match("foo-2025-123"));
    }

    #[test]
    fn age_months_from_parts_month() {
        let now_first = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        let age = age_months_from_parts(&DatePattern::Month, 2025, 1, now_first).unwrap();
        assert_eq!(age, 2);
    }

    #[test]
    fn age_months_from_parts_week() {
        let now_first = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        let age = age_months_from_parts(&DatePattern::Week, 2025, 1, now_first).unwrap();
        assert_eq!(age, 3);
    }

    #[test]
    fn age_months_from_parts_week_out_of_range() {
        let now_first = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap();
        assert!(age_months_from_parts(&DatePattern::Week, 2025, 0, now_first).is_err());
        assert!(age_months_from_parts(&DatePattern::Week, 2025, 54, now_first).is_err());
    }
}
