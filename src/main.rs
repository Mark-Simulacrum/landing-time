use chrono::{DateTime, Datelike, NaiveDate};
use reqwest::Client;
use serde_derive::{Deserialize, Serialize};
use std::collections::{HashMap, BTreeMap};
use std::error::Error;
use std::fmt;
use std::fmt::Write;
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

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

struct CacheClient<'a> {
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
        let mut query = if self.url.contains("api.github.com") {
            client.get(&self.url).basic_auth(
                "Mark-Simulacrum",
                Some(dotenv::var("GH_OAUTH_TOKEN").unwrap()),
            )
        } else if self.url.contains("ci.appveyor.com/api") {
            client.get(&self.url).header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", dotenv::var("APPVEYOR_OAUTH_TOKEN").unwrap()),
            )
        } else if self.url.contains("api.travis-ci.org") {
            client.get(&self.url)
                .header("Travis-API-Version", "3")
                .header(reqwest::header::AUTHORIZATION,
                    format!("token {}", dotenv::var("TRAVIS_TOKEN").unwrap())
                )
        } else {
            panic!("unknown API provider: {}", self.url);
        };
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
        query = query.header("Content-Type", "application/json");
        let mut resp = query.send()?;
        if resp.status() != reqwest::StatusCode::NOT_MODIFIED {
            let etag = resp.headers().get("ETag");
            let last_modified = resp.headers().get("Last-Modified");
            let link = resp.headers().get("Link");
            eprintln!(
                "fetching {}, modified, {:?} != {:?}",
                self.url, self.etag, etag
            );
            let r = Request {
                url: self.url.clone(),
                etag: etag.and_then(|v| v.to_str().ok()).map(|v| v.to_string()),
                last_modified: last_modified
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string()),
                link: link.and_then(|v| v.to_str().ok()).map(|v| v.to_string()),
                data: resp.text()?,
                _data: PhantomData,
            };
            return Ok(r);
        }
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

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
        (
            Request {
                url: self.url,
                etag: self.etag,
                last_modified: self.last_modified,
                link: self.link,
                data: String::new(),
                _data: PhantomData,
            },
            serde_json::from_str(&data).unwrap_or_else(|e| {
                panic!("deserialize {:?} ({:?}) failed: {:?}", url, data, e);
            }),
        )
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

    fn next(&self, gh: &CacheClient) -> Result<Option<Request<T>>> {
        if let Some(link) = &self.link {
            let links = link.parse::<hyperx::header::Link>().unwrap();
            let next = links.values().iter().find(|v| {
                v.rel()
                    .unwrap_or(&[])
                    .contains(&hyperx::header::RelationType::Next)
            });
            let next = if let Some(n) = next {
                n
            } else {
                return Ok(None);
            };
            Ok(Some(Request::from_url(
                gh.client,
                next.link().to_owned(),
                &gh.dir,
            )?))
        } else {
            Ok(None)
        }
    }

    fn data(&self) -> T {
        serde_json::from_str(&self.data).unwrap_or_else(|e| {
            panic!("deserialize {:?} ({:?}) failed: {:?}", self.url, self.data, e);
        })
    }
}

impl<'a> CacheClient<'a> {
    fn new(c: &'a Client) -> Self {
        CacheClient {
            client: c,
            dir: PathBuf::from("cache"),
        }
    }

