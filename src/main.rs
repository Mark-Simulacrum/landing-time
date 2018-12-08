use reqwest::Client;
use std::fmt::Write;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use serde_derive::{Serialize, Deserialize};
use std::path::{Path, PathBuf};
use std::error::Error;
use std::marker::PhantomData;
use chrono::{Datelike, NaiveDate, DateTime};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PullRequest {
    title: String,
    number: u32,
    comments_url: String,
    created_at: DateTime<chrono::FixedOffset>,
    merged_at: Option<DateTime<chrono::FixedOffset>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Comment {
    user: User,
    created_at: DateTime<chrono::FixedOffset>,
    body: String,
}

struct Github<'a> {
    client: &'a Client,
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Request<T> {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    data: String,
    _data: PhantomData<T>,
    link: Option<String>,
}

fn cache_path(cache: &Path, url: &str) -> PathBuf {
    let hex = crypto_hash::hex_digest(crypto_hash::Algorithm::SHA256, url.as_bytes());
    cache.join(hex)
}

impl Request<()> {
    fn query_url<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        client: &Client,
        cached: Option<T>,
    ) -> Result<Request<T>> {
        let mut query = client.get(&self.url)
            .basic_auth("Mark-Simulacrum", Some(dotenv::var("GH_OAUTH_TOKEN").unwrap()));
        query = query.header(reqwest::header::USER_AGENT, "Mark-Simulacrum");
        if let Some(etag) = &self.etag {
            if etag.starts_with("W/") {
                query = query.header("If-None-Match", &etag.as_str()[2..]);
            } else {
                query = query.header("If-None-Match", etag.as_str());
            }
        }
        if let Some(since) = &self.last_modified {
            query = query.header("If-Modified-Since", since.as_str());
        }
        let mut resp = query.send()?;
        if resp.status() != reqwest::StatusCode::NOT_MODIFIED {
            let etag = resp.headers().get("ETag");
            let last_modified = resp.headers().get("Last-Modified");
            let link = resp.headers().get("Link");
            eprintln!("fetching {}, modified, {:?} != {:?}", self.url, self.etag, etag);
            let r = Request {
                url: self.url.clone(),
                etag: etag.and_then(|v| v.to_str().ok()).map(|v| v.to_string()),
                last_modified: last_modified.and_then(|v| v.to_str().ok()).map(|v| v.to_string()),
                link: link.and_then(|v| v.to_str().ok()).map(|v| v.to_string()),
                data: resp.text()?,
                _data: PhantomData,
            };
            return Ok(r);
        }

        Ok(Request {
            url: self.url.clone(),
            etag: self.etag.clone(),
            last_modified: self.last_modified.clone(),
            link: self.link.clone(),
            data: serde_json::to_string(&cached.expect("must have a cached value")).unwrap(),
            _data: PhantomData,
        })
    }
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> Request<T> {
    fn split(self) -> (Request<()>, T) {
        let data = self.data;
        let url = self.url.clone();
        (Request {
            url: self.url,
            etag: self.etag,
            last_modified: self.last_modified,
            link: self.link,
            data: String::new(),
            _data: PhantomData,
        }, serde_json::from_str(&data).unwrap_or_else(|e| {
            panic!("deserialize {:?} (:?) failed: {:?} {:?}", url, data, e);
        }))
    }

    fn from_url(client: &Client, url: String, cache: &Path) -> Result<Request<T>> {
        let path = cache_path(cache, &url);
        let r = if path.exists() {
            let file = fs::read_to_string(&path)?;
            let r: Request<T> = serde_json::from_str(&file)?;
            if std::env::var_os("REFRESH").is_some() {
                let r = r.split();
                r.0.query_url(client, Some(r.1))?
            } else {
                r
            }
        } else {
            let r = Request {
                url: url,
                etag: None,
                last_modified: None,
                link: None,
                data: String::new(),
                _data: PhantomData,
            };
            r.query_url(client, None)?
        };
        std::fs::write(&path, serde_json::to_string(&r).unwrap()).unwrap();
        Ok(r)
    }

    fn next(&self, gh: &Github) -> Result<Option<Request<T>>> {
        if let Some(link) = &self.link {
            let links = link.parse::<hyperx::header::Link>().unwrap();
            let next = links.values()
                .iter().find(|v| {
                    v.rel().unwrap_or(&[]).contains(&hyperx::header::RelationType::Next)
                });
            let next = if let Some(n) = next { n } else { return Ok(None); };
            Ok(Some(Request::from_url(gh.client, next.link().to_owned(), &gh.dir)?))
        } else {
            Ok(None)
        }
    }

    fn data(&self) -> T {
        serde_json::from_str(&self.data).unwrap_or_else(|e| {
            panic!("deserialize {:?} (:?) failed: {:?} {:?}", self.url, self.data, e);
        })
    }
}

impl<'a> Github<'a> {
    fn new(c: &'a Client) -> Self {
        Github {
            client: c,
            dir: PathBuf::from("cache"),
        }
    }

    #[allow(unused)]
    fn request<T: serde::Serialize + serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        return Ok(Request::from_url(&self.client, url.to_string(), &self.dir)?.data());
    }

    fn request_seq<T: serde::Serialize + serde::de::DeserializeOwned>(&self, url: &str) -> Result<Vec<T>> {
        let req = Request::<Vec<T>>::from_url(&self.client, url.to_string(), &self.dir)?;
        let mut data: Vec<T> = Vec::new();
        let mut next_request = req.next(self)?;
        data.extend(req.data());
        while let Some(next) = next_request {
            next_request = next.next(self)?;
            data.extend(next.data());
        }
        Ok(data)
    }
}

