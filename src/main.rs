use reqwest::Client;
use std::fmt::Write;
use std::fmt;
use std::fs;
use serde_derive::{Serialize, Deserialize};
use std::path::{Path, PathBuf};
use std::error::Error;
use std::marker::PhantomData;
use chrono::DateTime;

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
            eprintln!("fetching {}, modified", self.url);
            let etag = resp.headers().get("ETag");
            let last_modified = resp.headers().get("Last-Modified");
            let link = resp.headers().get("Link");
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

fn main() -> Result<()> {
    let client = Client::new();
    let gh = Github::new(&client);
    let prs: Vec<PullRequest> = gh.request_seq("https://api.github.com/repos/rust-lang/rust/pulls?state=all&per_page=100")?;

    eprintln!("fetched {} PRs", prs.len());

    let mut f = String::new();
    //let mut last = Vec::new();
    let mut skipped = 0;
    let mut points = Vec::new();
    for pr in &prs[0..6000] {
        let comments: Vec<Comment> = gh.request_seq(&pr.comments_url)?;
        let merged_at = if let Some(m) = pr.merged_at { m } else { continue };
        if let Some(approval) = comments.iter().rfind(|c| {
            c.body.contains("bors r+") || c.body.contains("bors r=")
        }) {
            let pr_date = pr.created_at;
            //let create_to_approve = approval.created_at - pr.created_at;
            let approve_to_merge = merged_at - approval.created_at;
            let date = pr_date.date().naive_local().format("%Y-%m-%d").to_string();
            //last.push(approve_to_merge.num_hours());
            //if last.len() > 10 {
            //    last.remove(0);
            //}
            //let avg = last.iter().sum::<i64>() as usize / last.len();
            let point = approve_to_merge.num_hours();
            points.push(point);
            writeln!(f, r#"{},{}"#, date, point)?;
        } else {
            skipped += 1;
        }
    }

    points.sort_unstable();
    eprintln!("99th percentile: {}", *percentile(&points, 99) as f64 / 24.0);

    eprintln!("skipped {} PRs merged w/o approval", skipped);
    fs::write("out.csv", f.as_bytes())?;

    Ok(())
}

fn percentile<T>(v: &[T], p: usize) -> &T {
    let idx = v.len() * p / 100;
    &v[idx]
}