    #[allow(unused)]
    fn request<T: serde::Serialize + serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        return Ok(Request::from_url(&self.client, url.to_string(), &self.dir)?.data());
    }

    fn request_seq<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Vec<T>> {
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

#[derive(Deserialize, Serialize, Debug, Clone)]
struct AppVeyorJob {
    name: String,
    started: Option<DateTime<chrono::FixedOffset>>,
    finished: Option<DateTime<chrono::FixedOffset>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct AppVeyorBuild {
    #[serde(rename = "buildId")]
    id: u64,
    jobs: Vec<AppVeyorJob>,
    version: String,
    started: DateTime<chrono::FixedOffset>,
    finished: DateTime<chrono::FixedOffset>,
}

fn fetch_appveyor(client: &CacheClient) -> Result<(HashMap<u64, AppVeyorBuild>, HashMap<String, u64>)> {
    let mut map = HashMap::new();
    let mut id_map = HashMap::new();

    let url = "https://ci.appveyor.com/api/projects/rust-lang/rust/history?recordsNumber=100";

    let mut last_build_id = None;
    'outer: loop {
        let reply: serde_json::Value = match last_build_id {
            Some(b) => client.request(&format!("{}&startBuildId={}", url, b))?,
            None => client.request(url)?,
        };

        if reply["builds"].as_array().unwrap().is_empty() {
            break;
        }

        for build in reply["builds"].as_array().unwrap() {
            // Skip not started and/or not finished builds
            if !(build["started"].is_string() && build["finished"].is_string()) {
                continue;
            }
            let version = build["version"].as_str().unwrap();

            let b: serde_json::Value = client.request(
                &format!("https://ci.appveyor.com/api/projects/rust-lang/rust/build/{}", version))?;
            let b: AppVeyorBuild = serde_json::from_value(b["build"].clone()).unwrap_or_else(|e| {
                panic!("could not deserialize AppVeyorBuild from {:?}: {:?}", b["build"], e);
            });
            last_build_id = Some(b.id);
            id_map.insert(version.to_string(), b.id);
            map.insert(b.id, b);
        }
    }

    Ok((map, id_map))
}

#[derive(Serialize, Deserialize, Clone)]
struct TravisBuild {
    id: u64,
    started_at: DateTime<chrono::FixedOffset>,
    finished_at: DateTime<chrono::FixedOffset>,
}

#[derive(Debug, Copy, Clone)]
struct Datum {
    date: chrono::NaiveDate,
    merge_delay: i64,
    merge_time: Option<chrono::Duration>,
    appveyor_time: Option<chrono::Duration>,
    travis_time: Option<chrono::Duration>,
}

fn main() -> Result<()> {
    let client = Client::new();
    let gh = CacheClient::new(&client);
    let appveyor_re = regex::Regex::new(r#"\[status-appveyor\]\(.*?project/rust-lang/rust/builds?/(.+?)\)"#)?;
    let travis_re = regex::Regex::new(r#"\[status-travis\]\(.*?builds/(\d+).*?\)"#)?;
    let prs: Vec<PullRequest> =
        gh.request_seq("https://api.github.com/repos/rust-lang/rust/pulls?state=all&per_page=100")?;

    eprintln!("fetched {} PRs", prs.len());

    let appveyor_builds = fetch_appveyor(&gh)?;

    let mut skipped = 0;
    let mut data = Vec::new();
    for pr in &prs[0..8_000] {
        if pr.created_at.date().naive_local().year() != 2018 {
            break;
        }
        let comments: Vec<Comment> = gh.request_seq(&pr.comments_url)?;
        let merged_at = if let Some(m) = pr.merged_at {
            m
        } else {
            continue;
        };
        let mut pr_state = State::Proposed;
        let mut final_approval = None;
        let mut last_test = None;
        let mut merge_at = None;
        let mut appveyor_build = None;
        let mut travis_build: Option<TravisBuild> = None;
        for c in &comments {
            let prev = pr_state;
            if c.body.contains("bors r+") || c.body.contains("bors r=") {
                pr_state = State::Approved;
            } else if c.body.contains("resolve the merge conflicts") && c.user.login == "bors" {
                pr_state = State::MergeConflict;
            } else if c.body.contains("bors r-") {
                pr_state = State::Denied;
            }
            if c.body.contains("Testing commit") && c.user.login == "bors" {
                last_test = Some(c.created_at);
            }
            if c.user.login == "bors" && c.body.contains("Test successful") &&
                c.body.contains("Pushing") {
                if let Some(appveyor) = appveyor_re.captures(&c.body) {
                    appveyor_build = Some(appveyor[1].parse::<u64>().unwrap_or_else(|e| {
                        if appveyor[1].starts_with("1.0.") {
                            *appveyor_builds.1.get(&appveyor[1]).unwrap_or_else(|| {
                                panic!("could not retrieve build ID for {}", &appveyor[1])
                            })
                        } else {
                            panic!("could not parse build ID from {:?}: {:?}", &appveyor[1], e);
                        }
                    }));
                } else {
                    panic!("could not find AppVeyor build in {:?}", c.body);
                }
                if let Some(travis) = travis_re.captures(&c.body) {
                    let id = travis[1].parse::<u64>().unwrap_or_else(|e| {
                        panic!("could not parse build ID from {:?}: {:?}", &travis[1], e);
                    });
                    let build: TravisBuild =
                        gh.request(&format!("https://api.travis-ci.org/build/{}", id))?;
                    travis_build = Some(build);
                } else {
                    panic!("could not find Travis build in {:?}", c.body);
                }
                merge_at = Some(c.created_at);
            }
            match (prev, pr_state) {
                (State::Proposed, State::Approved) | (State::Denied, State::Approved) => {
                    final_approval = Some(c.created_at);
                }
                // Can merge conflict before first approval
                (State::MergeConflict, State::Approved) => {
                    if final_approval.is_none() {
                        final_approval = Some(c.created_at);
                    }
                }
                // Warn about cases where we're approving but haven't yet
                // recorded a final approval
                (_, State::Approved) if final_approval.is_none() => {
                    eprintln!("{:?} => approval in {:?}", prev, pr.number);
                }
                _ => {}
            }
        }
        if last_test.is_none() || merge_at.is_none() {
            last_test = None;
            merge_at = None;
        }
        if let Some(approval) = final_approval {
            let appveyor_build = if let Some(ab) = appveyor_build {
                Some(&appveyor_builds.0[&ab])
            } else {
                if merge_at.is_some() {
                    panic!("AppVeyor build ID unknown for {}, merged at {:?}", pr.number, merge_at);
                }
                None
            };
            let travis_build: Option<&TravisBuild> = if let Some(tb) = &travis_build {
                Some(tb)
            } else {
                if merge_at.is_some() {
                    panic!("Travis build unknown for {}, merged at {:?}", pr.number, merge_at);
                }
                None
            };
            let pr_date = pr.created_at;
            let approve_to_merge = merged_at - approval;
            let merge_time = last_test.map(|t| merge_at.unwrap() - t);
            data.push(Datum {
                date: pr_date.date().naive_local(),
                merge_delay: approve_to_merge.num_hours(),
                merge_time,
                appveyor_time: appveyor_build.map(|b| b.finished - b.started),
                travis_time: travis_build.map(|b| b.finished_at - b.started_at),
            });
        } else {
            skipped += 1;
        }
    }

    eprintln!("skipped {} PRs merged w/o approval", skipped);

    let mut f = String::new();
    let mut delay_by_date = BTreeMap::new();
    for datum in &data {
        let week = datum.date.iso_week().week();
        // 2-week intervals
        let week = week + week % 2;
        let date = NaiveDate::from_isoywd_opt(datum.date.year(), week, chrono::Weekday::Sat)
            .unwrap_or_else(|| {
                panic!("could not handle week {}", week);
            });
        delay_by_date.entry(date).or_insert_with(Vec::new).push(datum.merge_delay);
    }
    for datums in delay_by_date.values_mut() {
        datums.sort_unstable();
    }
    for (date, datums) in &delay_by_date {
        let p85 = percentile(&datums, 85);
        let p90 = percentile(&datums, 90);
        let p95 = percentile(&datums, 95);
        let p98 = percentile(&datums, 98);
        let p99 = percentile(&datums, 99);
        writeln!(
            f,
            r#"{},{},{},{},{},{},{},{}"#,
            date.format("%Y-%m-%d"),
            p85,
            p90,
            p95,
            p98,
            p99,
            datums.iter().sum::<i64>() as usize / datums.len(),
            datums.iter().max().unwrap(),
        )?;
    }
    fs::write("merge_delay.csv", f.as_bytes())?;

    let mut f = String::new();
    let mut merge_time_by_date = BTreeMap::new();
    for datum in &data {
        let week = datum.date.iso_week().week();
        let date = NaiveDate::from_isoywd_opt(datum.date.year(), week, chrono::Weekday::Wed)
            .unwrap_or_else(|| {
                panic!("could not handle week {}", week);
            });
        merge_time_by_date.entry(date).or_insert_with(Vec::new).push(datum);
    }
    for (date, datums) in &merge_time_by_date {
        if let Some(s) = write_merge_time(*date, &datums[..]) {
            writeln!(f, "{}", s)?;
        }
    }
    fs::write("merge_time.csv", f.as_bytes())?;

    Ok(())
}

fn write_merge_time(date: NaiveDate, datums: &[&Datum]) -> Option<String> {
    let percentile = 95;
    let overall = sort_percentile(
        datums.iter().flat_map(|d| d.merge_time).map(|d| d.num_minutes()), percentile)?;
    let travis = sort_percentile(
        datums.iter().flat_map(|d| d.travis_time).map(|d| d.num_minutes()), percentile)?;
    let appveyor = sort_percentile(
        datums.iter().flat_map(|d| d.appveyor_time).map(|d| d.num_minutes()), percentile)?;
    Some(format!(
        r#"{},{},{},{}"#,
        date.format("%Y-%m-%d"),
        overall,
        travis,
        appveyor,
    ))
}

fn sort_percentile<T: Ord>(v: impl Iterator<Item=T>, p: usize) -> Option<T> {
    let mut v = v.collect::<Vec<_>>();
    if v.is_empty() { return None }
    v.sort_unstable();
    let idx = v.len() as f64 * p as f64 / 100.0;
    Some(v.remove(idx.floor() as usize))
}

fn percentile<T>(v: &[T], p: usize) -> &T {
    let idx = v.len() as f64 * p as f64 / 100.0;
    &v[idx.floor() as usize]
}