#[allow(unused)]
struct LargeDuration(chrono::Duration);

impl fmt::Display for LargeDuration {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}d {}h", self.0.num_days(), self.0.num_hours())
    }
}

#[derive(Copy, Clone, Debug)]
enum State {
    /// beginning
    Proposed,
    /// r+
    Approved,
    /// "resolve the merge conflicts"
    MergeConflict,
    /// r-
    Denied,
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone)]
struct Datum {
    date: chrono::NaiveDate,
    // in hours
    merge_time: i64,
}

fn main() -> Result<()> {
    let client = Client::new();
    let gh = Github::new(&client);
    let prs: Vec<PullRequest> = gh.request_seq("https://api.github.com/repos/rust-lang/rust/pulls?state=all&per_page=100")?;

    eprintln!("fetched {} PRs", prs.len());

    let mut skipped = 0;
    let mut points = Vec::new();
    let mut data = Vec::new();
    for pr in &prs[0..6000] {
        let comments: Vec<Comment> = gh.request_seq(&pr.comments_url)?;
        let merged_at = if let Some(m) = pr.merged_at { m } else { continue };
        let mut pr_state = State::Proposed;
        let mut final_approval = None;
        for c in &comments {
            let prev = pr_state;
            if c.body.contains("bors r+") || c.body.contains("bors r=") {
                pr_state = State::Approved;
            } else if c.body.contains("resolve the merge conflicts") && c.user.login == "bors" {
                pr_state = State::MergeConflict;
            } else if c.body.contains("bors r-") {
                pr_state = State::Denied;
            }
            match (prev, pr_state) {
                (State::Proposed, State::Approved) |
                (State::Denied, State::Approved) => {
                    final_approval = Some(c.created_at);
                }
                // Can merge conflict before first approval
                (State::MergeConflict, State::Approved) => if final_approval.is_none() {
                    final_approval = Some(c.created_at);
                }
                // Warn about cases where we're approving but haven't yet
                // recorded a final approval
                (_, State::Approved) if final_approval.is_none() => {
                    eprintln!("{:?} => approval in {:?}", prev, pr.number);
                }
                _ => {}
            }
        }
        if let Some(approval) = final_approval {
            let pr_date = pr.created_at;
            let approve_to_merge = merged_at - approval;
            let point = approve_to_merge.num_hours();
            points.push(point);
            data.push(Datum {
                date: pr_date.date().naive_local(),
                merge_time: approve_to_merge.num_hours(),
            });
        } else {
            skipped += 1;
        }
    }

    points.sort_unstable();
    eprintln!("99th percentile: {}", *percentile(&points, 99) as f64 / 24.0);

    eprintln!("skipped {} PRs merged w/o approval", skipped);

    let mut f = String::new();
    let mut by_date = BTreeMap::new();
    for datum in &data {
        let week = datum.date.iso_week().week();
        // 2-week intervals
        let week = week + week % 2;
        let date = NaiveDate::from_isoywd_opt(
            datum.date.year(),
            week,
            chrono::Weekday::Sat,
        ).unwrap_or_else(|| {
            panic!("could not handle week {}", week);
        });
        by_date.entry(date).or_insert_with(Vec::new).push(datum);
    }
    for datums in by_date.values_mut() {
        datums.sort_unstable_by_key(|d| d.merge_time);
    }
    for (date, datums) in &by_date {
        let p85 = percentile(&datums, 85);
        let p90 = percentile(&datums, 90);
        let p95 = percentile(&datums, 95);
        let p98 = percentile(&datums, 98);
        let p99 = percentile(&datums, 99);
        writeln!(
            f,
            r#"{},{},{},{},{},{},{},{}"#,
            date.format("%Y-%m-%d"),
            p85.merge_time,
            p90.merge_time,
            p95.merge_time,
            p98.merge_time,
            p99.merge_time,
            datums.iter().map(|d| d.merge_time).sum::<i64>() as usize / datums.len(),
            datums.iter().map(|d| d.merge_time).max().unwrap(),
        )?;

    }
    fs::write("out.csv", f.as_bytes())?;

    Ok(())
}

fn percentile<T>(v: &[T], p: usize) -> &T {
    let idx = v.len() as f64 * p as f64 / 100.0;
    &v[idx.floor() as usize]
}
