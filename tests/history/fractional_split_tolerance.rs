use httpmock::Method::GET;
use httpmock::MockServer;
use paft::market::action::Action;
use url::Url;
use yfinance_rs::core::Interval;
use yfinance_rs::{HistoryBuilder, YfClient};

/// Regression: Yahoo occasionally emits a fractional split numerator for
/// preferred-share adjustments (observed: AXIA-P returns 1.262838).
/// Upstream 0.7.2 aborted the whole HistoryResponse. This patch scales
/// both sides of the ratio by 1e6 when either is fractional, yielding
/// `(1_262_838, 1_000_000)` — same ratio, integer pair — so the
/// rational `Action::Split { numerator: u32, denominator: u32 }`
/// representation stays lossless and downstream consumers can still
/// reconstruct the 1.262838 adjustment factor.
#[tokio::test]
async fn fractional_split_scaled_to_rational_pair() {
    let server = MockServer::start();

    let body = r#"{
      "chart":{
        "result":[
          {
            "timestamp":[1000,2000],
            "indicators":{
              "quote":[{
                "open":[100.0,100.0],
                "high":[101.0,101.0],
                "low":[ 99.0, 99.0],
                "close":[100.0,100.0],
                "volume":[10,10]
              }],
              "adjclose":[{"adjclose":[100.0,100.0]}]
            },
            "events": {
              "splits": {
                "2000": { "date": 2000, "numerator": 1.262838, "denominator": 1 }
              }
            }
          }
        ],
        "error": null
      }
    }"#;

    let mock = server.mock(|when, then| {
        when.method(GET).path("/v8/finance/chart/TEST");
        then.status(200)
            .header("content-type", "application/json")
            .body(body);
    });

    let client = YfClient::builder()
        .base_chart(Url::parse(&format!("{}/v8/finance/chart/", server.base_url())).unwrap())
        .build()
        .unwrap();

    let resp = HistoryBuilder::new(&client, "TEST")
        .interval(Interval::D1)
        .auto_adjust(true)
        .fetch_full()
        .await
        .expect("fractional split must not abort deserialization");

    mock.assert();
    assert_eq!(resp.candles.len(), 2);

    let split = resp
        .actions
        .iter()
        .find_map(|a| match a {
            Action::Split {
                numerator,
                denominator,
                ..
            } => Some((*numerator, *denominator)),
            _ => None,
        })
        .expect("split action must be present");

    assert_eq!(split, (1_262_838, 1_000_000));
    let ratio = f64::from(split.0) / f64::from(split.1);
    assert!((ratio - 1.262838).abs() < 1e-9);
}
