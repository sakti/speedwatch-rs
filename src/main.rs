use std::{
    str::FromStr,
    thread,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use cfspeedtest::{
    OutputFormat,
    speedtest::{run_latency_test, test_download},
};
use clap::Parser;
use miette::{IntoDiagnostic, Result, miette};
use prometheus_remote_write::{LABEL_NAME, Label, Sample, TimeSeries, WriteRequest};
use reqwest::blocking::Client;
use tracing::{debug, info};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(Parser, Debug)]
#[command(
    version,
    about,
    long_about = "speedwatch will monitor your internet speed and latency and push it to prometheus remote write"
)]
struct Args {
    /// Interval in minutes
    #[arg(short, long, default_value_t = 30)]
    interval: u64,

    /// Remote write URL
    #[arg(short, long, env = "SW_REMOTE_WRITE_URL", default_value_t = String::from("http://localhost:9090/api/v1/write"))]
    remote_write_url: String,

    /// Remote write username
    #[arg(short, long, env = "SW_REMOTE_WRITE_USERNAME")]
    username_remote_write: String,

    /// Remote write password
    #[arg(short, long, env = "SW_REMOTE_WRITE_PASSWORD")]
    password_remote_write: String,
}

fn collect_and_push(args: &Args) -> Result<()> {
    let hostname = hostname::get().into_diagnostic()?;

    let download_speed = test_download(
        &reqwest::blocking::Client::new(),
        10_000_000,
        OutputFormat::None, // don't write to stdout while running the test
    );

    let (_, avg_latency) = run_latency_test(
        &reqwest::blocking::Client::new(),
        25,
        OutputFormat::None, // don't write to stdout while running the test
    );

    // build write requests
    let time: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .into_diagnostic()?
        .as_millis()
        .try_into()
        .into_diagnostic()?;

    let mut bw_labels: Vec<Label> = vec![];
    bw_labels.push(Label {
        name: "hostname".to_string(),
        value: hostname.to_string_lossy().into_owned(),
    });
    bw_labels.push(Label {
        name: LABEL_NAME.to_string(),
        value: "sw_internet_bandwidth_mbit".to_string(),
    });
    let bw_timeseries = TimeSeries {
        labels: bw_labels,
        samples: vec![Sample {
            value: download_speed,
            timestamp: time,
        }],
    };

    let mut latency_labels: Vec<Label> = vec![];
    latency_labels.push(Label {
        name: "hostname".to_string(),
        value: hostname.to_string_lossy().into_owned(),
    });
    latency_labels.push(Label {
        name: LABEL_NAME.to_string(),
        value: "sw_internet_latency_ms".to_string(),
    });
    let latency_timeseries = TimeSeries {
        labels: latency_labels,
        samples: vec![Sample {
            value: avg_latency,
            timestamp: time,
        }],
    };

    let write_request = WriteRequest {
        timeseries: vec![bw_timeseries, latency_timeseries],
    };

    let mut req = write_request
        .build_http_request(
            &args
                .remote_write_url
                .parse::<url::Url>()
                .into_diagnostic()?,
            USER_AGENT,
        )
        .map_err(|err| miette!("operation failed: {}", err))?;

    let credentials = STANDARD.encode(format!(
        "{}:{}",
        args.username_remote_write, args.password_remote_write
    ));
    req.headers_mut().insert(
        "Authorization",
        format!("Basic {}", credentials).parse().unwrap(),
    );

    // send the http::Request
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .into_diagnostic()?;

    let (parts, body) = req.into_parts();
    let method = reqwest::Method::from_str(parts.method.as_str()).into_diagnostic()?;
    let mut req_builder = client.request(method, parts.uri.to_string());
    for (name, value) in parts.headers.iter() {
        req_builder = req_builder.header(name.to_string(), value.as_bytes());
    }
    req_builder = req_builder.body(body);
    let response = req_builder.send().into_diagnostic()?;

    info!("time: {}", time);
    info!("download speed in mbit: {download_speed}");
    info!("average latency in ms: {avg_latency}");
    debug!("response status: {}", response.status());
    Ok(())
}

pub fn execute_at_interval<F>(mut task: F, interval_minutes: u64) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    let interval = Duration::from_secs(interval_minutes * 60);

    loop {
        let start = Instant::now();

        // Execute the task
        task()?;

        // Sleep for remaining time to maintain precise interval
        let elapsed = start.elapsed();
        if elapsed < interval {
            thread::sleep(interval - elapsed);
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "speedwatch_rs=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    let interval = args.interval;

    info!("Starting speedwatch, with interval: {} minutes", interval);

    execute_at_interval(
        || {
            return collect_and_push(&args);
        },
        interval,
    )?;
    Ok(())
}
